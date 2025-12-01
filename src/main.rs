use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use async_trait::async_trait;
use chrono::{Datelike, TimeDelta, Timelike, Utc, Weekday};
use color_eyre::eyre::{self, Result, WrapErr, bail, eyre};
use rand::seq::IndexedRandom as _;
use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::{Mutex, broadcast, mpsc::UnboundedReceiver},
    time::{Duration, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::{RefreshingLoginCredentials, TokenStorage, UserAccessToken},
    message::{NoticeMessage, PrivmsgMessage, ServerMessage},
};

mod storage;

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
        #[instrument(skip(token))]
        // TODO: make secret
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
        #[instrument(skip(command))]
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
    ["supibot", "potatbotat", "streamelements"].contains(&username)
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
#[instrument(skip(broadcast_rx, total_users))]
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
/// - Monday-Thursday: 18:00 - 00:00
/// - Friday: 18:00 - 02:00 (next day)
/// - Saturday: 12:00 - 02:00 (next day)
/// - Sunday: 12:00 - 00:00
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
        Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu => (18, 0, true),
        Weekday::Fri => (18, 2, true),
        Weekday::Sat => (12, 2, true),
        Weekday::Sun => (12, 0, true),
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
#[instrument(skip(privmsg, client))]
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

        let response = if is_server_online(now) {
            "Der Server ist online Okayge 👉 REDACTED_IP:25565 PogChamp".to_string()
        } else {
            let next_start = get_next_session_start(now);
            let duration = next_start - now;
            info!(
                next_start = %next_start,
                duration_seconds = duration.num_seconds(),
                duration_days = duration.num_days(),
                "Calculated next session"
            );

            let time = next_start.time().to_string();
            if duration.num_days() > 0 {
                format!("Morgen um {time} WannMinecraft")
            } else {
                format!("Heute um {time} WannMinecraft")
            }
        };

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

    #[instrument(skip(self))]
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

    #[instrument(skip(self, token))]
    async fn update_token(&mut self, token: &UserAccessToken) -> Result<(), Self::UpdateError> {
        debug!(path = %self.path.display(), "Updating token in file");
        let buffer = ron::to_string(token)?.into_bytes();
        File::create(&self.path).await?.write_all(&buffer).await?;
        Ok(())
    }
}

fn install_tracing() {
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, fmt};

    let fmt_layer = fmt::layer().with_target(false);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();
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
#[instrument]
pub async fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    // Initialize tracing subscriber
    install_tracing();

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

    ensure_data_dir().await?;

    // Setup, connect, join channel, and verify authentication (all in one step)
    let (incoming_messages, client) = setup_and_verify_twitch_client().await?;

    // Wrap client in Arc for sharing across handlers
    let client = Arc::new(client);

    // Create broadcast channel for message distribution (capacity: 100 messages)
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    // Spawn message router task
    let router_handle = tokio::spawn(run_message_router(incoming_messages, broadcast_tx.clone()));

    // Check if Google Sheets is configured for scheduled messages
    let sheets_configured = std::env::var("GOOGLE_SHEETS_SPREADSHEET_ID").is_ok()
        && std::env::var("GOOGLE_SERVICE_ACCOUNT_PATH").is_ok();

    // Optionally spawn schedule loader service and handler
    let (loader_service, handler_scheduled_messages) = if sheets_configured {
        info!("Google Sheets configured, starting scheduled message system");

        // Create schedule cache for dynamic scheduled messages
        let schedule_cache = Arc::new(tokio::sync::RwLock::new(database::ScheduleCache::new()));

        // Spawn schedule loader service
        let loader = tokio::spawn({
            let cache = schedule_cache.clone();
            async move {
                run_schedule_loader_service(cache).await;
            }
        });

        // Spawn scheduled message handler
        let handler = tokio::spawn({
            let client = client.clone();
            let cache = schedule_cache.clone();
            async move { run_scheduled_message_handler(client, cache).await }
        });

        (Some(loader), Some(handler))
    } else {
        warn!(
            "Google Sheets not configured (GOOGLE_SHEETS_SPREADSHEET_ID and GOOGLE_SERVICE_ACCOUNT_PATH required). Scheduled messages disabled."
        );
        (None, None)
    };

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

    if sheets_configured {
        info!(
            "Bot running with continuous connection. Handlers: Schedule loader, 1337 tracker, Minecraft responder, Generic commands, Scheduled messages"
        );
        info!("Scheduled messages: Loaded dynamically from Google Sheets or cache");
    } else {
        info!(
            "Bot running with continuous connection. Handlers: 1337 tracker, Minecraft responder, Generic commands"
        );
    }
    info!(
        "1337 tracker scheduled to run daily at {}:{:02} (Europe/Berlin)",
        TARGET_HOUR,
        TARGET_MINUTE - 1
    );

    // Keep the program running until shutdown signal or any task exits
    info!("Bot is running. Press Ctrl+C to stop.");

    // Handle optional scheduled message handlers
    match (loader_service, handler_scheduled_messages) {
        (Some(loader), Some(handler)) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutdown signal received, exiting gracefully");
                }
                result = router_handle => {
                    error!("Message router exited unexpectedly: {result:?}");
                }
                result = loader => {
                    error!("Schedule loader service exited unexpectedly: {result:?}");
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
                result = handler => {
                    error!("Scheduled message handler exited unexpectedly: {result:?}");
                }
            }
        }
        _ => {
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
            }
        }
    }

    info!("Bot shutdown complete");
    Ok(())
}

