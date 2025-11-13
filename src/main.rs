use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, LazyLock},
    usize,
};

use async_trait::async_trait;
use chrono::{Timelike, Utc};
use color_eyre::eyre::{self, Context, Result, bail};
use rand::seq::IndexedRandom as _;
use secrecy::{ExposeSecret, SecretString};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::{Mutex, mpsc::UnboundedReceiver},
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

const TARGET_HOUR: u32 = 13;
const TARGET_MINUTE: u32 = 37;

/// Duration to wait after posting stats before disconnecting (30 seconds)
const POST_STATS_DELAY: Duration = Duration::from_secs(30);

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
                info!(
                    path = %self.path.display(),
                    "Loading user access token from file"
                );
                Ok(ron::from_str(&contents)?)
            }
            Err(_) => {
                // File doesn't exist, fall back to environment variables
                info!("Token file not found, loading intial values from environment variables");
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
        info!( path = %self.path.display(), "Updating token in file");
        let buffer = ron::to_string(token)?.into_bytes();
        File::create(&self.path).await?.write_all(&buffer).await?;
        Ok(())
    }
}

/// Main entry point for the twitch-1337 bot.
///
/// Runs a daily task that connects at 13:36 Berlin time, monitors messages at 13:37,
/// posts stats at 13:38, and disconnects at 13:38:30.
///
/// # Errors
///
/// Returns an error if required environment variables are missing.
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

    verify_twitch_connection().await?;

    let start_minute = TARGET_MINUTE - 1;

    // Spawn main task - runs daily at 13:36 Berlin time
    let main_task = tokio::spawn(async move {
        loop {
            wait_until_schedule(TARGET_HOUR, start_minute).await;

            info!("Starting daily bot session");

            if let Err(e) = run_daily_session().await {
                error!("Daily session failed: {e:?}");
            }

            info!("Daily session completed, waiting for next day");
        }
    });

    info!(
        "Bot scheduled to run daily at {}:{:02} (Europe/Berlin)",
        TARGET_HOUR, start_minute
    );

    // Keep the program running until shutdown signal
    info!("Bot is running. Press Ctrl+C to stop.");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received, exiting gracefully");
        }
        result = main_task => {
            error!("Main task unexpectedly exited: {result:?}");
        }
    }

    info!("Bot shutdown complete");
    Ok(())
}

fn setup_twitch_client() -> (
    UnboundedReceiver<ServerMessage>,
    TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>,
) {
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

#[instrument]
async fn verify_twitch_connection() -> Result<()> {
    info!("Verifying Twitch Connection");

    let (mut incoming_messages, client) = setup_twitch_client();

    client.connect().await;

    let handle = tokio::spawn(async move {
        while let Some(message) = incoming_messages.recv().await {
            trace!(message = ?message, "Received irc message");

            match message {
                ServerMessage::Notice(NoticeMessage { message_text, .. })
                    if message_text == "Login authentication failed" =>
                {
                    error!("Authentication with Twitch IRC Servers failed");
                    bail!("Failed to authenticate with twitch");
                }
                ServerMessage::GlobalUserState(_) => return Ok(()),
                _ => continue,
            }
        }

        Ok(())
    });

    match tokio::time::timeout(Duration::from_secs(30), handle).await {
        Err(elapsed) => {
            error!(elapsed = ?elapsed, "Connection to Twitch IRC Server timed out");
            bail!("Connection to Twitch timed out")
        }
        Ok(inner) => inner??,
    };

    info!("Connection to Twitch verified");

    Ok(())
}

/// Runs a single daily session: connect, monitor messages, post stats, disconnect.
///
/// # Errors
///
/// Returns an error if IRC connection fails, channel join fails, message sending fails,
/// or the monitoring task panics.
async fn run_daily_session() -> Result<()> {
    let (mut incoming_messages, client) = setup_twitch_client();

    info!(channel = %*CHANNEL_LOGIN, "Joining channel");
    let channels = [CHANNEL_LOGIN.to_string()].into();
    client.set_wanted_channels(channels)?;

    // Shared HashSet to track unique users (scoped to this session)
    let total_users = Arc::new(Mutex::new(HashSet::with_capacity(MAX_USERS)));

    // Spawn message monitoring task
    let monitor_handle = tokio::spawn({
        let total_users = total_users.clone();
        async move {
            while let Some(message) = incoming_messages.recv().await {
                trace!(message = ?message, "Received irc message");

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
            debug!("Message monitor loop exited");
        }
    });

    // Wait until 13:36:30 to send reminder
    sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30).await;

    info!("Posting reminder to channel");
    client
        .say(CHANNEL_LOGIN.clone(), "PausersHype".to_string())
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to send reminder message to channel '{}'",
                *CHANNEL_LOGIN
            )
        })?;

    // Wait until 13:38 to post stats
    sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0).await;

    // Get user list and count while keeping monitor running
    let (count, user_list) = {
        let users = total_users.lock().await;
        let count = users.len();
        let mut user_vec: Vec<String> = users.iter().cloned().collect();
        user_vec.sort(); // Sort alphabetically for consistency
        (count, user_vec)
    };

    let message = match count {
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
    };

    // Post stats message
    info!(count = count, "Posting stats to channel");
    client
        .say(CHANNEL_LOGIN.clone(), message)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to send stats message (count: {}) to channel '{}'",
                count, *CHANNEL_LOGIN
            )
        })?;

    info!("Stats posted successfully");

    // Wait 30 seconds before disconnecting
    info!(
        "Waiting {} seconds before disconnecting",
        POST_STATS_DELAY.as_secs()
    );
    sleep(POST_STATS_DELAY).await;

    // Disconnect client (will close incoming_messages and stop monitor task)
    info!("Disconnecting IRC client");
    drop(client);

    // Wait for monitor task to finish
    monitor_handle.await.wrap_err("Monitor task panicked")?;

    info!("Daily session completed successfully");
    Ok(())
}

fn one_of<const L: usize, T>(array: &[T; L]) -> &T {
    array.choose(&mut rand::rng()).unwrap()
}
