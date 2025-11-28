use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use async_trait::async_trait;
use chrono::{Datelike, TimeDelta, Timelike, Utc, Weekday};
use color_eyre::eyre::{self, Result, WrapErr, bail};
use rand::seq::IndexedRandom as _;
use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::{Mutex, broadcast, mpsc::UnboundedReceiver},
    time::{Duration, sleep},
};
use tracing::{debug, error, info, instrument, trace};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::{RefreshingLoginCredentials, TokenStorage, UserAccessToken},
    message::{NoticeMessage, PrivmsgMessage, ServerMessage},
};

/// StreamElements API client and types for managing bot commands.
///
/// This module provides an HTTP client for interacting with the StreamElements API
/// to retrieve and update bot commands. Used primarily for managing ping commands
/// that notify users about community game sessions.
mod streamelements {
    use eyre::{Result, WrapErr as _};
    use reqwest::header::{self, HeaderValue};
    use serde::{Deserialize, Serialize};
    use tracing::instrument;

    use crate::APP_USER_AGENT;

    /// A StreamElements bot command with all its configuration.
    ///
    /// Commands can be triggered by users in chat and have various settings
    /// like cooldowns, access levels, and the reply text.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Command {
        pub cooldown: CommandCooldown,
        pub aliases: Vec<String>,
        pub keywords: Vec<String>,
        pub enabled: bool,
        pub enabled_online: bool,
        pub enabled_offline: bool,
        pub hidden: bool,
        pub cost: i64,
        #[serde(rename = "type")]
        pub command_type: String,
        pub access_level: i64,
        #[serde(rename = "_id")]
        pub id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub regex: Option<String>,
        pub reply: String,
        pub command: String,
        pub channel: String,
        pub created_at: String,
        pub updated_at: String,
    }

    /// Cooldown settings for a command.
    ///
    /// Defines how long users must wait between command uses.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct CommandCooldown {
        /// Per-user cooldown in seconds
        pub user: i64,
        /// Global cooldown in seconds (affects all users)
        pub global: i64,
    }

    /// Error response from the StreamElements API.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Error {
        status_code: i64,
        error: String,
        message: String,
        details: Vec<ErrorDetail>,
    }

    /// Detailed error information for a specific field.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ErrorDetail {
        path: Vec<String>,
        message: String,
    }

    /// HTTP client for the StreamElements API.
    ///
    /// Handles authentication and provides methods to interact with bot commands.
    #[derive(Debug)]
    pub struct SEClient(reqwest::Client);

    impl SEClient {
        /// Creates a new StreamElements API client with the given authentication token.
        ///
        /// # Errors
        ///
        /// Returns an error if the token format is invalid or the HTTP client cannot be built.
        #[instrument]
        pub fn new(token: &str) -> Result<Self> {
            let mut headers = header::HeaderMap::new();
            let mut auth_value = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
            auth_value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, auth_value);
            headers.insert(
                header::ACCEPT,
                HeaderValue::from_static("application/json; charset=utf-8"),
            );
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );

            let http = reqwest::Client::builder()
                .user_agent(APP_USER_AGENT)
                .default_headers(headers)
                .build()
                .wrap_err("Failed to build HTTP Client")?;

            Ok(Self(http))
        }

        /// Retrieves all bot commands for a given channel.
        ///
        /// # Errors
        ///
        /// Returns an error if the API request fails or the response cannot be parsed.
        #[instrument]
        pub async fn get_all_commands(&self, channel_id: &str) -> Result<Vec<Command>> {
            let commands = self
                .0
                .get(format!(
                    "https://api.streamelements.com/kappa/v2/bot/commands/{channel_id}"
                ))
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<Command>>()
                .await?;

            Ok(commands)
        }

        /// Updates an existing bot command.
        ///
        /// # Errors
        ///
        /// Returns an error if the API request fails or the response cannot be parsed.
        #[instrument]
        pub async fn update_command(&self, channel_id: &str, command: Command) -> Result<()> {
            self.0
                .put(format!(
                    "https://api.streamelements.com/kappa/v2/bot/commands/{channel_id}/{}",
                    command.id
                ))
                .json(&command)
                .send()
                .await?
                .error_for_status()?
                .json::<Command>()
                .await?;

            Ok(())
        }
    }
}

use crate::streamelements::SEClient;

/// Type alias for the authenticated Twitch IRC client
type AuthenticatedTwitchClient =
    TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>;

const TARGET_HOUR: u32 = 13;
const TARGET_MINUTE: u32 = 37;

/// Maximum number of unique users to track (prevents unbounded memory growth)
const MAX_USERS: usize = 10_000;

/// Expected Latency of the Twitch IRC Server
/// Will adjust all schedules by the latency in order to increase accuracy
const EXPECTED_LATENCY: u32 = 89;

/// Scheduled message configuration: (message, hours_between_posts)
const SCHEDULED_MESSAGES: &[(&str, u64)] = &[(
    "DinkDonk An alle Wichtel: Vergesst nicht eure Adresse im Textfeld einzutragen, falls ihr es noch nicht getan habt DinkDonk Wer keine Adresse einträgt, kriegt keine Geschenke DinkDonk",
    1,
)];