fn get_data_dir() -> PathBuf {
    std::env::var("DATA_DIR")
        .unwrap_or_else(|_| "/var/lib/twitch-1337".to_string())
        .into()
}

#[instrument]
async fn ensure_data_dir() -> Result<()> {
    let data_dir = get_data_dir();
    if !data_dir.exists() {
        tokio::fs::create_dir_all(data_dir).await?;
    }
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
#[instrument(skip(incoming_messages, broadcast_tx))]
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
#[instrument(skip(broadcast_tx, client))]
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
#[instrument(skip(broadcast_tx, client))]
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
#[instrument(skip(broadcast_tx, client))]
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
#[instrument(skip(privmsg, client, se_client))]
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
#[instrument(skip(privmsg, client, se_client))]
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

#[instrument(skip(privmsg, client, se_client))]
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

/// Run a single schedule task.
/// This task will run the schedule at its configured interval,
/// checking if it's still active before each post.
#[instrument(skip(client, cache), fields(schedule = %schedule.name))]
async fn run_schedule_task(
    schedule: database::Schedule,
    client: Arc<AuthenticatedTwitchClient>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
) {
    use chrono::Utc;
    use tokio::time::{Duration, sleep};

    info!(
        schedule = %schedule.name,
        interval_seconds = schedule.interval.num_seconds(),
        "Schedule task started"
    );

    loop {
        // Wait for the configured interval
        let interval_duration = Duration::from_secs(schedule.interval.num_seconds() as u64);
        sleep(interval_duration).await;

        // Check if schedule still exists in cache
        let still_exists = {
            let cache_guard = cache.read().await;
            cache_guard
                .schedules
                .iter()
                .any(|s| s.name == schedule.name)
        };

        if !still_exists {
            info!(
                schedule = %schedule.name,
                "Schedule no longer in cache, stopping task"
            );
            break;
        }

        // Check if schedule is currently active (respects date range and time window)
        let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

        if !schedule.is_active(now) {
            debug!(
                schedule = %schedule.name,
                "Schedule not active at current time, skipping post"
            );
            continue;
        }

        // Post the message
        info!(
            schedule = %schedule.name,
            message = %schedule.message,
            "Posting scheduled message"
        );

        if let Err(e) = client
            .say(CHANNEL_LOGIN.clone(), schedule.message.clone())
            .await
        {
            error!(
                error = ?e,
                schedule = %schedule.name,
                "Failed to send scheduled message"
            );
        } else {
            debug!(schedule = %schedule.name, "Scheduled message posted successfully");
        }
    }

    info!(schedule = %schedule.name, "Schedule task exiting");
}

/// Dynamic scheduled message handler that monitors cache for changes.
/// Spawns and stops tasks dynamically based on cache updates.
#[instrument(skip(client, cache))]
async fn run_scheduled_message_handler(
    client: Arc<AuthenticatedTwitchClient>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
) {
    use std::collections::HashMap;
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, interval};

    info!("Dynamic scheduled message handler started");

    // Track running tasks by schedule name
    let mut running_tasks: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut current_version = 0u64;

    // Monitor cache for changes every 30 seconds
    let mut check_interval = interval(Duration::from_secs(30));

    loop {
        check_interval.tick().await;

        let (schedules, version) = {
            let cache_guard = cache.read().await;
            (cache_guard.schedules.clone(), cache_guard.version)
        };

        // Check if cache version has changed
        if version != current_version {
            info!(
                old_version = current_version,
                new_version = version,
                schedule_count = schedules.len(),
                "Cache version changed, updating tasks"
            );

            current_version = version;

            // Build set of schedule names that should be running
            let desired_schedules: HashMap<String, database::Schedule> =
                schedules.into_iter().map(|s| (s.name.clone(), s)).collect();

            // Stop tasks for schedules that no longer exist or have changed
            running_tasks.retain(|name, handle| {
                if !desired_schedules.contains_key(name) {
                    info!(schedule = %name, "Stopping task for removed/changed schedule");
                    handle.abort();
                    false
                } else {
                    true
                }
            });

            // Start tasks for new schedules
            for (name, schedule) in desired_schedules {
                running_tasks.entry(name.clone()).or_insert_with(|| {
                    info!(schedule = %name, "Starting task for new schedule");

                    tokio::spawn(run_schedule_task(
                        schedule.clone(),
                        client.clone(),
                        cache.clone(),
                    ))
                });
            }

            info!(active_tasks = running_tasks.len(), "Task update complete");
        }
    }
}

