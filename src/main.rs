use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{MappedLocalTime, TimeDelta, Timelike, Utc};
use color_eyre::eyre::{self, Result, WrapErr, bail};
use rand::seq::IndexedRandom as _;
use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::{Mutex, broadcast, mpsc::UnboundedReceiver},
    time::{Duration, sleep},
};
use tracing::{debug, error, info, instrument, trace, warn};
use twitch_irc::irc;
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::{RefreshingLoginCredentials, TokenStorage, UserAccessToken},
    message::{NoticeMessage, PongMessage, PrivmsgMessage, ServerMessage},
};

mod aviation;
mod commands;
mod database;
mod openrouter;
mod streamelements;

use crate::openrouter::{ChatCompletionRequest, Message, OpenRouterClient};
use crate::streamelements::SEClient;

/// Type alias for the authenticated Twitch IRC client
pub(crate) type AuthenticatedTwitchClient =
    TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>;

const TARGET_HOUR: u32 = 13;
const TARGET_MINUTE: u32 = 37;

/// Maximum number of unique users to track (prevents unbounded memory growth)
const MAX_USERS: usize = 10_000;

/// Interval between PING measurements
const LATENCY_PING_INTERVAL: Duration = Duration::from_secs(300);

/// Timeout waiting for PONG response
const LATENCY_PING_TIMEOUT: Duration = Duration::from_secs(10);

/// EMA smoothing factor (0.2 = moderate responsiveness)
const LATENCY_EMA_ALPHA: f64 = 0.2;

/// Only log EMA changes at info level when delta exceeds this threshold
const LATENCY_LOG_THRESHOLD: u32 = 10;

const LEADERBOARD_FILENAME: &str = "leaderboard.ron";

pub(crate) static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