static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

static CHANNEL_LOGIN: LazyLock<String> = LazyLock::new(|| {
    std::env::var("TWITCH_CHANNEL").unwrap_or_else(|_| "REDACTED_CHANNEL".to_string())
});

static TWITCH_USERNAME: LazyLock<String> = LazyLock::new(|| {
    std::env::var("TWITCH_USERNAME").expect("TWITCH_USERNAME environment variable must be set")
});

static TWITCH_ACCESS_TOKEN: LazyLock<SecretString> = LazyLock::new(|| {
    let token = std::env::var("TWITCH_ACCESS_TOKEN")
        .expect("TWITCH_ACCESS_TOKEN environment variable must be set");
    SecretString::new(token.into())
});

static TWITCH_REFRESH_TOKEN: LazyLock<SecretString> = LazyLock::new(|| {
    let token = std::env::var("TWITCH_REFRESH_TOKEN")
        .expect("TWITCH_REFRESH_TOKEN environment variable must be set");
    SecretString::new(token.into())
});

static TWITCH_CLIENT_ID: LazyLock<String> = LazyLock::new(|| {
    std::env::var("TWITCH_CLIENT_ID").expect("TWITCH_CLIENT_ID environment variable must be set")
});

static TWITCH_CLIENT_SECRET: LazyLock<SecretString> = LazyLock::new(|| {
    let secret = std::env::var("TWITCH_CLIENT_SECRET")
        .expect("TWITCH_CLIENT_SECRET environment variable must be set");
    SecretString::new(secret.into())
});

static STREAMELEMENTS_API_TOKEN: LazyLock<SecretString> = LazyLock::new(|| {
    let secret = std::env::var("STREAMELEMENTS_API_TOKEN")
        .expect("STREAMELEMENTS_API_TOKEN environment variable must be set");
    SecretString::new(secret.into())
});

/// Calculates the next occurrence of a daily time in Europe/Berlin timezone.
///
/// If the specified time has already passed today, returns tomorrow's occurrence.
fn calculate_next_occurrence(hour: u32, minute: u32) -> chrono::DateTime<Utc> {
    let berlin_now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

    // Create target time today in Berlin timezone
    let mut target = berlin_now
        .date_naive()
        .and_hms_opt(hour, minute, 0)
        .expect("Invalid hour/minute for Berlin time")
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()
        .expect("Ambiguous time during DST transition");

    // If target time has already passed today, schedule for tomorrow
    if target <= berlin_now {
        target = (berlin_now + chrono::Duration::days(1))
            .date_naive()
            .and_hms_opt(hour, minute, 0)
            .expect("Invalid hour/minute for Berlin time")
            .and_local_timezone(chrono_tz::Europe::Berlin)
            .single()
            .expect("Ambiguous time during DST transition");
    }

    target.with_timezone(&Utc)
}

/// Sleeps until the next occurrence of a daily time in Europe/Berlin timezone.
#[instrument]
async fn wait_until_schedule(hour: u32, minute: u32) {
    let next_run = calculate_next_occurrence(hour, minute);
    let now = Utc::now();

    if next_run > now {
        let duration = (next_run - now)
            .to_std()
            .expect("Duration calculation failed");

        info!(
            next_run_utc = ?next_run,
            next_run_berlin = ?next_run.with_timezone(&chrono_tz::Europe::Berlin),
            wait_seconds = duration.as_secs(),
            "Sleeping until next scheduled time"
        );

        sleep(duration).await;
    }
}

/// Sleeps until a specific time today in Europe/Berlin timezone.
///
/// If the target time has already passed, returns immediately.
#[instrument]
async fn sleep_until_hms(hour: u32, minute: u32, second: u32, expected_latency: u32) {
    let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
    let time = now
        .date_naive()
        .and_hms_opt(hour, minute, second)
        .expect("Invalid stats time")
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()
        .expect("Ambiguous time during DST transition");

    let wait_duration =
        (time.with_timezone(&Utc) - Utc::now() - TimeDelta::milliseconds(expected_latency as i64))
            .to_std()
            .unwrap_or(Duration::from_secs(0));

    if wait_duration > Duration::from_secs(0) {
        info!(
            wait_seconds = wait_duration.as_secs(),
            "Waiting until 13:38 to post stats"
        );
        sleep(wait_duration).await;
    }
}

/// Checks if a username belongs to an ignored bot.
///
/// Returns true if the username matches any bot in the ignore list.
fn is_ignored_bot(username: &str) -> bool {
    ["supibot", "potatbotat"].contains(&username)
}

/// Determines if a message should be counted as a valid 1337 message.
///
/// Filters out bot messages and checks for keywords "1337" or "DANKIES".
fn is_valid_1337_message(message: &PrivmsgMessage) -> bool {
    if is_ignored_bot(&message.sender.login) {
        return false;
    }
    if message.message_text.contains("DANKIES") || message.message_text.contains("1337") {
        return true;
    }
    false
}