mod database {
    use chrono::{DateTime, NaiveDateTime, NaiveTime, TimeDelta, Utc};
    use chrono_tz::Tz;
    use eyre::{Result, eyre};
    use serde::{Deserialize, Serialize};

    struct CaffeineProduct {
        name: String,
        unit: String,
        mg_coffeine: f64,
    }

    struct CaffeineConsumption {
        username: String,
        product: u64,
        amount: f64,
        timestamp: chrono::NaiveDateTime,
    }

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct Schedule {
        pub name: String,
        pub start_date: Option<NaiveDateTime>,
        pub end_date: Option<NaiveDateTime>,
        pub active_time_start: Option<NaiveTime>,
        pub active_time_end: Option<NaiveTime>,
        pub interval: TimeDelta,
        pub message: String,
    }

    impl Schedule {
        /// Check if the schedule is currently active based on date range and time window.
        pub fn is_active(&self, now: DateTime<Tz>) -> bool {
            // Check date range
            if let Some(start) = self.start_date {
                let start_utc = start.and_utc();
                if now < start_utc {
                    return false;
                }
            }

            if let Some(end) = self.end_date {
                let end_utc = end.and_utc();
                if now > end_utc {
                    return false;
                }
            }

            // Check time window (if specified)
            if let (Some(start_time), Some(end_time)) =
                (self.active_time_start, self.active_time_end)
            {
                let current_time = now.time();

                // Handle midnight-spanning windows (e.g., 22:00 - 02:00)
                if end_time < start_time {
                    // Window spans midnight: active if time >= start OR time < end
                    if !(current_time >= start_time || current_time < end_time) {
                        return false;
                    }
                } else {
                    // Normal window: active if time is within range
                    if !(current_time >= start_time && current_time < end_time) {
                        return false;
                    }
                }
            }

            true
        }