fn default_expected_latency() -> u32 {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TwitchConfiguration {
    channel: String,
    username: String,
    #[serde(serialize_with = "serialize_secret_string")]
    refresh_token: SecretString,
    #[serde(serialize_with = "serialize_secret_string")]
    client_id: SecretString,
    #[serde(serialize_with = "serialize_secret_string")]
    client_secret: SecretString,
    #[serde(default = "default_expected_latency")]
    expected_latency: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StreamelementsConfig {
    #[serde(serialize_with = "serialize_secret_string")]
    api_token: SecretString,
    channel_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OpenRouterConfig {
    #[serde(serialize_with = "serialize_secret_string")]
    api_key: SecretString,
    /// OpenRouter model to use (default: "google/gemini-2.0-flash-exp:free")
    #[serde(default = "default_openrouter_model")]
    model: String,
}

fn default_openrouter_model() -> String {
    "google/gemini-2.0-flash-exp:free".to_string()
}

/// Configuration for a scheduled message loaded from config.toml.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ScheduleConfig {
    name: String,
    message: String,
    /// Interval in "hh:mm" format (e.g., "01:30" for 1 hour 30 minutes)
    interval: String,
    /// Start date in ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
    #[serde(default)]
    start_date: Option<String>,
    /// End date in ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
    #[serde(default)]
    end_date: Option<String>,
    /// Daily active time start in HH:MM format
    #[serde(default)]
    active_time_start: Option<String>,
    /// Daily active time end in HH:MM format
    #[serde(default)]
    active_time_end: Option<String>,
    /// Whether the schedule is enabled (default: true)
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Configuration {
    twitch: TwitchConfiguration,
    streamelements: StreamelementsConfig,
    #[serde(default)]
    openrouter: Option<OpenRouterConfig>,
    #[serde(default)]
    schedules: Vec<ScheduleConfig>,
}

fn serialize_secret_string<S>(secret: &SecretString, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(secret.expose_secret())
}

impl Configuration {
    fn validate(&self) -> Result<()> {
        if self.twitch.channel.trim().is_empty() {
            bail!("twitch.channel cannot be empty");
        }

        if self.twitch.username.trim().is_empty() {
            bail!("twitch.username cannot be empty");
        }

        if self.twitch.expected_latency > 1000 {
            bail!("twitch.expected_latency must be <= 1000ms (got {})", self.twitch.expected_latency);
        }

        if self.streamelements.channel_id.trim().is_empty() {
            bail!("streamelements.channel_id cannot be empty");
        }

        // Validate each schedule config
        for schedule in &self.schedules {
            if schedule.name.trim().is_empty() {
                bail!("Schedule name cannot be empty");
            }
            if schedule.message.trim().is_empty() {
                bail!("Schedule '{}' message cannot be empty", schedule.name);
            }
            if schedule.interval.trim().is_empty() {
                bail!("Schedule '{}' interval cannot be empty", schedule.name);
            }
            // Validate interval format by parsing it
            database::Schedule::parse_interval(&schedule.interval)
                .wrap_err_with(|| format!("Schedule '{}' has invalid interval format", schedule.name))?;
        }

        Ok(())
    }
}

/// Resolves a naive datetime to Berlin local time, handling DST transitions.
///
/// During spring-forward (gap), interprets as UTC to land just after the gap.
/// During fall-back (ambiguous), picks the later occurrence.
fn resolve_berlin_time(naive: chrono::NaiveDateTime) -> chrono::DateTime<chrono_tz::Tz> {
    match naive.and_local_timezone(chrono_tz::Europe::Berlin) {
        MappedLocalTime::Single(t) => t,
        MappedLocalTime::Ambiguous(_, latest) => latest,
        MappedLocalTime::None => naive.and_utc().with_timezone(&chrono_tz::Europe::Berlin),
    }
}

/// Calculates the next occurrence of a daily time in Europe/Berlin timezone.
///
/// If the specified time has already passed today, returns tomorrow's occurrence.
fn calculate_next_occurrence(hour: u32, minute: u32) -> chrono::DateTime<Utc> {
    let berlin_now = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

    // Create target time today in Berlin timezone
    let mut target = resolve_berlin_time(
        berlin_now
            .date_naive()
            .and_hms_opt(hour, minute, 0)
            .expect("Invalid hour/minute for Berlin time"),
    );

    // If target time has already passed today, schedule for tomorrow
    if target <= berlin_now {
        target = resolve_berlin_time(
            (berlin_now + chrono::Duration::days(1))
                .date_naive()
                .and_hms_opt(hour, minute, 0)
                .expect("Invalid hour/minute for Berlin time"),
        );
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
    let time = resolve_berlin_time(
        now.date_naive()
            .and_hms_opt(hour, minute, second)
            .expect("Invalid stats time"),
    );

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

/// Checks if a given user is a clanker
///
/// Returns true if the login name matches any bot in the ignore list.
fn is_clanker(login: &str) -> bool {
    [
        "supibot",
        "potatbotat",
        "streamelements",
        "koknuts",
        "thedagothur",
    ]
    .contains(&login)
}

/// Determines if a message should be counted as a valid 1337 message.
///
/// Filters out clanker messages and checks for keywords "1337" or "DANKIES".
fn is_valid_1337_message(message: &PrivmsgMessage) -> bool {
    if is_clanker(&message.sender.login) {
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

/// Returns a random element from an array.
///
/// Used for adding variety to bot responses by randomly selecting from predefined options.
fn one_of<const L: usize, T>(array: &[T; L]) -> &T {
    array.choose(&mut rand::rng()).unwrap()
}

/// Parse a compact duration string like "1h", "30m", "2h30m" into a Duration.
fn parse_flight_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else {
            match ch.to_ascii_lowercase() {
                'h' => {
                    let hours: u64 = current_num.parse().ok()?;
                    total_secs += hours * 3600;
                    current_num.clear();
                }
                'm' => {
                    let minutes: u64 = current_num.parse().ok()?;
                    total_secs += minutes * 60;
                    current_num.clear();
                }
                _ => return None,
            }
        }
    }

    if !current_num.is_empty() || total_secs == 0 {
        return None;
    }

    Some(std::time::Duration::from_secs(total_secs))
}

/// System prompt for the AI command, instructing the model how to behave.
const AI_SYSTEM_PROMPT: &str = r#"You are a helpful Twitch chat bot assistant. Keep responses brief (2-3 sentences max) since they'll appear in chat. Be friendly and casual. Respond in the same language the user writes in (German or English)."#;

/// Maximum response length for Twitch chat (to stay within limits).
pub(crate) const MAX_RESPONSE_LENGTH: usize = 500;

/// Executes the AI command by sending a chat completion request to OpenRouter.
async fn execute_ai_request(
    instruction: &str,
    openrouter_client: &OpenRouterClient,
) -> Result<String> {
    let messages = vec![
        Message {
            role: "system".to_string(),
            content: Some(AI_SYSTEM_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: "user".to_string(),
            content: Some(instruction.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let request = ChatCompletionRequest {
        model: openrouter_client.model().to_string(),
        messages,
        tools: None,
    };

    let response = openrouter_client.chat_completion(request).await?;

    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| eyre::eyre!("No choices in OpenRouter response"))?;

    choice
        .message
        .content
        .ok_or_else(|| eyre::eyre!("No text response from OpenRouter"))
}

/// Truncates a string to the maximum number of characters at a word boundary.
pub(crate) fn truncate_response(text: &str, max_chars: usize) -> String {
    // Collapse whitespace and newlines
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");

    // Find the byte index corresponding to the max_chars boundary
    let byte_limit = match collapsed.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => byte_idx,
        None => return collapsed, // Fewer than max_chars characters
    };

    // Find last space before the character limit
    let truncated = &collapsed[..byte_limit];
    if let Some(last_space) = truncated.rfind(' ') {
        format!("{}...", &truncated[..last_space])
    } else {
        format!("{}...", truncated)
    }
}

/// Monitors broadcast messages and tracks users who say 1337 during the target minute.
///
/// Runs in a loop until the broadcast channel closes or an error occurs.
/// Only tracks messages sent during the configured TARGET_HOUR:TARGET_MINUTE.
#[instrument(skip(broadcast_rx, total_users))]
async fn monitor_1337_messages(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    total_users: Arc<Mutex<HashMap<String, Option<u64>>>>,
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
                    let mut users = total_users.lock().await;
                    // Double-check minute to prevent race condition
                    let current_minute = local.minute();
                    if current_minute == TARGET_MINUTE {
                        if users.len() < MAX_USERS {
                            // Only insert if user not already present (first message wins)
                            if !users.contains_key(privmsg.sender.login.as_str()) {
                                let username = privmsg.sender.login.clone();
                                let ms_since_minute = local.second() as u64 * 1000
                                    + local.timestamp_subsec_millis() as u64;
                                if ms_since_minute < 1000 {
                                    debug!(user = %username, ms = ms_since_minute, "User said 1337 at 13:37 (sub-second)");
                                    users.insert(username, Some(ms_since_minute));
                                } else {
                                    debug!(user = %username, "User said 1337 at 13:37");
                                    users.insert(username, None);
                                }
                            }
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


/// A user's personal best time for the 1337 challenge.
///
/// Tracks the fastest sub-1-second message time and the date it was achieved.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersonalBest {
    /// Milliseconds after 13:37:00.000 (0-999)
    ms: u64,
    /// The date (Europe/Berlin) when this record was set
    date: chrono::NaiveDate,
}

/// Loads the all-time leaderboard from disk.
///
/// Returns an empty HashMap if the file doesn't exist or is corrupted.
async fn load_leaderboard() -> HashMap<String, PersonalBest> {
    let path = get_data_dir().join(LEADERBOARD_FILENAME);
    match fs::read_to_string(&path).await {
        Ok(contents) => match ron::from_str::<HashMap<String, PersonalBest>>(&contents) {
            Ok(leaderboard) => {
                info!(
                    entries = leaderboard.len(),
                    "Loaded leaderboard from {}",
                    path.display()
                );
                leaderboard
            }
            Err(e) => {
                warn!(error = ?e, "Failed to parse leaderboard file, starting fresh");
                HashMap::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("No leaderboard file found, starting fresh");
            HashMap::new()
        }
        Err(e) => {
            warn!(error = ?e, "Failed to read leaderboard file, starting fresh");
            HashMap::new()
        }
    }
}

/// Saves the all-time leaderboard to disk.
///
/// Logs an error and continues if the write fails.
async fn save_leaderboard(leaderboard: &HashMap<String, PersonalBest>) {
    let path = get_data_dir().join(LEADERBOARD_FILENAME);
    match ron::to_string(leaderboard) {
        Ok(serialized) => {
            if let Err(e) = fs::write(&path, serialized.as_bytes()).await {
                error!(error = ?e, "Failed to write leaderboard file");
            } else {
                info!(
                    entries = leaderboard.len(),
                    "Saved leaderboard to {}",
                    path.display()
                );
            }
        }
        Err(e) => {
            error!(error = ?e, "Failed to serialize leaderboard");
        }
    }
}

/// Token storage implementation that persists tokens to disk.
///
/// Falls back to initial refresh token from config on first load if no token file exists.
#[derive(Debug)]
struct FileBasedTokenStorage {
    path: PathBuf,
    initial_refresh_token: SecretString,
}

impl FileBasedTokenStorage {
    fn new(initial_refresh_token: SecretString) -> Self {
        Self {
            path: get_data_dir().join("token.ron"),
            initial_refresh_token,
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist yet, use initial refresh token from configuration
                warn!("Token file not found, using refresh token from configuration");
                let token = UserAccessToken {
                    access_token: String::new(),
                    refresh_token: self.initial_refresh_token.expose_secret().to_string(),
                    created_at: chrono::Utc::now(),
                    expires_at: None,
                };

                // Save the token for future use
                self.update_token(&token).await?;

                Ok(token)
            }
            Err(e) => {
                Err(eyre::Report::from(e).wrap_err("Failed to read token file"))
            }
        }
    }

    #[instrument(skip(self, token))]
    async fn update_token(&mut self, token: &UserAccessToken) -> Result<(), Self::UpdateError> {
        debug!(path = %self.path.display(), "Updating token in file");
        let buffer = ron::to_string(token)?.into_bytes();
        let tmp_path = self.path.with_extension("ron.tmp");
        File::create(&tmp_path).await?.write_all(&buffer).await?;
        fs::rename(&tmp_path, &self.path).await?;
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

fn get_config_path() -> PathBuf {
    get_data_dir().join("config.toml")
}

async fn load_configuration() -> Result<Configuration> {
    let config_path = get_config_path();
    let data = tokio::fs::read_to_string(&config_path)
        .await
        .wrap_err_with(|| format!(
            "Failed to read config file: {}\nPlease create config.toml from config.toml.example",
            config_path.display()
        ))?;

    info!("Loading configuration from {}", config_path.display());

    let config: Configuration = toml::from_str(&data)
        .wrap_err("Failed to parse config.toml - check for syntax errors")?;

    config.validate()?;

    Ok(config)
}

/// Parse a datetime string in ISO 8601 format (YYYY-MM-DDTHH:MM:SS).
fn parse_datetime(s: &str) -> Result<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .wrap_err_with(|| format!("Invalid datetime format '{}' (expected YYYY-MM-DDTHH:MM:SS)", s))
}

/// Parse a time string in HH:MM format.
fn parse_time(s: &str) -> Result<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M")
        .wrap_err_with(|| format!("Invalid time format '{}' (expected HH:MM)", s))
}

/// Convert a ScheduleConfig from config.toml into a database::Schedule.
fn schedule_config_to_schedule(config: &ScheduleConfig) -> Result<database::Schedule> {
    let interval = database::Schedule::parse_interval(&config.interval)?;

    let start_date = config.start_date.as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let end_date = config.end_date.as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let active_time_start = config.active_time_start.as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let active_time_end = config.active_time_end.as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let schedule = database::Schedule {
        name: config.name.clone(),
        start_date,
        end_date,
        active_time_start,
        active_time_end,
        interval,
        message: config.message.clone(),
    };

    schedule.validate()?;

    Ok(schedule)
}

/// Load schedules from the Configuration struct.
/// Filters out disabled schedules and validates all enabled ones.
fn load_schedules_from_config(config: &Configuration) -> Vec<database::Schedule> {
    let mut schedules = Vec::new();

    for schedule_config in &config.schedules {
        if !schedule_config.enabled {
            debug!(schedule = %schedule_config.name, "Skipping disabled schedule");
            continue;
        }

        match schedule_config_to_schedule(schedule_config) {
            Ok(schedule) => schedules.push(schedule),
            Err(e) => {
                error!(
                    schedule = %schedule_config.name,
                    error = ?e,
                    "Failed to parse schedule config, skipping"
                );
            }
        }
    }

    schedules
}

/// Reload configuration from config.toml and extract schedules.
/// Returns None if config cannot be loaded or parsed.
fn reload_schedules_from_config() -> Option<Vec<database::Schedule>> {
    let config_path = get_config_path();
    let data = match std::fs::read_to_string(&config_path) {
        Ok(data) => data,
        Err(e) => {
            error!(error = ?e, path = %config_path.display(), "Failed to read config for reload");
            return None;
        }
    };

    let config: Configuration = match toml::from_str(&data) {
        Ok(config) => config,
        Err(e) => {
            error!(error = ?e, path = %config_path.display(), "Failed to parse config for reload");
            return None;
        }
    };

    if let Err(e) = config.validate() {
        error!(error = ?e, "Config validation failed during reload");
        return None;
    }

    Some(load_schedules_from_config(&config))
}

/// Config file watcher service that monitors config.toml for changes.
/// Uses notify-debouncer-mini with 2 second debounce to avoid rapid reloads.
#[instrument(skip(cache))]
async fn run_config_watcher_service(
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
) {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
    use std::time::Duration as StdDuration;

    info!("Config watcher service started");

    // Create channel for receiving file change events
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);

    // Get absolute path to config file for watching
    let config_path = match std::fs::canonicalize(get_config_path()) {
        Ok(p) => p,
        Err(e) => {
            error!(error = ?e, "Failed to get absolute path for config.toml");
            return;
        }
    };

    // Spawn blocking task for the file watcher (notify is sync)
    let watcher_config_path = config_path.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let mut watcher_handle = tokio::task::spawn_blocking(move || {
        let tx = tx;
        let config_path = watcher_config_path;

        // Create debouncer with 2 second timeout
        let mut debouncer = match new_debouncer(
            StdDuration::from_secs(2),
            move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify_debouncer_mini::notify::Error>| {
                match res {
                    Ok(events) => {
                        for event in events {
                            debug!(path = ?event.path, "File change event received");
                            // Use blocking_send since we're in a sync context
                            if tx.blocking_send(()).is_err() {
                                // Channel closed, watcher should stop
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, "File watcher error");
                    }
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                error!(error = ?e, "Failed to create file watcher");
                return;
            }
        };

        // Watch the config file's parent directory
        let watch_path = config_path.parent().unwrap_or(Path::new("."));
        if let Err(e) = debouncer.watcher().watch(watch_path, RecursiveMode::NonRecursive) {
            error!(error = ?e, path = ?watch_path, "Failed to watch config directory");
            return;
        }

        info!(path = ?watch_path, "Watching for config changes");

        // Keep the watcher alive until shutdown is signaled
        let _ = shutdown_rx.blocking_recv();
    });

    // Main loop: handle file change events
    loop {
        tokio::select! {
            Some(()) = rx.recv() => {
                info!("Config file changed, reloading schedules");

                if let Some(schedules) = reload_schedules_from_config() {
                    let mut cache_guard = cache.write().await;
                    let old_count = cache_guard.schedules.len();
                    cache_guard.update(schedules);

                    info!(
                        old_count,
                        new_count = cache_guard.schedules.len(),
                        version = cache_guard.version,
                        "Schedules reloaded from config"
                    );
                } else {
                    warn!("Failed to reload config, keeping existing schedules");
                }
            }
            _ = &mut watcher_handle => {
                error!("File watcher task exited unexpectedly");
                break;
            }
        }
    }

    // Signal the watcher thread to exit cleanly
    drop(shutdown_tx);
}

/// Main entry point for the twitch-1337 bot.
///
/// Establishes a persistent Twitch IRC connection and runs multiple handlers in parallel:
/// - Daily 1337 tracker: monitors 13:37 messages, posts stats at 13:38
/// - Generic commands: handles !toggle-ping and other bot commands
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

    let config = load_configuration().await?;

    let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);

    let schedules_enabled = !config.schedules.is_empty();

    info!(
        local_time = ?local,
        utc_time = ?Utc::now(),
        channel = %config.twitch.channel,
        username = %config.twitch.username,
        schedules_enabled,
        schedule_count = config.schedules.len(),
        "Starting twitch-1337 bot"
    );

    ensure_data_dir().await?;

    // Setup, connect, join channel, and verify authentication (all in one step)
    let (incoming_messages, client) = setup_and_verify_twitch_client(&config.twitch).await?;

    // Wrap client in Arc for sharing across handlers
    let client = Arc::new(client);

    // Create broadcast channel for message distribution (capacity: 100 messages)
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    // Spawn message router task
    let router_handle = tokio::spawn(run_message_router(incoming_messages, broadcast_tx.clone()));

    // Optionally spawn config watcher service and scheduled message handler
    let (watcher_service, handler_scheduled_messages) = if schedules_enabled {
        info!(
            count = config.schedules.len(),
            "Schedules configured, starting scheduled message system"
        );

        // Load initial schedules from config
        let initial_schedules = load_schedules_from_config(&config);
        info!(
            loaded = initial_schedules.len(),
            "Loaded initial schedules from config"
        );

        // Create schedule cache for dynamic scheduled messages
        let mut cache = database::ScheduleCache::new();
        cache.update(initial_schedules);
        let schedule_cache = Arc::new(tokio::sync::RwLock::new(cache));

        // Spawn config watcher service
        let watcher = tokio::spawn({
            let cache = schedule_cache.clone();
            async move {
                run_config_watcher_service(cache).await;
            }
        });

        // Spawn scheduled message handler
        let handler = tokio::spawn({
            let client = client.clone();
            let cache = schedule_cache.clone();
            let channel = config.twitch.channel.clone();
            async move { run_scheduled_message_handler(client, cache, channel).await }
        });

        (Some(watcher), Some(handler))
    } else {
        info!("No schedules configured, scheduled messages disabled");
        (None, None)
    };

    // Create shared latency estimate, seeded from config
    let latency = Arc::new(AtomicU32::new(config.twitch.expected_latency));

    // Spawn latency monitor handler
    let handler_latency = tokio::spawn({
        let client = client.clone();
        let broadcast_tx = broadcast_tx.clone();
        let latency = latency.clone();
        async move {
            run_latency_handler(client, broadcast_tx, latency).await;
        }
    });

    // Spawn 1337 handler task
    let handler_1337 = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let channel = config.twitch.channel.clone();
        let latency = latency.clone();
        async move {
            run_1337_handler(broadcast_tx, client, channel, latency).await;
        }
    });

    let handler_generic_commands = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let se_config = config.streamelements.clone();
        let openrouter_config = config.openrouter.clone();
        async move { run_generic_command_handler(broadcast_tx, client, se_config, openrouter_config).await }
    });

    if schedules_enabled {
        info!(
            "Bot running with continuous connection. Handlers: Config watcher, 1337 tracker, Generic commands, Scheduled messages, Latency monitor"
        );
        info!("Scheduled messages: Loaded from config.toml, reloads on file change");
    } else {
        info!(
            "Bot running with continuous connection. Handlers: 1337 tracker, Generic commands, Latency monitor"
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
    match (watcher_service, handler_scheduled_messages) {
        (Some(watcher), Some(handler)) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutdown signal received, exiting gracefully");
                }
                result = router_handle => {
                    error!("Message router exited unexpectedly: {result:?}");
                }
                result = watcher => {
                    error!("Config watcher service exited unexpectedly: {result:?}");
                }
                result = handler_1337 => {
                    error!("1337 handler exited unexpectedly: {result:?}");
                }
                result = handler_generic_commands => {
                    error!("Generic Command Handler exited unexpectedly: {result:?}");
                }
                result = handler => {
                    error!("Scheduled message handler exited unexpectedly: {result:?}");
                }
                result = handler_latency => {
                    error!("Latency handler exited unexpectedly: {result:?}");
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
                result = handler_generic_commands => {
                    error!("Generic Command Handler exited unexpectedly: {result:?}");
                }
                result = handler_latency => {
                    error!("Latency handler exited unexpectedly: {result:?}");
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

#[instrument(skip(config))]
fn setup_twitch_client(config: &TwitchConfiguration) -> (UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient) {
    // Create authenticated IRC client with refreshing tokens
    let credentials = RefreshingLoginCredentials::init_with_username(
        Some(config.username.clone()),
        config.client_id.expose_secret().to_string(),
        config.client_secret.expose_secret().to_string(),
        FileBasedTokenStorage::new(config.refresh_token.clone()),
    );
    let twitch_config = ClientConfig::new_simple(credentials);
    TwitchIRCClient::<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>::new(
        twitch_config,
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
#[instrument(skip(config))]
async fn setup_and_verify_twitch_client(
    config: &TwitchConfiguration,
) -> Result<(UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient)> {
    info!("Setting up and verifying Twitch connection");

    let (mut incoming_messages, client) = setup_twitch_client(config);

    // Connect to Twitch IRC
    info!("Connecting to Twitch IRC");
    client.connect().await;

    // Join the configured channel
    info!(channel = %config.channel, "Joining channel");
    let channels = [config.channel.clone()].into();
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
#[instrument(skip(broadcast_tx, client, channel, latency))]
async fn run_1337_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    channel: String,
    latency: Arc<AtomicU32>,
) {
    info!("1337 handler started");

    loop {
        // Wait until 13:36 to start monitoring
        wait_until_schedule(TARGET_HOUR, TARGET_MINUTE - 1).await;

        info!("Starting daily 1337 monitoring session");

        // Load the all-time leaderboard
        let mut leaderboard = load_leaderboard().await;

        // Fresh HashMap for today's users (maps username -> ms_since_minute if sub-second)
        let total_users = Arc::new(Mutex::new(HashMap::with_capacity(MAX_USERS)));

        // Spawn message monitoring subtask
        let mut monitor_handle = tokio::spawn({
            let total_users = total_users.clone();
            // Subscribe fresh when we wake up - only see messages from now on
            let broadcast_rx = broadcast_tx.subscribe();

            async move {
                monitor_1337_messages(broadcast_rx, total_users).await;
            }
        });

        // Wait until 13:36:30 to send reminder
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30, latency.load(Ordering::Relaxed)).await;

        info!("Posting reminder to channel");
        if let Err(e) = client
            .say(channel.clone(), "PausersHype".to_string())
            .await
        {
            error!(error = ?e, "Failed to send reminder message");
        }

        // Wait until 13:38 to post stats
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0, latency.load(Ordering::Relaxed)).await;

        // Abort the monitor and wait for it to fully stop before reading results
        monitor_handle.abort();
        let _ = (&mut monitor_handle).await;

        // Get user list, count, and fastest
        let (count, user_list, fastest) = {
            let users = total_users.lock().await;
            let count = users.len();
            let mut user_vec: Vec<String> = users.keys().cloned().collect();
            user_vec.sort();

            let fastest: Option<(String, u64)> = users
                .iter()
                .filter_map(|(name, ms)| ms.map(|ms| (name.clone(), ms)))
                .min_by_key(|(_, ms)| *ms);

            (count, user_vec, fastest)
        };

        let mut message = generate_stats_message(count, &user_list);

        if let Some((ref fastest_user, fastest_ms)) = fastest {
            // Check if this is a new all-time record BEFORE updating the leaderboard
            let previous_best = leaderboard
                .values()
                .map(|pb| pb.ms)
                .min();
            let is_record = match previous_best {
                Some(best) => fastest_ms < best,
                None => true, // First ever sub-1s time
            };

            message.push_str(&format!(
                " | {fastest_user} war mass schnellste mit {fastest_ms}ms"
            ));
            if is_record {
                message.push_str(" - neuer Rekord!");
            }
        }

        // Update leaderboard with today's sub-1s times
        let today = Utc::now()
            .with_timezone(&chrono_tz::Europe::Berlin)
            .date_naive();
        {
            let users = total_users.lock().await;
            for (username, timing) in users.iter() {
                if let Some(ms) = timing {
                    let update = match leaderboard.get(username) {
                        Some(existing) => *ms < existing.ms,
                        None => true,
                    };
                    if update {
                        leaderboard.insert(
                            username.clone(),
                            PersonalBest {
                                ms: *ms,
                                date: today,
                            },
                        );
                    }
                }
            }
        }
        save_leaderboard(&leaderboard).await;

        // Post stats message
        info!(count = count, "Posting stats to channel");
        if let Err(e) = client.say(channel.clone(), message).await {
            error!(error = ?e, count = count, "Failed to send stats message");
        } else {
            info!("Stats posted successfully");
        }

        info!("Daily 1337 session completed, waiting for next day");
    }
}


/// Periodically measures IRC latency via PING/PONG and updates a shared EMA estimate.
///
/// Sends a PING with a unique nonce every 5 minutes, measures the round-trip time
/// from the matching PONG response, and updates an exponential moving average (EMA)
/// of the one-way latency. The EMA is stored in a shared `AtomicU32` that other
/// handlers (e.g., the 1337 handler) read for timing adjustments.
///
/// The handler is fully independent — PING failures or PONG timeouts are logged
/// but never crash the handler or affect the EMA.
#[instrument(skip(client, broadcast_tx, latency))]
async fn run_latency_handler(
    client: Arc<AuthenticatedTwitchClient>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
    latency: Arc<AtomicU32>,
) {
    let initial = latency.load(Ordering::Relaxed);
    info!(initial_latency_ms = initial, "Latency handler started");

    let mut ema: f64 = initial as f64;
    let mut last_logged_ema: u32 = initial;

    loop {
        sleep(LATENCY_PING_INTERVAL).await;

        let nonce = format!("{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));

        // Subscribe before sending so we don't miss the PONG on fast connections
        let mut broadcast_rx = broadcast_tx.subscribe();

        // Send PING with unique nonce
        let send_time = tokio::time::Instant::now();
        if let Err(e) = client.send_message(irc!["PING", nonce.clone()]).await {
            warn!(error = ?e, "Failed to send PING");
            continue;
        }
        let pong_result = tokio::time::timeout(LATENCY_PING_TIMEOUT, async {
            loop {
                match broadcast_rx.recv().await {
                    Ok(ServerMessage::Pong(PongMessage { source, .. })) => {
                        if source.params.get(1).map(String::as_str) == Some(nonce.as_str()) {
                            return send_time.elapsed();
                        }
                        // Nonce mismatch — likely library keepalive, ignore
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("Broadcast channel closed during PONG wait");
                        return send_time.elapsed();
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "Latency handler lagged during PONG wait");
                        continue;
                    }
                    _ => continue,
                }
            }
        })
        .await;

        let rtt = match pong_result {
            Ok(elapsed) => elapsed,
            Err(_) => {
                warn!("PONG timeout after {:?}", LATENCY_PING_TIMEOUT);
                continue;
            }
        };

        let one_way_ms = rtt.as_millis() as f64 / 2.0;
        ema = LATENCY_EMA_ALPHA * one_way_ms + (1.0 - LATENCY_EMA_ALPHA) * ema;
        let ema_rounded = ema.round() as u32;

        latency.store(ema_rounded, Ordering::Relaxed);

        debug!(
            rtt_ms = rtt.as_millis() as u64,
            one_way_ms = one_way_ms as u64,
            ema_ms = ema_rounded,
            "Latency measurement"
        );

        // Log at info level only when EMA shifts significantly
        if ema_rounded.abs_diff(last_logged_ema) >= LATENCY_LOG_THRESHOLD {
            info!(
                previous_ms = last_logged_ema,
                current_ms = ema_rounded,
                "Latency EMA changed"
            );
            last_logged_ema = ema_rounded;
        }
    }
}

/// Handler for generic text commands that start with `!`.
///
/// Monitors chat for commands and dispatches them to appropriate handlers.
/// Currently supports:
/// - `!toggle-ping <command>` - Adds/removes user from StreamElements ping command
/// - `!ai <instruction>` - AI-powered responses (if OpenRouter configured)
///
/// Runs continuously in a loop, processing all incoming messages.
#[instrument(skip(broadcast_tx, client, se_config, openrouter_config))]
async fn run_generic_command_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    se_config: StreamelementsConfig,
    openrouter_config: Option<OpenRouterConfig>,
) {
    info!("Generic Command Handler started");

    // Subscribe to the broadcast channel
    let broadcast_rx = broadcast_tx.subscribe();

    // Initialize StreamElements client
    let se_client = match SEClient::new(se_config.api_token.expose_secret()) {
        Ok(client) => client,
        Err(e) => {
            error!(error = ?e, "Failed to initialize StreamElements client");
            error!("Generic Command Handler cannot start without valid StreamElements API token");
            return;
        }
    };

    // Initialize OpenRouter client (optional)
    let openrouter_client = if let Some(ref openrouter_cfg) = openrouter_config {
        match OpenRouterClient::new(
            openrouter_cfg.api_key.expose_secret(),
            &openrouter_cfg.model,
        ) {
            Ok(client) => {
                info!(model = %openrouter_cfg.model, "OpenRouter AI command enabled");
                Some(client)
            }
            Err(e) => {
                error!(error = ?e, "Failed to initialize OpenRouter client");
                error!("AI command will be disabled");
                None
            }
        }
    } else {
        debug!("OpenRouter not configured, AI command disabled");
        None
    };

    // Initialize aviation client for !up command
    let aviation_client = match aviation::AviationClient::new() {
        Ok(client) => {
            info!("Aviation client initialized for !up command");
            client
        }
        Err(e) => {
            error!(error = ?e, "Failed to initialize aviation client");
            error!("!up command will be disabled");
            return;
        }
    };

    run_generic_command_handler_inner(
        broadcast_rx,
        client,
        se_client,
        se_config.channel_id,
        openrouter_client,
        aviation_client,
    )
    .await;
}

/// Inner loop for the generic command handler.
#[instrument(skip(broadcast_rx, client, se_client, openrouter_client, aviation_client))]
async fn run_generic_command_handler_inner(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    se_client: SEClient,
    channel_id: String,
    openrouter_client: Option<OpenRouterClient>,
    aviation_client: aviation::AviationClient,
) {
    // Cooldown tracking for AI command
    let ai_cooldowns: Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Cooldown tracking for !up command
    let up_cooldowns: Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                // Catch any errors from command handling to prevent task crash
                if let Err(e) = handle_generic_commands(
                    &privmsg,
                    &client,
                    &se_client,
                    &channel_id,
                    openrouter_client.as_ref(),
                    &ai_cooldowns,
                    &aviation_client,
                    &up_cooldowns,
                )
                .await
                {
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
#[allow(clippy::too_many_arguments)]
#[instrument(skip(privmsg, client, se_client, channel_id, openrouter_client, ai_cooldowns, aviation_client, up_cooldowns))]
async fn handle_generic_commands(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    openrouter_client: Option<&OpenRouterClient>,
    ai_cooldowns: &Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    aviation_client: &aviation::AviationClient,
    up_cooldowns: &Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>>,
) -> Result<()> {
    let mut words = privmsg.message_text.split_whitespace();
    let Some(first_word) = words.next() else {
        return Ok(());
    };

    if first_word == "!toggle-ping" {
        toggle_ping_command(privmsg, client, se_client, channel_id, words.next()).await?;
    } else if first_word == "!list-pings" {
        list_pings_command(privmsg, client, se_client, channel_id, words.next()).await?;
    } else if first_word == "!ai" {
        // Check if AI is enabled
        if let Some(openrouter) = openrouter_client {
            // Collect remaining words as the instruction
            let instruction: String = words.collect::<Vec<_>>().join(" ");
            ai_command(privmsg, client, openrouter, ai_cooldowns, &instruction).await?;
        } else {
            // AI not configured - silently ignore or could add a message
            debug!("AI command received but OpenRouter not configured");
        }
    } else if first_word == "!fl" {
        flight_command(privmsg, client, words.next(), words.next()).await?;
    } else if first_word == "!up" {
        let input: String = words.collect::<Vec<_>>().join(" ");
        aviation::up_command(privmsg, client, aviation_client, &input, up_cooldowns).await?;
    }

    Ok(())
}

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
/// Ping commands are used to notify community members about game sessions.
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
#[instrument(skip(privmsg, client, se_client, channel_id))]
async fn toggle_ping_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
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
        .get_all_commands(channel_id)
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
        if let Some(at_pos) = command.reply.find('@') {
            // Insert after first @username token
            let after_at = &command.reply[at_pos..];
            let token_end = after_at.find(' ').unwrap_or(after_at.len());
            let insert_pos = at_pos + token_end;
            let (head, tail) = command.reply.split_at(insert_pos);
            format!("{head} @{}{tail}", privmsg.sender.name)
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
        .update_command(channel_id, command)
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

#[instrument(skip(privmsg, client, se_client, channel_id))]
async fn list_pings_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    enabled_option: Option<&str>,
) -> Result<()> {
    let filter = enabled_option.unwrap_or("enabled");

    let commands = se_client
        .get_all_commands(channel_id)
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

/// Cooldown duration for the AI command (30 seconds).
const AI_COMMAND_COOLDOWN: Duration = Duration::from_secs(30);

/// Handles the `!ai` command for AI-powered responses.
///
/// Takes user instructions and processes them with OpenRouter.
///
/// # Command Format
///
/// `!ai <instruction>`
///
/// # Rate Limiting
///
/// Per-user cooldown of 30 seconds to prevent spam.
///
/// # Errors
///
/// Returns an error if the OpenRouter API call fails.
#[instrument(skip(privmsg, client, openrouter_client, cooldowns))]
async fn ai_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    openrouter_client: &OpenRouterClient,
    cooldowns: &Arc<Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    instruction: &str,
) -> Result<()> {
    let user = &privmsg.sender.login;

    // Check cooldown
    {
        let cooldowns_guard = cooldowns.lock().await;
        if let Some(last_use) = cooldowns_guard.get(user) {
            let elapsed = last_use.elapsed();
            if elapsed < AI_COMMAND_COOLDOWN {
                let remaining = AI_COMMAND_COOLDOWN - elapsed;
                debug!(
                    user = %user,
                    remaining_secs = remaining.as_secs(),
                    "AI command on cooldown"
                );
                if let Err(e) = client
                    .say_in_reply_to(privmsg, "Bitte warte noch ein bisschen Waiting".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send cooldown message");
                }
                return Ok(());
            }
        }
    }

    // Check for empty instruction
    if instruction.trim().is_empty() {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Benutzung: !ai <anweisung>".to_string())
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    }

    debug!(user = %user, instruction = %instruction, "Processing AI command");

    // Update cooldown before making the API call
    {
        let mut cooldowns_guard = cooldowns.lock().await;
        cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
    }

    // Execute AI with timeout
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        execute_ai_request(instruction, openrouter_client),
    )
    .await;

    let response = match result {
        Ok(Ok(text)) => {
            // Truncate response for Twitch chat
            truncate_response(&text, MAX_RESPONSE_LENGTH)
        }
        Ok(Err(e)) => {
            error!(error = ?e, "AI execution failed");
            "Da ist was schiefgelaufen FDM".to_string()
        }
        Err(_) => {
            error!("AI execution timed out");
            "Das hat zu lange gedauert Waiting".to_string()
        }
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response).await {
        error!(error = ?e, "Failed to send AI response");
    }

    Ok(())
}

#[instrument(skip(privmsg, client), fields(user = %privmsg.sender.login))]
async fn flight_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    aircraft_code: Option<&str>,
    duration_str: Option<&str>,
) -> Result<()> {
    const USAGE_MSG: &str = "Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM";

    let (Some(aircraft_code), Some(duration_str)) = (aircraft_code, duration_str) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from(USAGE_MSG))
            .await
        {
            error!(error = ?e, "Failed to send flight usage message");
        }
        return Ok(());
    };

    let Some(aircraft) = random_flight::aircraft_by_icao_type(aircraft_code) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das Flugzeug kenn ich nich FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'unknown aircraft' error message");
        }
        return Ok(());
    };

    let Some(duration) = parse_flight_duration(duration_str) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from(USAGE_MSG))
            .await
        {
            error!(error = ?e, "Failed to send flight duration usage message");
        }
        return Ok(());
    };

    // Can take many retries internally
    let result = tokio::task::spawn_blocking(move || {
        random_flight::generate_flight_plan(aircraft, duration, None)
    })
    .await
    .wrap_err("Flight plan generation task panicked")?;

    let fp = match result {
        Ok(fp) => fp,
        Err(e) => {
            warn!(error = ?e, "Flight plan generation failed");
            if let Err(e) = client
                .say_in_reply_to(
                    privmsg,
                    String::from("Hab keine Route gefunden, versuch mal ne andere Zeit FDM"),
                )
                .await
            {
                error!(error = ?e, "Failed to send 'no route found' error message");
            }
            return Ok(());
        }
    };

    // Format: "1h12m" or "45m"
    let total_mins = fp.block_time.as_secs() / 60;
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    let time_str = if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    };

    let response = format!(
        "{} → {} | {:.0} nm | {} | FL{} | {}",
        fp.departure.icao,
        fp.arrival.icao,
        fp.distance_nm,
        time_str,
        fp.cruise_altitude_ft / 100,
        fp.simbrief_url(),
    );

    client.say_in_reply_to(privmsg, response).await?;

    Ok(())
}

/// Run a single schedule task.
/// This task will run the schedule at its configured interval,
/// checking if it's still active before each post.
#[instrument(skip(client, cache, channel), fields(schedule = %schedule.name))]
async fn run_schedule_task(
    schedule: database::Schedule,
    client: Arc<AuthenticatedTwitchClient>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
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
            .say(channel.clone(), schedule.message.clone())
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
#[instrument(skip(client, cache, channel))]
async fn run_scheduled_message_handler(
    client: Arc<AuthenticatedTwitchClient>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
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
                let channel = channel.clone();
                running_tasks.entry(name.clone()).or_insert_with(|| {
                    info!(schedule = %name, "Starting task for new schedule");

                    tokio::spawn(run_schedule_task(
                        schedule.clone(),
                        client.clone(),
                        cache.clone(),
                        channel,
                    ))
                });
            }

            info!(active_tasks = running_tasks.len(), "Task update complete");
        }
    }
}


