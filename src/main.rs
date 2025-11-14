use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use async_trait::async_trait;
use chrono::{Datelike, Timelike, Utc, Weekday};
use color_eyre::eyre::{self, Result, bail};
use rand::seq::IndexedRandom as _;
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

// TODO: handle twitch latency (90ms)
// TODO: ping lazy users

/// Type alias for the authenticated Twitch IRC client
type AuthenticatedTwitchClient =
    TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>;

const TARGET_HOUR: u32 = 13;
const TARGET_MINUTE: u32 = 37;

/// Maximum number of unique users to track (prevents unbounded memory growth)
const MAX_USERS: usize = 10_000;

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
async fn sleep_until_hms(hour: u32, minute: u32, second: u32) {
    let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
    let time = now
        .date_naive()
        .and_hms_opt(hour, minute, second)
        .expect("Invalid stats time")
        .and_local_timezone(chrono_tz::Europe::Berlin)
        .single()
        .expect("Ambiguous time during DST transition");

    let wait_duration = (time.with_timezone(&Utc) - Utc::now())
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

/// Determines if a message should be counted as a valid 1337 message.
///
/// Filters out bot messages and checks for keywords "1337" or "DANKIES".
fn is_valid_1337_message(message: &PrivmsgMessage) -> bool {
    if ["supibot", "potatbotat"].contains(&message.sender.login.as_str()) {
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
        1 => format!(
            "@{} zumindest einer {}",
            user_list
                .first()
                .expect("Count should equal user list length"),
            one_of(&["fuh", "uhh"])
        ),
        2..=3 if !user_list.contains(&"gargoyletec".to_string()) => {
            format!(
                "{count}, nichtmal gnocci {}",
                one_of(&["Sadding", "Sadge", "Sadeg", "SadgeCry"])
            )
        }
        2..=3 => format!(
            "{count}, {}",
            one_of(&[
                "geht besser Okayge",
                "Verbesserungswürdig",
                "unterdurchnittlich"
            ])
        ),
        4 => "3.6, nicht gut, nicht dramatisch".to_string(),
        5..=7 => {
            format!(
                "{count}, gute Auslese {}",
                one_of(&["bieberbutzemanPepe", "peepoHappy"])
            )
        }
        _ => {
            format!("{count}, insane quota Pag")
        }
    }
}

/// Monitors broadcast messages and tracks users who say 1337 during the target minute.
///
/// Runs in a loop until the broadcast channel closes or an error occurs.
/// Only tracks messages sent during the configured TARGET_HOUR:TARGET_MINUTE.
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
                let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
                if (local.hour(), local.minute()) != (TARGET_HOUR, TARGET_MINUTE) {
                    continue;
                }

                if is_valid_1337_message(&privmsg) {
                    let username = &privmsg.sender.login;
                    debug!(user = %username, "User said 1337 at 13:37");

                    let mut users = total_users.lock().await;
                    // Double-check minute to prevent race condition
                    let current_minute = Utc::now()
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
    let first_session = chrono::NaiveDate::from_ymd_opt(2025, 11, 29)?;
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

    // Check today's session
    if let Some((start, end)) = get_session_times(date)
        && now >= start
        && now < end
    {
        return true;
    }

    // Check if we're in the early hours of a day following a late-night session
    // (between 00:00 and 01:00 after Friday or Saturday)
    if now.hour() == 0 {
        let yesterday = date - chrono::Duration::days(1);
        if let Some((_, end)) = get_session_times(yesterday)
            && now < end
        {
            return true;
        }
    }

    false
}

/// Calculates the next session start time based on the current time.
///
/// Returns the DateTime when the next Minecraft session will begin.
fn get_next_session_start(now: chrono::DateTime<chrono_tz::Tz>) -> chrono::DateTime<chrono_tz::Tz> {
    let mut check_date = now.date_naive();

    // Check if there's a session today that hasn't started yet
    if let Some((start, _)) = get_session_times(check_date)
        && now < start
    {
        return start;
    }

    // Otherwise, check the next 7 days
    for _ in 0..7 {
        check_date += chrono::Duration::days(1);
        if let Some((start, _)) = get_session_times(check_date) {
            return start;
        }
    }

    // This should never happen, but return a fallback
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
async fn process_minecraft_message(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
) -> bool {
    let text = privmsg.message_text.to_lowercase();
    if text.contains("wannminecraft") || text.contains("wann minecraft") {
        debug!(user = %privmsg.sender.login, "User asked about Minecraft");

        let now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

        let response = if is_server_online(now) {
            "Der Server ist online Okayge 👉 REDACTED_HOST:255652 PogChamp".to_string()
        } else {
            let next_start = get_next_session_start(now);
            let duration = next_start - now;
            let countdown = format_countdown(duration);
            format!("Noch {} WannMinecraft", countdown)
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

    info!("Bot running with continuous connection. Handlers: 1337 tracker, Minecraft responder");
    info!(
        "1337 tracker scheduled to run daily at {}:{:02} (Europe/Berlin)",
        TARGET_HOUR,
        TARGET_MINUTE - 1
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
    }

    info!("Bot shutdown complete");
    Ok(())
}

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
                    error!("Authentication with Twitch IRC Servers failed");
                    bail!("Failed to authenticate with Twitch");
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
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30).await;

        info!("Posting reminder to channel");
        if let Err(e) = client
            .say(CHANNEL_LOGIN.clone(), "PausersHype".to_string())
            .await
        {
            error!(error = ?e, "Failed to send reminder message");
        }

        // Wait until 13:38 to post stats
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0).await;

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

fn one_of<const L: usize, T>(array: &[T; L]) -> &T {
    array.choose(&mut rand::rng()).unwrap()
}