        /// Parse interval string into TimeDelta.
        /// Supports formats:
        /// - "hh:mm" (e.g., "01:30" for 1 hour 30 minutes)
        /// - Legacy "30m", "1h", "2h30m" format (backwards compatibility)
        pub fn parse_interval(s: &str) -> Result<TimeDelta> {
            let s = s.trim();
            if s.is_empty() {
                return Err(eyre!("Interval string is empty"));
            }

            // Try parsing as hh:mm format first
            if s.contains(':') {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() != 2 {
                    return Err(eyre!("Invalid hh:mm format: {}", s));
                }

                let hours: i64 = parts[0]
                    .parse()
                    .map_err(|_| eyre!("Invalid hours in hh:mm format: {}", parts[0]))?;

                let minutes: i64 = parts[1]
                    .parse()
                    .map_err(|_| eyre!("Invalid minutes in hh:mm format: {}", parts[1]))?;

                if hours < 0 || minutes < 0 || minutes >= 60 {
                    return Err(eyre!(
                        "Invalid hh:mm values (hours={}, minutes={})",
                        hours,
                        minutes
                    ));
                }

                let total_seconds = hours * 3600 + minutes * 60;

                // Enforce minimum interval of 1 minute to prevent spam
                if total_seconds < 60 {
                    return Err(eyre!("Interval must be at least 1 minute (got {})", s));
                }

                return TimeDelta::try_seconds(total_seconds)
                    .ok_or_else(|| eyre!("Interval too large: {} seconds", total_seconds));
            }

            // Legacy format parsing (e.g., "30m", "1h", "2h30m")
            let s = s.to_lowercase();
            let mut total_seconds = 0i64;
            let mut current_num = String::new();

            for ch in s.chars() {
                if ch.is_ascii_digit() {
                    current_num.push(ch);
                } else if ch == 'h' || ch == 'm' || ch == 's' {
                    if current_num.is_empty() {
                        return Err(eyre!("No number before unit '{}'", ch));
                    }

                    let num: i64 = current_num
                        .parse()
                        .map_err(|_| eyre!("Invalid number: {}", current_num))?;

                    total_seconds += match ch {
                        'h' => num * 3600,
                        'm' => num * 60,
                        's' => num,
                        _ => unreachable!(),
                    };

                    current_num.clear();
                } else {
                    return Err(eyre!("Invalid character in interval: '{}'", ch));
                }
            }

            if !current_num.is_empty() {
                return Err(eyre!(
                    "Number without unit at end of interval: {}",
                    current_num
                ));
            }

            if total_seconds == 0 {
                return Err(eyre!("Interval must be greater than zero"));
            }

            // Enforce minimum interval of 1 minute to prevent spam
            if total_seconds < 60 {
                return Err(eyre!(
                    "Interval must be at least 1 minute (got {} seconds)",
                    total_seconds
                ));
            }

            TimeDelta::try_seconds(total_seconds)
                .ok_or_else(|| eyre!("Interval too large: {} seconds", total_seconds))
        }

        /// Validate the schedule for required fields and logical consistency.
        pub fn validate(&self) -> Result<()> {
            // Name is required and must not be empty
            if self.name.trim().is_empty() {
                return Err(eyre!("Schedule name cannot be empty"));
            }

            // Message is required and must not be empty
            if self.message.trim().is_empty() {
                return Err(eyre!("Schedule message cannot be empty"));
            }

            // Interval must be positive
            if self.interval.num_seconds() <= 0 {
                return Err(eyre!("Interval must be positive"));
            }

            // Interval must be at least 1 minute
            if self.interval.num_seconds() < 60 {
                return Err(eyre!("Interval must be at least 1 minute"));
            }

            // If both start_date and end_date are set, end must be after start
            if let (Some(start), Some(end)) = (self.start_date, self.end_date)
                && end <= start
            {
                return Err(eyre!("End date must be after start date"));
            }

            // Time window validation: both or neither must be set
            match (self.active_time_start, self.active_time_end) {
                (Some(_), None) => {
                    return Err(eyre!(
                        "active_time_end must be set if active_time_start is set"
                    ));
                }
                (None, Some(_)) => {
                    return Err(eyre!(
                        "active_time_start must be set if active_time_end is set"
                    ));
                }
                _ => {} // Both set or both None is valid
            }

            Ok(())
        }
    }