/// Generates a stats message based on the number of users who said 1337.
///
/// Returns a contextual message with emotes based on participation level.
fn generate_stats_message(count: usize, user_list: &[String]) -> String {
    match count {
        0 => one_of(&["Erm", "fuh"]).to_string(),
        1 => one_of(&[
            format!(
                "@{} zumindest einer {}",
                user_list
                    .first()
                    .expect("Count should equal user list length"),
                one_of(&["fuh", "uhh"])
            ),
            format!(
                "War wohl zu viel verlangt {}",
                one_of(&["BRUHSIT", "UltraMad", "Madeg"])
            ),
        ])
        .to_string(),
        2..=3 if !user_list.contains(&"gargoyletec".to_string()) => {
            format!(
                "{count}{} gnocci {}",
                one_of(&[" und nichtmal", ", aber wo", " und ohne"]),
                one_of(&["Sadding", "Sadge", "Sadeg", "SadgeCry", "Saddies"])
            )
        }
        2..=3 => format!(
            "{count}, {}",
            one_of(&[
                "geht besser Okayge",
                "verbesserungswürdig Waiting",
                "unterdurchschnittlich Waiting",
                "ausbaufähig Waiting",
                "entspricht nicht ganz den Erwarungen Waiting",
                "bemüht",
                "anpassungsfähig YEP"
            ])
        ),
        4 => one_of(&[
            "3.6, nicht gut, nicht dramatisch".to_string(),
            format!("{count}, {}", one_of(&["Standard Performance", "solide"])),
        ])
        .to_string(),
        5..=7 => {
            format!(
                "{count}, gute Auslese {}",
                one_of(&["bieberbutzemanPepe", "peepoHappy"])
            )
        }
        _ => {
            format!(
                "{count}, {}",
                one_of(&["insane quota Pag", "rekordverdächtig WICKED"])
            )
        }
    }
}

/// Encodes a string into hexadecimal representation.
///
/// Each byte of the input string is converted to its two-digit hex representation.
fn encode_hex(input: &str) -> String {
    input.bytes().map(|b| format!("{:02x}", b)).collect()
}

/// Returns a random element from an array.
///
/// Used for adding variety to bot responses by randomly selecting from predefined options.
fn one_of<const L: usize, T>(array: &[T; L]) -> &T {
    array.choose(&mut rand::rng()).unwrap()
}

/// Monitors broadcast messages and tracks users who say 1337 during the target minute.
///
/// Runs in a loop until the broadcast channel closes or an error occurs.
/// Only tracks messages sent during the configured TARGET_HOUR:TARGET_MINUTE.
#[instrument]
async fn monitor_1337_messages(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    total_users: Arc<Mutex<HashSet<String>>>,
) {
    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                // Check time and message content
                let local = privmsg
                    .server_timestamp
                    .with_timezone(&chrono_tz::Europe::Berlin);
                if (local.hour(), local.minute()) != (TARGET_HOUR, TARGET_MINUTE) {
                    continue;
                }

                if is_valid_1337_message(&privmsg) {
                    let username = &privmsg.sender.login;
                    debug!(user = %username, "User said 1337 at 13:37");

                    let mut users = total_users.lock().await;
                    // Double-check minute to prevent race condition
                    let current_minute = privmsg
                        .server_timestamp
                        .with_timezone(&chrono_tz::Europe::Berlin)
                        .minute();
                    if current_minute == TARGET_MINUTE {
                        if users.len() < MAX_USERS {
                            users.insert(privmsg.sender.login);
                        } else {
                            error!(max = MAX_USERS, "User limit reached");
                        }
                    } else {
                        debug!("Skipping insert, minute changed to {current_minute}");
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "1337 handler lagged, skipped messages");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, 1337 monitor exiting");
                break;
            }
        }
    }
}

/// Returns the session start and end times for a given date in Berlin timezone.
///
/// Schedule:
/// - Monday-Thursday: 18:00 - 23:00
/// - Friday: 18:00 - 01:00 (next day)
/// - Saturday: 15:00 - 01:00 (next day)
/// - Sunday: 15:00 - 23:00
///
/// Returns None if the date is before the first session (2025-11-29) or if time calculation fails.
fn get_session_times(
    date: chrono::NaiveDate,
) -> Option<(
    chrono::DateTime<chrono_tz::Tz>,
    chrono::DateTime<chrono_tz::Tz>,
)> {
    // Server doesn't exist before 2025-11-29
    let first_session = chrono::NaiveDate::from_ymd_opt(2025, 11, 30)?;
    if date < first_session {
        return None;
    }

    let weekday = date.weekday();
    let (start_hour, end_hour, end_next_day) = match weekday {
        Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu => (18, 23, false),
        Weekday::Fri => (18, 1, true),
        Weekday::Sat => (15, 1, true),
        Weekday::Sun => (15, 23, false),
    };

    let start = date
        .and_hms_opt(start_hour, 0, 0)?
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()?;

    let end_date = if end_next_day {
        date + chrono::Duration::days(1)
    } else {
        date
    };

    let end = end_date
        .and_hms_opt(end_hour, 0, 0)?
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()?;

    Some((start, end))
}