    /// Cache structure for storing loaded schedules with metadata.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct ScheduleCache {
        pub schedules: Vec<Schedule>,
        pub last_updated: DateTime<Utc>,
        pub version: u64,
    }

    impl ScheduleCache {
        /// Create a new empty cache.
        pub fn new() -> Self {
            Self {
                schedules: Vec::new(),
                last_updated: Utc::now(),
                version: 0,
            }
        }

        /// Update cache with new schedules, incrementing version.
        pub fn update(&mut self, schedules: Vec<Schedule>) {
            self.schedules = schedules;
            self.last_updated = Utc::now();
            self.version += 1;
        }
    }
}

fn get_schedule_cache_dir() -> PathBuf {
    get_data_dir().join("schedule_cache.ron")
}

/// Save the schedule cache to disk in RON format.
fn save_cache_to_disk(cache: &database::ScheduleCache) -> Result<()> {
    let cache_path = get_schedule_cache_dir();

    // Serialize to RON format
    let ron_string = ron::ser::to_string_pretty(cache, ron::ser::PrettyConfig::default())
        .wrap_err("Failed to serialize cache to RON")?;

    // Write to file
    std::fs::write(&cache_path, ron_string)
        .wrap_err_with(|| format!("Failed to write cache to {}", cache_path.display()))?;

    debug!(
        path = %cache_path.display(),
        version = cache.version,
        "Cache saved to disk"
    );
    Ok(())
}

/// Load the schedule cache from disk.
fn load_cache_from_disk() -> Result<database::ScheduleCache> {
    let cache_path = get_schedule_cache_dir();

    // Read file contents
    let contents = std::fs::read_to_string(&cache_path)
        .wrap_err_with(|| format!("Failed to read cache from {}", cache_path.display()))?;

    // Deserialize from RON
    let cache: database::ScheduleCache =
        ron::from_str(&contents).wrap_err("Failed to deserialize cache from RON")?;

    info!(
        path = %cache_path.display(),
        version = cache.version,
        schedule_count = cache.schedules.len(),
        last_updated = %cache.last_updated,
        "Cache loaded from disk"
    );

    Ok(cache)
}

/// Custom HTTP client builder for yup-oauth2 that uses webpki-roots instead of native certs.
/// This is required for FROM scratch containers that don't have system CA certificates.
struct WebPkiHyperClient;

impl yup_oauth2::authenticator::HyperClientBuilder for WebPkiHyperClient {
    type Connector =
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;

    fn build_hyper_client(
        self,
    ) -> std::result::Result<
        hyper_util::client::legacy::Client<Self::Connector, String>,
        yup_oauth2::Error,
    > {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots() // Use webpki-roots instead of native-roots
            .https_or_http()
            .enable_http1()
            .build();

        Ok(
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector),
        )
    }

    fn build_test_hyper_client(
        self,
    ) -> hyper_util::client::legacy::Client<Self::Connector, String> {
        self.build_hyper_client().unwrap()
    }
}

/// Build a mapping of column names to their indices from the header row.
/// Returns a HashMap with normalized (lowercase, trimmed) column names as keys.
fn build_column_map(
    header_row: &[serde_json::Value],
) -> Result<std::collections::HashMap<String, usize>> {
    use std::collections::HashMap;

    let mut map = HashMap::new();

    for (index, cell) in header_row.iter().enumerate() {
        if let Some(name) = cell.as_str() {
            let normalized = name.trim().to_lowercase();
            if !normalized.is_empty() {
                map.insert(normalized, index);
            }
        }
    }

    // Check for required columns
    let required_columns = ["schedule name", "message", "interval"];
    for col in &required_columns {
        if !map.contains_key(*col) {
            return Err(eyre!(
                "Required column '{}' not found in header row. Available columns: {:?}",
                col,
                map.keys().collect::<Vec<_>>()
            ));
        }
    }

    Ok(map)
}

/// Fetch schedules from Google Sheets.
/// Returns a vector of validated schedules.
async fn fetch_schedules_from_sheets() -> Result<Vec<database::Schedule>> {
    use google_sheets4::api::Sheets;

    // Get configuration from environment
    let spreadsheet_id = std::env::var("GOOGLE_SHEETS_SPREADSHEET_ID")
        .wrap_err("GOOGLE_SHEETS_SPREADSHEET_ID environment variable not set")?;

    let sheet_name = std::env::var("GOOGLE_SHEETS_SHEET_NAME")
        .unwrap_or_else(|_| "ScheduledMessages".to_string());

    let service_account_path = std::env::var("GOOGLE_SERVICE_ACCOUNT_PATH")
        .wrap_err("GOOGLE_SERVICE_ACCOUNT_PATH environment variable not set")?;

    debug!(
        spreadsheet_id = %spreadsheet_id,
        sheet_name = %sheet_name,
        service_account_path = %service_account_path,
        "Fetching schedules from Google Sheets"
    );

    // Load service account credentials
    let service_account_key = yup_oauth2::read_service_account_key(&service_account_path)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to read service account key from {}",
                service_account_path
            )
        })?;

    // Create authenticator with custom HTTP client that uses webpki-roots
    // The default yup-oauth2 client uses native-roots which won't work in FROM scratch containers
    let auth = yup_oauth2::ServiceAccountAuthenticator::with_client(
        service_account_key,
        WebPkiHyperClient,
    )
    .build()
    .await
    .wrap_err("Failed to create service account authenticator")?;

    // Build HTTP client with webpki roots for the Sheets API
    // This must match the connector type from WebPkiHyperClient
    let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();

    // Create hyper client for google-sheets4 (uses BoxBody as the body type)
    let hyper_client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build(https_connector);

    // Create Sheets hub
    let hub = Sheets::new(hyper_client, auth);

    // First, fetch the header row to determine column positions
    let header_range = format!("'{}'!A1:Z1", sheet_name);
    let header_result = hub
        .spreadsheets()
        .values_get(&spreadsheet_id, &header_range)
        .doit()
        .await
        .wrap_err("Failed to fetch header row from Google Sheets")?;

    let header_values = header_result
        .1
        .values
        .ok_or_else(|| eyre!("No header row found in Google Sheets"))?;

    let header_row = header_values
        .first()
        .ok_or_else(|| eyre!("Header row is empty"))?;

    // Build column name -> index mapping (case-insensitive)
    let column_map = build_column_map(header_row)?;

    debug!(columns = ?column_map, "Column mapping created");

    // Determine the last column to fetch based on max index
    let max_index = column_map.values().max().copied().unwrap_or(0);
    let last_column = std::char::from_u32(65 + max_index as u32)
        .ok_or_else(|| eyre!("Column index too large"))?;

    // Fetch data from sheet (A2:LAST to skip header row)
    let range = format!("'{}'!A2:{}", sheet_name, last_column);

    let result = hub
        .spreadsheets()
        .values_get(&spreadsheet_id, &range)
        .doit()
        .await
        .wrap_err("Failed to fetch data from Google Sheets")?;

    let values = result
        .1
        .values
        .ok_or_else(|| eyre!("No data found in Google Sheets"))?;

    info!(row_count = values.len(), "Fetched rows from Google Sheets");

    // Parse rows into Schedule objects
    let mut schedules = Vec::new();
    let mut skipped_rows = 0;

    for (index, row) in values.iter().enumerate() {
        let row_num = index + 2; // +2 because we skip header (row 1) and are 0-indexed

        match parse_schedule_row(row, row_num, &column_map) {
            Ok(Some(schedule)) => {
                // Validate schedule
                if let Err(e) = schedule.validate() {
                    error!(row = row_num, error = ?e, "Schedule validation failed");
                    skipped_rows += 1;
                } else {
                    schedules.push(schedule);
                }
            }
            Ok(None) => {
                // Schedule is disabled, skip silently (already logged at debug level)
            }
            Err(e) => {
                error!(row = row_num, content = ?row, error = ?e, "Failed to parse schedule row");
                skipped_rows += 1;
            }
        }
    }

    info!(
        loaded = schedules.len(),
        skipped = skipped_rows,
        "Parsed schedules from Google Sheets"
    );

    Ok(schedules)
}