/// Checks if the Minecraft server is currently online based on the schedule.
///
/// Returns true if the current time falls within an active session window.
fn is_server_online(now: chrono::DateTime<chrono_tz::Tz>) -> bool {
    let date = now.date_naive();

    debug!(date = %date, now = %now, "Checking if server is online");

    // Check today's session
    if let Some((start, end)) = get_session_times(date) {
        debug!(start = %start, end = %end, "Today's session times");
        if now >= start && now < end {
            debug!("Server is online (current session)");
            return true;
        }
    } else {
        debug!("No session found for today");
    }

    // Check if we're in the early hours of a day following a late-night session
    // (between 00:00 and 01:00 after Friday or Saturday)
    if now.hour() == 0 {
        let yesterday = date - chrono::Duration::days(1);
        if let Some((_, end)) = get_session_times(yesterday)
            && now < end
        {
            debug!("Server is online (late-night session from yesterday)");
            return true;
        }
    }

    debug!("Server is offline");
    false
}

/// Calculates the next session start time based on the current time.
///
/// Returns the DateTime when the next Minecraft session will begin.
fn get_next_session_start(now: chrono::DateTime<chrono_tz::Tz>) -> chrono::DateTime<chrono_tz::Tz> {
    let mut check_date = now.date_naive();

    debug!(now = %now, "Calculating next session start");

    // Check if there's a session today that hasn't started yet
    if let Some((start, _)) = get_session_times(check_date) {
        debug!(today = %check_date, start = %start, "Found session today");
        if now < start {
            debug!(next_start = %start, "Session today hasn't started yet");
            return start;
        }
    }

    // Otherwise, check the next 30 days (should find the first session starting 2025-11-29)
    for i in 0..30 {
        check_date += chrono::Duration::days(1);
        debug!(checking_date = %check_date, iteration = i, "Checking next day");
        if let Some((start, _)) = get_session_times(check_date) {
            debug!(next_start = %start, "Found next session");
            return start;
        }
    }

    // This should never happen, but return a fallback
    error!("Could not find next session within 30 days, using fallback");
    now + chrono::Duration::days(1)
}

/// Formats a duration as a countdown string in German.
///
/// Returns a string like "1 Tag, 2 Stunden, 30 Minuten, 45 Sekunden".
fn format_countdown(duration: chrono::Duration) -> String {
    let days = duration.num_days();
    let hours = duration.num_hours() % 24;
    let minutes = duration.num_minutes() % 60;
    let seconds = duration.num_seconds() % 60;

    let mut parts = Vec::new();

    if days > 0 {
        parts.push(format!(
            "{} {}",
            days,
            if days == 1 { "Tag" } else { "Tage" }
        ));
    }
    if hours > 0 {
        parts.push(format!(
            "{} {}",
            hours,
            if hours == 1 { "Stunde" } else { "Stunden" }
        ));
    }
    if minutes > 0 {
        parts.push(format!(
            "{} {}",
            minutes,
            if minutes == 1 { "Minute" } else { "Minuten" }
        ));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!(
            "{} {}",
            seconds,
            if seconds == 1 { "Sekunde" } else { "Sekunden" }
        ));
    }

    parts.join(", ")
}

/// Processes a PRIVMSG to check if it's asking about Minecraft and sends a response.
///
/// Returns true if a response was sent, false otherwise.
#[instrument]
async fn process_minecraft_message(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
) -> bool {
    // Ignore messages from bots
    if is_ignored_bot(&privmsg.sender.login) {
        return false;
    }

    let text = privmsg.message_text.to_lowercase();
    if text.contains("wannminecraft") || text.contains("wann minecraft") {
        debug!(user = %privmsg.sender.login, "User asked about Minecraft");

        let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
        info!(current_time = %now, "Processing Minecraft query");

        let mut response = if is_server_online(now) {
            "Der Server ist online Okayge 👉 REDACTED_HOST:255652 PogChamp".to_string()
        } else {
            let next_start = get_next_session_start(now);
            let duration = next_start - now;
            info!(
                next_start = %next_start,
                duration_seconds = duration.num_seconds(),
                duration_days = duration.num_days(),
                "Calculated next session"
            );

            // Special response for REDACTED_USER: raw milliseconds only
            if privmsg.sender.login == "REDACTED_USER" {
                duration.num_milliseconds().to_string()
            } else {
                let countdown = format_countdown(duration);
                info!(countdown = %countdown, "Formatted countdown");
                format!("Noch {} WannMinecraft", countdown)
            }
        };

        // Hex-encode all replies to REDACTED_USER
        if privmsg.sender.login == "REDACTED_USER" {
            response = encode_hex(&response);
        }

        if let Err(e) = client.say_in_reply_to(privmsg, response).await {
            error!(error = ?e, "Failed to send Minecraft response");
        }
        return true;
    }
    false
}