/// Parse a Google Sheets row into a Schedule using the column mapping.
/// Required columns: Schedule Name, Message, Interval
/// Optional columns: Start Date, End Date, Active Time Start, Active Time End, Enabled
/// Returns Ok(None) if schedule is disabled (no error logged).
fn parse_schedule_row(
    row: &[serde_json::Value],
    row_num: usize,
    column_map: &std::collections::HashMap<String, usize>,
) -> Result<Option<database::Schedule>> {
    use chrono::{NaiveDateTime, NaiveTime};

    // Helper to get string value from named column
    let get_string = |col_name: &str| -> Result<String> {
        let index = column_map
            .get(col_name)
            .ok_or_else(|| eyre!("Column '{}' not found in mapping", col_name))?;

        row.get(*index)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                eyre!(
                    "Column '{}' (index {}) is missing or not a string",
                    col_name,
                    index
                )
            })
    };

    // Helper to get optional string value from named column
    let get_optional_string = |col_name: &str| -> Option<String> {
        let index = *column_map.get(col_name)?;
        row.get(index)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
    };

    // Parse required fields
    let name = get_string("schedule name")
        .wrap_err_with(|| format!("Row {}: Schedule Name is required", row_num))?;

    let message =
        get_string("message").wrap_err_with(|| format!("Row {}: Message is required", row_num))?;

    let interval_str = get_string("interval")
        .wrap_err_with(|| format!("Row {}: Interval is required", row_num))?;

    let interval = database::Schedule::parse_interval(&interval_str).wrap_err_with(|| {
        format!(
            "Row {}: Invalid interval format '{}'",
            row_num, interval_str
        )
    })?;

    // Check if schedule is enabled (if Enabled column exists)
    // Silently skip disabled schedules by returning Ok(None)
    if let Some(enabled_str) = get_optional_string("enabled") {
        let enabled = enabled_str.to_lowercase();
        if enabled == "false" || enabled == "no" || enabled == "0" {
            debug!(row = row_num, name = %name, "Skipping disabled schedule");
            return Ok(None);
        }
    }

    // Parse optional date fields
    // Support multiple date formats: ISO 8601, DD/MM/YYYY, and DD/MM/YYYY HH:MM
    let parse_date = |s: &str| -> Result<NaiveDateTime> {
        // Try ISO 8601 format first (YYYY-MM-DDTHH:MM:SS)
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
            return Ok(dt);
        }

        // Try DD/MM/YYYY HH:MM format
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%d/%m/%Y %H:%M") {
            return Ok(dt);
        }

        // Try DD/MM/YYYY format (assume midnight)
        if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%d/%m/%Y") {
            return Ok(date.and_hms_opt(0, 0, 0).unwrap());
        }

        Err(eyre!(
            "Invalid date format '{}' (expected YYYY-MM-DDTHH:MM:SS, DD/MM/YYYY HH:MM, or DD/MM/YYYY)",
            s
        ))
    };

    let start_date = if let Some(s) = get_optional_string("start date") {
        Some(
            parse_date(&s)
                .wrap_err_with(|| format!("Row {}: Invalid start date format", row_num))?,
        )
    } else {
        None
    };

    let end_date = if let Some(s) = get_optional_string("end date") {
        Some(parse_date(&s).wrap_err_with(|| format!("Row {}: Invalid end date format", row_num))?)
    } else {
        None
    };

    // Parse optional time window fields (HH:MM format)
    let active_time_start = if let Some(s) = get_optional_string("active time start") {
        Some(NaiveTime::parse_from_str(&s, "%H:%M").wrap_err_with(|| {
            format!(
                "Row {}: Invalid active time start format '{}' (expected HH:MM)",
                row_num, s
            )
        })?)
    } else {
        None
    };

    let active_time_end = if let Some(s) = get_optional_string("active time end") {
        Some(NaiveTime::parse_from_str(&s, "%H:%M").wrap_err_with(|| {
            format!(
                "Row {}: Invalid active time end format '{}' (expected HH:MM)",
                row_num, s
            )
        })?)
    } else {
        None
    };

    Ok(Some(database::Schedule {
        name,
        start_date,
        end_date,
        active_time_start,
        active_time_end,
        interval,
        message,
    }))
}