/// Token storage implementation that persists tokens to disk.
///
/// Falls back to environment variables on first load if no token file exists.
#[derive(Debug)]
struct FileBasedTokenStorage {
    path: PathBuf,
}

impl Default for FileBasedTokenStorage {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./token.ron"),
        }
    }
}

#[async_trait]
impl TokenStorage for FileBasedTokenStorage {
    type LoadError = eyre::Report;
    type UpdateError = eyre::Report;

    #[instrument]
    async fn load_token(&mut self) -> Result<UserAccessToken, Self::LoadError> {
        // Try to load from file first
        match fs::read_to_string(&self.path).await {
            Ok(contents) => {
                debug!(
                    path = %self.path.display(),
                    "Loading user access token from file"
                );
                Ok(ron::from_str(&contents)?)
            }
            Err(_) => {
                // File doesn't exist, fall back to environment variables
                debug!("Token file not found, loading initial values from environment variables");
                let token = UserAccessToken {
                    access_token: TWITCH_ACCESS_TOKEN.expose_secret().to_string(),
                    refresh_token: TWITCH_REFRESH_TOKEN.expose_secret().to_string(),
                    created_at: chrono::Utc::now(),
                    expires_at: Some(chrono::Utc::now()), // we do not know when it expires
                };

                // Save the token for future use
                self.update_token(&token).await?;

                Ok(token)
            }
        }
    }

    #[instrument]
    async fn update_token(&mut self, token: &UserAccessToken) -> Result<(), Self::UpdateError> {
        debug!(path = %self.path.display(), "Updating token in file");
        let buffer = ron::to_string(token)?.into_bytes();
        File::create(&self.path).await?.write_all(&buffer).await?;
        Ok(())
    }
}

/// Main entry point for the twitch-1337 bot.
///
/// Establishes a persistent Twitch IRC connection and runs multiple handlers in parallel:
/// - Daily 1337 tracker: monitors 13:37 messages, posts stats at 13:38
/// - Minecraft responder: replies to "WannMinecraft"/"Wann Minecraft" messages
///
/// # Errors
///
/// Returns an error if required environment variables are missing or connection fails.
#[tokio::main]
pub async fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

    info!(local_time = ?local, utc_time = ?Utc::now(), "Starting twitch-1337 bot");

    // Validate environment variables at startup
    info!(channel = %*CHANNEL_LOGIN, "Channel configured");
    let _ = &*TWITCH_USERNAME;
    let _ = &*TWITCH_ACCESS_TOKEN;
    let _ = &*TWITCH_REFRESH_TOKEN;
    let _ = &*TWITCH_CLIENT_ID;
    let _ = &*TWITCH_CLIENT_SECRET;
    let _ = &*STREAMELEMENTS_API_TOKEN;
    info!("Environment variables validated");

    // Setup, connect, join channel, and verify authentication (all in one step)
    let (incoming_messages, client) = setup_and_verify_twitch_client().await?;

    // Wrap client in Arc for sharing across handlers
    let client = Arc::new(client);

    // Create broadcast channel for message distribution (capacity: 100 messages)
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    // Spawn message router task
    let router_handle = tokio::spawn(run_message_router(incoming_messages, broadcast_tx.clone()));

    // Spawn 1337 handler task
    let handler_1337 = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        async move {
            run_1337_handler(broadcast_tx, client).await;
        }
    });

    // Spawn Minecraft handler task
    let handler_minecraft = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        async move {
            run_minecraft_handler(broadcast_tx, client).await;
        }
    });

    let handler_generic_commands = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        async move { run_generic_command_handler(broadcast_tx, client).await }
    });

    let handler_scheduled_messages = tokio::spawn({
        let client = client.clone();
        async move { run_scheduled_message_handler(client).await }
    });

    info!(
        "Bot running with continuous connection. Handlers: 1337 tracker, Minecraft responder, Generic commands, Scheduled messages"
    );
    info!(
        "1337 tracker scheduled to run daily at {}:{:02} (Europe/Berlin)",
        TARGET_HOUR,
        TARGET_MINUTE - 1
    );
    info!(
        "Scheduled messages: {} message(s) configured",
        SCHEDULED_MESSAGES.len()
    );

    // Keep the program running until shutdown signal or any task exits
    info!("Bot is running. Press Ctrl+C to stop.");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received, exiting gracefully");
        }
        result = router_handle => {
            error!("Message router exited unexpectedly: {result:?}");
        }
        result = handler_1337 => {
            error!("1337 handler exited unexpectedly: {result:?}");
        }
        result = handler_minecraft => {
            error!("Minecraft handler exited unexpectedly: {result:?}");
        }
        result = handler_generic_commands => {
            error!("Generic Command Handler exited unexpectedly: {result:?}");
        }
        result = handler_scheduled_messages => {
            error!("Scheduled message handler exited unexpectedly: {result:?}");
        }
    }

    info!("Bot shutdown complete");
    Ok(())
}

#[instrument]
fn setup_twitch_client() -> (UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient) {
    // Create authenticated IRC client with refreshing tokens
    let credentials = RefreshingLoginCredentials::init_with_username(
        Some(TWITCH_USERNAME.clone()),
        TWITCH_CLIENT_ID.clone(),
        TWITCH_CLIENT_SECRET.expose_secret().to_string(),
        FileBasedTokenStorage::default(),
    );
    let config = ClientConfig::new_simple(credentials);
    TwitchIRCClient::<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>::new(
        config,
    )
}

/// Sets up and verifies the Twitch IRC connection.
///
/// Creates the client, connects, joins the configured channel, and verifies authentication
/// by waiting for a GlobalUserState message. Returns the verified client and message receiver.
///
/// # Errors
///
/// Returns an error if connection times out (30s) or authentication fails.
#[instrument]
async fn setup_and_verify_twitch_client()
-> Result<(UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient)> {
    info!("Setting up and verifying Twitch connection");

    let (mut incoming_messages, client) = setup_twitch_client();

    // Connect to Twitch IRC
    info!("Connecting to Twitch IRC");
    client.connect().await;

    // Join the configured channel
    info!(channel = %*CHANNEL_LOGIN, "Joining channel");
    let channels = [CHANNEL_LOGIN.to_string()].into();
    client.set_wanted_channels(channels)?;

    // Verify authentication by waiting for GlobalUserState message
    let verification = async {
        while let Some(message) = incoming_messages.recv().await {
            trace!(message = ?message, "Received IRC message during verification");

            match message {
                ServerMessage::Notice(NoticeMessage { message_text, .. })
                    if message_text == "Login authentication failed" =>
                {
                    error!(
                        "Authentication with Twitch IRC Servers failed: {}",
                        message_text
                    );
                    bail!(
                        "Failed to authenticate with Twitch. This is likely due to missing token scopes. \
                        Ensure your token has 'chat:read' and 'chat:edit' scopes."
                    );
                }
                ServerMessage::Notice(NoticeMessage { message_text, .. })
                    if message_text == "Login unsuccessful" =>
                {
                    error!(
                        "Authentication with Twitch IRC Servers failed: {}",
                        message_text
                    );
                    bail!(
                        "Failed to authenticate with Twitch. This is likely due to an invalid or expired token. \
                        Check your TWITCH_ACCESS_TOKEN and TWITCH_REFRESH_TOKEN."
                    );
                }
                ServerMessage::GlobalUserState(_) => {
                    info!("Connection verified and authenticated");
                    return Ok(());
                }
                _ => continue,
            }
        }
        bail!("Connection closed during verification")
    };

    match tokio::time::timeout(Duration::from_secs(30), verification).await {
        Err(_) => {
            error!("Connection to Twitch IRC Server timed out");
            bail!("Connection to Twitch timed out")
        }
        Ok(result) => result?,
    };

    Ok((incoming_messages, client))
}

/// Message router task that broadcasts incoming IRC messages to all handlers.
///
/// Reads from the twitch-irc receiver and broadcasts to all subscribed handlers.
/// Exits when the incoming_messages channel is closed.
#[instrument]
async fn run_message_router(
    mut incoming_messages: UnboundedReceiver<ServerMessage>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
) {
    info!("Message router started");

    while let Some(message) = incoming_messages.recv().await {
        trace!(message = ?message, "Routing IRC message");

        // Broadcast to all listeners (ignore errors if no receivers)
        let _ = broadcast_tx.send(message);
    }

    debug!("Message router exited (connection closed)");
}

/// Handler for the daily 1337 tracking feature.
///
/// Monitors messages during the 13:37 window, tracks unique users, and posts stats at 13:38.
/// Runs continuously, resetting state daily.
#[instrument]
async fn run_1337_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
) {
    info!("1337 handler started");

    loop {
        // Wait until 13:36 to start monitoring
        wait_until_schedule(TARGET_HOUR, TARGET_MINUTE - 1).await;

        info!("Starting daily 1337 monitoring session");

        // Fresh HashSet for today's users
        let total_users = Arc::new(Mutex::new(HashSet::with_capacity(MAX_USERS)));

        // Spawn message monitoring subtask
        let monitor_handle = tokio::spawn({
            let total_users = total_users.clone();
            // Subscribe fresh when we wake up - only see messages from now on
            let broadcast_rx = broadcast_tx.subscribe();

            async move {
                monitor_1337_messages(broadcast_rx, total_users).await;
            }
        });

        // Wait until 13:36:30 to send reminder
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30, EXPECTED_LATENCY).await;

        info!("Posting reminder to channel");
        if let Err(e) = client
            .say(CHANNEL_LOGIN.clone(), "PausersHype".to_string())
            .await
        {
            error!(error = ?e, "Failed to send reminder message");
        }

        // Wait until 13:38 to post stats
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0, EXPECTED_LATENCY).await;

        // Get user list and count
        let (count, user_list) = {
            let users = total_users.lock().await;
            let count = users.len();
            let mut user_vec: Vec<String> = users.iter().cloned().collect();
            user_vec.sort(); // Sort alphabetically for consistency
            (count, user_vec)
        };

        let message = generate_stats_message(count, &user_list);

        // Post stats message
        info!(count = count, "Posting stats to channel");
        if let Err(e) = client.say(CHANNEL_LOGIN.clone(), message).await {
            error!(error = ?e, count = count, "Failed to send stats message");
        } else {
            info!("Stats posted successfully");
        }

        // Abort the monitor task
        monitor_handle.abort();

        info!("Daily 1337 session completed, waiting for next day");
    }
}