/// Schedule loader service that polls Google Sheets and updates the cache.
/// Runs continuously in a background task.
#[instrument(skip(cache))]
async fn run_schedule_loader_service(cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>) {
    use tokio::time::{Duration, interval};

    info!("Schedule loader service started");

    // Initial load on startup
    let initial_schedules = match try_load_schedules_from_any_source().await {
        Ok(schedules) => {
            info!(count = schedules.len(), "Initial schedules loaded");
            schedules
        }
        Err(e) => {
            warn!(error = ?e, "Failed to load initial schedules, starting with empty cache");
            Vec::new()
        }
    };

    // Update cache with initial schedules
    {
        let mut cache_guard = cache.write().await;
        cache_guard.update(initial_schedules);
    }

    // Save initial cache to disk
    {
        let cache_guard = cache.read().await;
        if let Err(e) = save_cache_to_disk(&cache_guard) {
            error!(error = ?e, "Failed to save initial cache to disk");
        }
    }

    // Start polling loop (every 5 minutes)
    let mut poll_interval = interval(Duration::from_secs(300)); // 5 minutes
    poll_interval.tick().await; // Skip first tick (we just did initial load)

    let mut consecutive_failures = 0;

    loop {
        poll_interval.tick().await;

        debug!("Polling Google Sheets for schedule updates");

        match fetch_schedules_from_sheets().await {
            Ok(schedules) => {
                consecutive_failures = 0;

                info!(
                    count = schedules.len(),
                    "Successfully fetched schedules from Google Sheets"
                );

                // Update cache
                {
                    let mut cache_guard = cache.write().await;
                    cache_guard.update(schedules);
                    info!(version = cache_guard.version, "Cache updated");
                }

                // Save to disk
                {
                    let cache_guard = cache.read().await;
                    if let Err(e) = save_cache_to_disk(&cache_guard) {
                        error!(error = ?e, "Failed to save cache to disk");
                    }
                }
            }
            Err(e) => {
                consecutive_failures += 1;

                error!(
                    error = ?e,
                    consecutive_failures,
                    "Failed to fetch schedules from Google Sheets"
                );

                if consecutive_failures >= 10 {
                    warn!(
                        consecutive_failures,
                        "Many consecutive failures, cache may be stale"
                    );
                }
            }
        }
    }
}

/// Try to load schedules from any available source in priority order:
/// 1. Google Sheets (if configured)
/// 2. Disk cache
/// 3. Empty (last resort)
async fn try_load_schedules_from_any_source() -> Result<Vec<database::Schedule>> {
    // Check if Google Sheets is configured
    let sheets_configured = std::env::var("GOOGLE_SHEETS_SPREADSHEET_ID").is_ok()
        && std::env::var("GOOGLE_SERVICE_ACCOUNT_PATH").is_ok();

    if sheets_configured {
        info!("Google Sheets configured, attempting to load from Google Sheets");

        match fetch_schedules_from_sheets().await {
            Ok(schedules) => {
                info!(
                    count = schedules.len(),
                    "Loaded schedules from Google Sheets"
                );
                return Ok(schedules);
            }
            Err(e) => {
                warn!(error = ?e, "Failed to load from Google Sheets, trying disk cache");
            }
        }
    } else {
        info!("Google Sheets not configured, skipping");
    }

    // Try disk cache
    match load_cache_from_disk() {
        Ok(cache) => {
            info!(
                count = cache.schedules.len(),
                "Loaded schedules from disk cache"
            );
            return Ok(cache.schedules);
        }
        Err(e) => {
            warn!(error = ?e, "Failed to load from disk cache");
        }
    }

    // Last resort: empty cache
    warn!("No schedules available from any source, starting with empty cache");
    Ok(Vec::new())
}