/// Handler for responding to "WannMinecraft" messages.
///
/// Monitors chat for users asking about Minecraft and responds.
/// Runs continuously.
#[instrument]
async fn run_minecraft_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
) {
    info!("Minecraft handler started");

    // Subscribe to the broadcast channel
    let mut broadcast_rx = broadcast_tx.subscribe();

    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                process_minecraft_message(&privmsg, &client).await;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "Minecraft handler lagged, skipped messages");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, Minecraft handler exiting");
                break;
            }
        }
    }
}

/// Handler for generic text commands that start with `!`.
///
/// Monitors chat for commands and dispatches them to appropriate handlers.
/// Currently supports:
/// - `!toggle-ping <command>` - Adds/removes user from StreamElements ping command
///
/// Runs continuously in a loop, processing all incoming messages.
#[instrument]
async fn run_generic_command_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
) {
    info!("Generic Command Handler started");

    // Subscribe to the broadcast channel
    let mut broadcast_rx = broadcast_tx.subscribe();

    // Initialize StreamElements client
    let se_client = match SEClient::new(STREAMELEMENTS_API_TOKEN.expose_secret()) {
        Ok(client) => client,
        Err(e) => {
            error!(error = ?e, "Failed to initialize StreamElements client");
            error!("Generic Command Handler cannot start without valid StreamElements API token");
            return;
        }
    };

    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                // Catch any errors from command handling to prevent task crash
                if let Err(e) = handle_generic_commands(&privmsg, &client, &se_client).await {
                    error!(
                        error = ?e,
                        user = %privmsg.sender.login,
                        message = %privmsg.message_text,
                        "Error handling generic command"
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "Generic Command Handler lagged, skipped messages");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, Generic Command Handler exiting");
                break;
            }
        }
    }
}

/// Dispatches chat messages to the appropriate command handler.
///
/// Parses the first word of the message and routes to specialized handlers.
/// This acts as a simple command router for all `!` commands.
///
/// # Errors
///
/// Returns an error if command execution fails, but does not crash the handler.
#[instrument]
async fn handle_generic_commands(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
) -> Result<()> {
    let mut words = privmsg.message_text.split_whitespace();
    let Some(first_word) = words.next() else {
        return Ok(());
    };

    if first_word == "!toggle-ping" {
        toggle_ping_command(privmsg, client, se_client, words.next()).await?;
    } else if first_word == "!list-pings" {
        list_pings_command(privmsg, client, se_client, words.next()).await?;
    }

    Ok(())
}

// TODO: use layzlock and request id dynamically
const STREAMELEMENTS_CHANNEL_ID: &str = "REDACTED_SE_ID";
const PING_COMMANDS: &[&str] = &[
    "ackern",
    "amra",
    "arbeitszeitbetrug",
    "dayz",
    "dbd",
    "deadlock",
    "eft",
    "euv",
    "fetentiere",
    "front",
    "hoi",
    "kluft",
    "kreuzzug",
    "ron",
    "ttt",
    "vicky",
];

/// Toggles a user's mention in a StreamElements ping command.
///
/// Ping commands are used to notify community members about game sessions (e.g., Minecraft).
/// This function adds the requesting user's @mention to the command reply if not present,
/// or removes it if already present.
///
/// # Command Format
///
/// `!toggle-ping <command_name>`
///
/// # Behavior
///
/// 1. Searches for a StreamElements command matching `<command_name>` with the "pinger" keyword
/// 2. If user's @mention exists in the reply, removes it (case-insensitive)
/// 3. If not present, adds @mention after the first existing @ symbol (or at the start)
/// 4. Updates the command via StreamElements API
/// 5. Confirms success to the user
///
/// # Error Responses
///
/// - "Das kann ich nicht FDM" - No command name provided
/// - "Das finde ich nicht FDM" - Command not found
///
/// # Errors
///
/// Returns an error if IRC communication or StreamElements API calls fail.
/// User-facing errors are sent as chat messages before returning the error.
#[instrument]
async fn toggle_ping_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    command_name: Option<&str>,
) -> Result<()> {
    let Some(command_name) = command_name else {
        // Best-effort reply, log but don't fail if this specific reply fails
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das kann ich nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'no command name' error message");
        }
        return Ok(());
    };

    if !PING_COMMANDS.contains(&command_name) {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das finde ich nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'command not found' error message");
        }
        return Ok(());
    }

    // Fetch all commands from StreamElements
    let commands = se_client
        .get_all_commands(STREAMELEMENTS_CHANNEL_ID)
        .await
        .wrap_err("Failed to fetch commands from StreamElements API")?;

    // Find the matching command with "pinger" keyword
    let Some(mut command) = commands
        .into_iter()
        .find(|command| command.command == command_name)
    else {
        // Best-effort reply
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das gibt es nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'command not found' error message");
        }
        return Ok(());
    };

    // Create case-insensitive regex to find user's mention
    // Use regex::escape to prevent username from being interpreted as regex
    let escaped_username = regex::escape(&privmsg.sender.login);
    let re = Regex::new(&format!("(?i)@?\\s*{}", escaped_username))
        .wrap_err("Failed to create username regex")?;

    // Toggle user's mention in the command reply
    let mut has_added_ping = false;
    let new_reply = if re.is_match(&command.reply) {
        // Remove user's mention
        re.replace_all(&command.reply, "").to_string()
    } else {
        has_added_ping = true;
        // Add user's mention
        if let Some(insert_location) = command.reply.find('@') {
            // Insert after first @ symbol
            let (head, tail) = command.reply.split_at(insert_location);
            format!("{head} @{} {tail}", privmsg.sender.name)
        } else {
            // No @ found, add at the beginning
            format!("@{} {}", privmsg.sender.name, command.reply)
        }
    };

    // Clean up whitespaces
    command.reply = new_reply.split_whitespace().collect::<Vec<_>>().join(" ");

    debug!(
        command_name = %command_name,
        user = %privmsg.sender.login,
        new_reply = %command.reply,
        "Updating ping command"
    );

    // Update the command via StreamElements API
    se_client
        .update_command(STREAMELEMENTS_CHANNEL_ID, command)
        .await
        .wrap_err("Failed to update command via StreamElements API")?;

    // Confirm success to the user
    client
        .say_in_reply_to(
            privmsg,
            format!(
                "Hab ich {} gemacht Okayge",
                match has_added_ping {
                    true => "an",
                    false => "aus",
                }
            ),
        )
        .await
        .wrap_err("Failed to send success confirmation message")?;

    Ok(())
}

#[instrument]
async fn list_pings_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    enabled_option: Option<&str>,
) -> Result<()> {
    let filter = enabled_option.unwrap_or("enabled");

    let commands = se_client
        .get_all_commands(STREAMELEMENTS_CHANNEL_ID)
        .await
        .wrap_err("Failed to fetch commands from StreamElements API")?;

    let response = match filter {
        "enabled" => &commands
            .iter()
            .filter(|command| PING_COMMANDS.contains(&command.command.as_str()))
            .filter(|command| {
                command
                    .reply
                    .to_lowercase()
                    .contains(&format!("@{}", privmsg.sender.login.to_lowercase()))
            })
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "disabled" => &commands
            .iter()
            .filter(|command| PING_COMMANDS.contains(&command.command.as_str()))
            .filter(|command| {
                !command
                    .reply
                    .to_lowercase()
                    .contains(&format!("@{}", privmsg.sender.login.to_lowercase()))
            })
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "all" => &PING_COMMANDS.join(" "),
        _ => "Das weiß ich nicht Sadding",
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response.to_string()).await {
        error!(error = ?e, "Failed to send response message");
    }

    Ok(())
}

/// Handler for sending scheduled messages at configured intervals.
///
/// Spawns a separate task for each scheduled message. Each task runs independently
/// and posts its message at its configured interval.
#[instrument]
async fn run_scheduled_message_handler(client: Arc<AuthenticatedTwitchClient>) {
    info!("Scheduled message handler started");

    let mut handles = Vec::new();

    for (index, (message, interval_hours)) in SCHEDULED_MESSAGES.iter().enumerate() {
        let client = client.clone();
        let message = message.to_string();
        let interval_hours = *interval_hours;

        let handle = tokio::spawn(async move {
            info!(
                message_index = index,
                interval_hours = interval_hours,
                "Starting scheduled message task"
            );

            loop {
                // Sleep for the configured interval
                sleep(Duration::from_secs(interval_hours * 3600)).await;

                info!(
                    message_index = index,
                    message = %message,
                    "Posting scheduled message"
                );

                // Post the message
                if let Err(e) = client.say(CHANNEL_LOGIN.clone(), message.clone()).await {
                    error!(
                        error = ?e,
                        message_index = index,
                        "Failed to send scheduled message"
                    );
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all tasks (they run forever, so this will only exit on error)
    for (index, handle) in handles.into_iter().enumerate() {
        let result = handle.await;
        error!(
            error = ?result,
            message_index = index,
            "Scheduled message task exited unexpectedly"
        );
    }
}

mod database {
    struct CaffeineProduct {
        id: u64,
        name: String,
        unit: String,
        mg_coffeine: f64,
    }

    struct CaffeineConsumption {
        id: u64,
        user_id: String,
        product_id: u64,
        amount: f64,
        timestamp: chrono::NaiveDateTime,
    }
}
