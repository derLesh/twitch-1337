use std::{
    collections::{HashMap, HashSet, VecDeque},
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
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
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
mod cooldown;
mod database;
mod flight_tracker;
mod llm;
mod memory;
mod ping;
mod prefill;

/// Type alias for the authenticated Twitch IRC client
pub(crate) type AuthenticatedTwitchClient =
    TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>;

/// Type alias for the shared chat history buffer (username, message text).
pub(crate) type ChatHistory = Arc<tokio::sync::Mutex<VecDeque<(String, String)>>>;

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

pub(crate) static APP_USER_AGENT: &str =
    concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

fn default_expected_latency() -> u32 {
    100
}

#[derive(Debug, Clone, Deserialize)]
struct TwitchConfiguration {
    channel: String,
    username: String,
    refresh_token: SecretString,
    client_id: SecretString,
    client_secret: SecretString,
    #[serde(default = "default_expected_latency")]
    expected_latency: u32,
    #[serde(default)]
    hidden_admins: Vec<String>,
    #[serde(default)]
    admin_channel: Option<String>,
}

/// Which LLM backend to use.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AiBackend {
    OpenAi,
    Ollama,
}

#[derive(Debug, Clone, Deserialize)]
struct AiConfig {
    /// Backend type: "openai" or "ollama"
    backend: AiBackend,
    /// API key (required for openai, not used for ollama)
    #[serde(default)]
    api_key: Option<SecretString>,
    /// Base URL for the API (optional, has per-backend defaults)
    #[serde(default)]
    base_url: Option<String>,
    /// Model name to use
    model: String,
    /// System prompt sent to the model
    #[serde(default = "default_system_prompt")]
    system_prompt: String,
    /// Template for the user message. Use `{message}` and `{chat_history}` as placeholders.
    #[serde(default = "default_instruction_template")]
    instruction_template: String,
    /// Timeout for AI requests in seconds (default: 30)
    #[serde(default = "default_ai_timeout")]
    timeout: u64,
    /// Number of recent chat messages to include as context (0 = disabled, max 100)
    #[serde(default)]
    history_length: u64,
    /// Optional: Prefill chat history from a rustlog-compatible API at startup
    #[serde(default)]
    history_prefill: Option<prefill::HistoryPrefillConfig>,
    /// Enable persistent AI memory (default: false)
    #[serde(default)]
    memory_enabled: bool,
    /// Maximum number of stored memories (default: 50)
    #[serde(default = "default_max_memories")]
    max_memories: usize,
}

fn default_system_prompt() -> String {
    "You are a helpful Twitch chat bot assistant. Keep responses brief (2-3 sentences max) since they'll appear in chat. Be friendly and casual. Respond in the same language the user writes in (German or English).".to_string()
}

fn default_instruction_template() -> String {
    "{chat_history}\n{message}".to_string()
}

fn default_ai_timeout() -> u64 {
    30
}

fn default_max_memories() -> usize {
    50
}

/// Configuration for a scheduled message loaded from config.toml.
#[derive(Debug, Clone, Deserialize)]
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

fn default_cooldown() -> u64 {
    300
}

fn default_ai_cooldown() -> u64 {
    30
}

fn default_up_cooldown() -> u64 {
    30
}

fn default_feedback_cooldown() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
struct CooldownsConfig {
    #[serde(default = "default_ai_cooldown")]
    ai: u64,
    #[serde(default = "default_up_cooldown")]
    up: u64,
    #[serde(default = "default_feedback_cooldown")]
    feedback: u64,
}

impl Default for CooldownsConfig {
    fn default() -> Self {
        Self {
            ai: default_ai_cooldown(),
            up: default_up_cooldown(),
            feedback: default_feedback_cooldown(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PingsConfig {
    #[serde(default = "default_cooldown")]
    cooldown: u64,
    #[serde(default)]
    public: bool,
}

impl Default for PingsConfig {
    fn default() -> Self {
        Self {
            cooldown: default_cooldown(),
            public: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Configuration {
    twitch: TwitchConfiguration,
    #[serde(default)]
    pings: PingsConfig,
    #[serde(default)]
    cooldowns: CooldownsConfig,
    #[serde(default)]
    ai: Option<AiConfig>,
    #[serde(default)]
    schedules: Vec<ScheduleConfig>,
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
            bail!(
                "twitch.expected_latency must be <= 1000ms (got {})",
                self.twitch.expected_latency
            );
        }

        if let Some(ref admin_ch) = self.twitch.admin_channel {
            if admin_ch.trim().is_empty() {
                bail!("twitch.admin_channel cannot be empty when specified");
            }
            if admin_ch == &self.twitch.channel {
                bail!("twitch.admin_channel must be different from twitch.channel");
            }
        }

        // Validate AI config
        if let Some(ref ai) = self.ai
            && matches!(ai.backend, AiBackend::OpenAi)
            && ai.api_key.is_none()
        {
            bail!("AI backend 'openai' requires an api_key");
        }

        if let Some(ref ai) = self.ai
            && ai.history_length > 100
        {
            bail!(
                "ai.history_length must be <= 100 (got {})",
                ai.history_length
            );
        }

        if let Some(ref ai) = self.ai
            && let Some(ref prefill) = ai.history_prefill
        {
            if prefill.base_url.trim().is_empty() {
                bail!("ai.history_prefill.base_url cannot be empty");
            }
            if !(0.0..=1.0).contains(&prefill.threshold) {
                bail!(
                    "ai.history_prefill.threshold must be between 0.0 and 1.0 (got {})",
                    prefill.threshold
                );
            }
        }

        if let Some(ref ai) = self.ai
            && ai.history_prefill.is_some()
            && ai.history_length == 0
        {
            bail!("ai.history_prefill requires history_length > 0");
        }

        if let Some(ref ai) = self.ai
            && ai.memory_enabled
            && !(1..=200).contains(&ai.max_memories)
        {
            bail!(
                "ai.max_memories must be between 1 and 200 (got {})",
                ai.max_memories
            );
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
            database::Schedule::parse_interval(&schedule.interval).wrap_err_with(|| {
                format!("Schedule '{}' has invalid interval format", schedule.name)
            })?;
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

    let wait_duration = (time.with_timezone(&Utc)
        - Utc::now()
        - TimeDelta::milliseconds(i64::from(expected_latency)))
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
        .clone(),
        2..=3 if !user_list.iter().any(|u| u == "gargoyletec") => {
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
        .clone(),
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
pub(crate) fn parse_flight_duration(s: &str) -> Option<std::time::Duration> {
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

/// Maximum response length for Twitch chat (to stay within limits).
pub(crate) const MAX_RESPONSE_LENGTH: usize = 500;

/// Truncates a string to the maximum number of characters at a word boundary.
pub(crate) fn truncate_response(text: &str, max_chars: usize) -> String {
    // Collapse whitespace and newlines
    let collapsed: String = text.split_whitespace().fold(String::new(), |mut acc, w| {
        if !acc.is_empty() {
            acc.push(' ');
        }
        acc.push_str(w);
        acc
    });

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
                    if users.len() >= MAX_USERS {
                        error!(max = MAX_USERS, "User limit reached");
                    } else if !users.contains_key(privmsg.sender.login.as_str()) {
                        let username = privmsg.sender.login.clone();
                        let ms_since_minute = u64::from(local.second()) * 1000
                            + u64::from(local.timestamp_subsec_millis());
                        if ms_since_minute < 1000 {
                            debug!(user = %username, ms = ms_since_minute, "User said 1337 at 13:37 (sub-second)");
                            users.insert(username, Some(ms_since_minute));
                        } else {
                            debug!(user = %username, "User said 1337 at 13:37");
                            users.insert(username, None);
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "1337 handler lagged, skipped messages");
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
pub(crate) struct PersonalBest {
    /// Milliseconds after 13:37:00.000 (0-999)
    pub(crate) ms: u64,
    /// The date (Europe/Berlin) when this record was set
    pub(crate) date: chrono::NaiveDate,
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

/// Saves the all-time leaderboard to disk using atomic write+rename.
///
/// Logs an error and continues if the write fails.
async fn save_leaderboard(leaderboard: &HashMap<String, PersonalBest>) {
    let path = get_data_dir().join(LEADERBOARD_FILENAME);
    let tmp_path = path.with_extension("ron.tmp");
    match ron::to_string(leaderboard) {
        Ok(serialized) => {
            if let Err(e) = fs::write(&tmp_path, serialized.as_bytes()).await {
                error!(error = ?e, "Failed to write leaderboard tmp file");
            } else if let Err(e) = fs::rename(&tmp_path, &path).await {
                error!(error = ?e, "Failed to rename leaderboard file");
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
            Err(e) => Err(eyre::Report::from(e).wrap_err("Failed to read token file")),
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

    let config: Configuration =
        toml::from_str(&data).wrap_err("Failed to parse config.toml - check for syntax errors")?;

    config.validate()?;

    if config.pings.public {
        info!("Pings can be triggered by anyone");
    } else {
        info!("Pings can only be triggered by members");
    }

    Ok(config)
}

/// Parse a datetime string in ISO 8601 format (YYYY-MM-DDTHH:MM:SS).
fn parse_datetime(s: &str) -> Result<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").wrap_err_with(|| {
        format!(
            "Invalid datetime format '{}' (expected YYYY-MM-DDTHH:MM:SS)",
            s
        )
    })
}

/// Parse a time string in HH:MM format.
fn parse_time(s: &str) -> Result<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M")
        .wrap_err_with(|| format!("Invalid time format '{}' (expected HH:MM)", s))
}

/// Convert a ScheduleConfig from config.toml into a database::Schedule.
fn schedule_config_to_schedule(config: &ScheduleConfig) -> Result<database::Schedule> {
    let interval = database::Schedule::parse_interval(&config.interval)?;

    let start_date = config
        .start_date
        .as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let end_date = config
        .end_date
        .as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let active_time_start = config
        .active_time_start
        .as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let active_time_end = config
        .active_time_end
        .as_ref()
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
async fn run_config_watcher_service(cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>) {
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
    let mut watcher_handle = tokio::task::spawn_blocking(move || {
        let tx = tx;
        let config_path = watcher_config_path;

        // Create debouncer with 2 second timeout
        let mut debouncer = match new_debouncer(
            StdDuration::from_secs(2),
            move |res: Result<
                Vec<notify_debouncer_mini::DebouncedEvent>,
                notify_debouncer_mini::notify::Error,
            >| {
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
        if let Err(e) = debouncer
            .watcher()
            .watch(watch_path, RecursiveMode::NonRecursive)
        {
            error!(error = ?e, path = ?watch_path, "Failed to watch config directory");
            return;
        }

        info!(path = ?watch_path, "Watching for config changes");

        // Park the thread so the debouncer stays alive; the thread terminates
        // with the process or when this handle's future is dropped on shutdown.
        loop {
            std::thread::park();
        }
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
}

/// Main entry point for the twitch-1337 bot.
///
/// Establishes a persistent Twitch IRC connection and runs multiple handlers in parallel:
/// - Daily 1337 tracker: monitors 13:37 messages, posts stats at 13:38
/// - Generic commands: handles !p, !up, !fl, !ai, and ping triggers
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
    let leaderboard = Arc::new(tokio::sync::RwLock::new(load_leaderboard().await));

    // Setup, connect, join channel, and verify authentication (all in one step)
    let (incoming_messages, client) = setup_and_verify_twitch_client(&config.twitch).await?;

    // Wrap client in Arc for sharing across handlers
    let client = Arc::new(client);

    let shared_aviation_client = match aviation::AviationClient::new() {
        Ok(client) => Some(client),
        Err(e) => {
            error!(
                error = ?e,
                "Failed to initialize aviation client; aviation commands and flight tracker disabled"
            );
            None
        }
    };

    // Placeholder pending task keeps the tokio::select! arm shape uniform; see shutdown below.
    let (tracker_tx, handler_flight_tracker) = match shared_aviation_client.clone() {
        Some(av) => {
            let (tx, rx) = tokio::sync::mpsc::channel::<flight_tracker::TrackerCommand>(32);
            let handle = tokio::spawn({
                let client = client.clone();
                let channel = config.twitch.channel.clone();
                let data_dir = get_data_dir();
                async move {
                    flight_tracker::run_flight_tracker(rx, client, channel, av, data_dir).await;
                }
            });
            (Some(tx), handle)
        }
        None => (None, tokio::spawn(std::future::pending::<()>())),
    };

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
        let leaderboard = leaderboard.clone();
        async move {
            run_1337_handler(broadcast_tx, client, channel, latency, leaderboard).await;
        }
    });

    let ping_manager = Arc::new(tokio::sync::RwLock::new(
        ping::PingManager::load(&get_data_dir()).wrap_err("Failed to load ping manager")?,
    ));

    let handler_generic_commands = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let ai_config = config.ai.clone();
        let leaderboard = leaderboard.clone();
        let ping_manager = ping_manager.clone();
        let hidden_admin_ids = config.twitch.hidden_admins.clone();
        let default_cooldown = Duration::from_secs(config.pings.cooldown);
        let pings_public = config.pings.public;
        let cooldowns = config.cooldowns.clone();
        let tracker_tx = tracker_tx.clone();
        let aviation_client = shared_aviation_client;
        let admin_channel = config.twitch.admin_channel.clone();
        let bot_username = config.twitch.username.clone();
        let channel = config.twitch.channel.clone();
        async move {
            run_generic_command_handler(CommandHandlerConfig {
                broadcast_tx,
                client,
                ai_config,
                leaderboard,
                ping_manager,
                hidden_admin_ids,
                default_cooldown,
                pings_public,
                cooldowns,
                tracker_tx,
                aviation_client,
                admin_channel,
                bot_username,
                channel,
            })
            .await;
        }
    });

    if schedules_enabled {
        info!(
            "Bot running with continuous connection. Handlers: Config watcher, 1337 tracker, Generic commands, Scheduled messages, Latency monitor, Flight tracker"
        );
        info!("Scheduled messages: Loaded from config.toml, reloads on file change");
    } else {
        info!(
            "Bot running with continuous connection. Handlers: 1337 tracker, Generic commands, Latency monitor, Flight tracker"
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
                result = handler_flight_tracker => {
                    error!("Flight tracker exited unexpectedly: {result:?}");
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
                result = handler_flight_tracker => {
                    error!("Flight tracker exited unexpectedly: {result:?}");
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
fn setup_twitch_client(
    config: &TwitchConfiguration,
) -> (UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient) {
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

    // Join the configured channel(s)
    let mut channels: HashSet<String> = [config.channel.clone()].into();
    if let Some(ref admin_channel) = config.admin_channel {
        info!(admin_channel = %admin_channel, "Joining admin channel");
        channels.insert(admin_channel.clone());
    }
    info!(channel = %config.channel, "Joining channel");
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
                _ => {}
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
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
) {
    info!("1337 handler started");

    loop {
        // Wait until 13:36 to start monitoring
        wait_until_schedule(TARGET_HOUR, TARGET_MINUTE - 1).await;

        info!("Starting daily 1337 monitoring session");

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
        sleep_until_hms(
            TARGET_HOUR,
            TARGET_MINUTE - 1,
            30,
            latency.load(Ordering::Relaxed),
        )
        .await;

        info!("Posting reminder to channel");
        if let Err(e) = client.say(channel.clone(), "PausersHype".to_string()).await {
            error!(error = ?e, "Failed to send reminder message");
        }

        // Wait until 13:38 to post stats
        sleep_until_hms(
            TARGET_HOUR,
            TARGET_MINUTE + 1,
            0,
            latency.load(Ordering::Relaxed),
        )
        .await;

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
            let leaderboard_guard = leaderboard.read().await;
            let previous_pb = leaderboard_guard.get(fastest_user).map(|pb| pb.ms);
            let is_record = previous_pb.is_none_or(|best| fastest_ms < best);
            drop(leaderboard_guard);

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
            let mut leaderboard_guard = leaderboard.write().await;
            for (username, timing) in users.iter() {
                if let Some(ms) = timing {
                    let update = match leaderboard_guard.get(username) {
                        Some(existing) => *ms < existing.ms,
                        None => true,
                    };
                    if update {
                        leaderboard_guard.insert(
                            username.clone(),
                            PersonalBest {
                                ms: *ms,
                                date: today,
                            },
                        );
                    }
                }
            }
            save_leaderboard(&leaderboard_guard).await;
        }

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

    let mut ema: f64 = f64::from(initial);
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
                    // Pongs with mismatched nonces (library keepalives) fall
                    // through the guard to the wildcard arm and keep waiting.
                    Ok(ServerMessage::Pong(PongMessage { source, .. }))
                        if source.params.get(1).map(String::as_str) == Some(nonce.as_str()) =>
                    {
                        return send_time.elapsed();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("Broadcast channel closed during PONG wait");
                        return send_time.elapsed();
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "Latency handler lagged during PONG wait");
                    }
                    _ => {}
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

struct CommandHandlerConfig {
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    ai_config: Option<AiConfig>,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    ping_manager: Arc<tokio::sync::RwLock<ping::PingManager>>,
    hidden_admin_ids: Vec<String>,
    default_cooldown: Duration,
    pings_public: bool,
    cooldowns: CooldownsConfig,
    tracker_tx: Option<tokio::sync::mpsc::Sender<flight_tracker::TrackerCommand>>,
    aviation_client: Option<aviation::AviationClient>,
    admin_channel: Option<String>,
    bot_username: String,
    channel: String,
}

/// Handler for generic text commands that start with `!`.
#[instrument(skip(cfg))]
async fn run_generic_command_handler(cfg: CommandHandlerConfig) {
    info!("Generic Command Handler started");

    let CommandHandlerConfig {
        broadcast_tx,
        client,
        ai_config,
        leaderboard,
        ping_manager,
        hidden_admin_ids,
        default_cooldown,
        pings_public,
        cooldowns,
        tracker_tx,
        aviation_client,
        admin_channel,
        bot_username,
        channel,
    } = cfg;

    // Subscribe to the broadcast channel
    let broadcast_rx = broadcast_tx.subscribe();

    // Extract history_length before ai_config is consumed
    let history_length = ai_config.as_ref().map_or(0, |c| c.history_length) as usize;
    let prefill_config = ai_config.as_ref().and_then(|c| c.history_prefill.clone());

    // Initialize LLM client (optional)
    let llm_client: Option<(Arc<dyn llm::LlmClient>, AiConfig)> = if let Some(ai_cfg) = ai_config {
        let client_result = match ai_cfg.backend {
            AiBackend::OpenAi => {
                let api_key = ai_cfg
                    .api_key
                    .as_ref()
                    .expect("validated: openai backend has api_key");
                llm::openai::OpenAiClient::new(
                    api_key.expose_secret(),
                    &ai_cfg.model,
                    ai_cfg.base_url.as_deref(),
                )
                .map(|c| Arc::new(c) as Arc<dyn llm::LlmClient>)
            }
            AiBackend::Ollama => {
                llm::ollama::OllamaClient::new(&ai_cfg.model, ai_cfg.base_url.as_deref())
                    .map(|c| Arc::new(c) as Arc<dyn llm::LlmClient>)
            }
        };
        match client_result {
            Ok(client) => {
                info!(backend = ?ai_cfg.backend, model = %ai_cfg.model, "AI command enabled");
                Some((client, ai_cfg))
            }
            Err(e) => {
                error!(error = ?e, "Failed to initialize LLM client, AI command disabled");
                None
            }
        }
    } else {
        debug!("AI not configured, AI command disabled");
        None
    };

    // Create chat history buffer for AI context (if history_length > 0)
    let chat_history: Option<ChatHistory> = if history_length > 0 {
        let buf = if let Some(ref prefill_cfg) = prefill_config {
            prefill::prefill_chat_history(&channel, history_length, prefill_cfg).await
        } else {
            VecDeque::with_capacity(history_length)
        };
        Some(Arc::new(tokio::sync::Mutex::new(buf)))
    } else {
        None
    };

    let data_dir = get_data_dir();

    let mut commands: Vec<Box<dyn commands::Command>> = vec![
        Box::new(commands::ping_admin::PingAdminCommand::new(
            ping_manager.clone(),
            hidden_admin_ids,
        )),
        Box::new(commands::random_flight::RandomFlightCommand),
        Box::new(commands::flights_above::FlightsAboveCommand::new(
            aviation_client,
            Duration::from_secs(cooldowns.up),
        )),
        Box::new(commands::leaderboard::LeaderboardCommand::new(leaderboard)),
        Box::new(commands::feedback::FeedbackCommand::new(
            data_dir.clone(),
            Duration::from_secs(cooldowns.feedback),
        )),
    ];

    if let Some(tx) = tracker_tx {
        commands.push(Box::new(commands::track::TrackCommand::new(tx.clone())));
        commands.push(Box::new(commands::untrack::UntrackCommand::new(tx.clone())));
        commands.push(Box::new(commands::flights::FlightsCommand::new(tx.clone())));
        commands.push(Box::new(commands::flights::FlightCommand::new(tx)));
    }

    if let Some((client, cfg)) = llm_client {
        // Load memory store if enabled
        let memory_config = if cfg.memory_enabled {
            match memory::MemoryStore::load(&data_dir) {
                Ok((store, path)) => Some(memory::MemoryConfig {
                    store: Arc::new(tokio::sync::RwLock::new(store)),
                    path,
                    max_memories: cfg.max_memories,
                }),
                Err(e) => {
                    error!(error = ?e, "Failed to load AI memory store, memory disabled");
                    None
                }
            }
        } else {
            None
        };

        let chat_ctx = chat_history
            .clone()
            .map(|history| commands::ai::ChatContext {
                history,
                history_length,
                bot_username: bot_username.clone(),
            });

        commands.push(Box::new(commands::ai::AiCommand::new(
            client,
            cfg.model,
            commands::ai::AiPrompts {
                system: cfg.system_prompt,
                instruction_template: cfg.instruction_template,
            },
            Duration::from_secs(cfg.timeout),
            Duration::from_secs(cooldowns.ai),
            chat_ctx,
            memory_config,
        )));
    }

    // PingTriggerCommand must be last: it matches any !<name> that is a registered ping,
    // so built-in commands earlier in the list take priority and can't be shadowed.
    commands.push(Box::new(commands::ping_trigger::PingTriggerCommand::new(
        ping_manager,
        default_cooldown,
        pings_public,
    )));

    run_command_dispatcher(
        broadcast_rx,
        client,
        commands,
        admin_channel,
        chat_history,
        history_length,
    )
    .await;
}

/// Main dispatch loop for trait-based commands.
async fn run_command_dispatcher(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    commands: Vec<Box<dyn commands::Command>>,
    admin_channel: Option<String>,
    chat_history: Option<ChatHistory>,
    history_length: usize,
) {
    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                // In the admin channel, only the broadcaster can use commands
                if let Some(ref admin_ch) = admin_channel
                    && privmsg.channel_login == *admin_ch
                    && !privmsg.badges.iter().any(|b| b.name == "broadcaster")
                {
                    continue;
                }

                // Record message in chat history (main channel only)
                if let Some(ref history) = chat_history {
                    let is_admin_channel = admin_channel
                        .as_ref()
                        .is_some_and(|ch| privmsg.channel_login == *ch);
                    if !is_admin_channel {
                        let mut buf = history.lock().await;
                        if buf.len() >= history_length {
                            buf.pop_front();
                        }
                        buf.push_back((privmsg.sender.login.clone(), privmsg.message_text.clone()));
                    }
                }

                let mut words = privmsg.message_text.split_whitespace();
                let Some(first_word) = words.next() else {
                    continue;
                };

                let Some(cmd) = commands
                    .iter()
                    .find(|c| c.enabled() && c.matches(first_word))
                else {
                    continue;
                };

                let ctx = commands::CommandContext {
                    privmsg: &privmsg,
                    client: &client,
                    trigger: first_word,
                    args: words.collect(),
                };

                if let Err(e) = cmd.execute(ctx).await {
                    error!(
                        error = ?e,
                        user = %privmsg.sender.login,
                        command = %first_word,
                        "Error handling command"
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "Command handler lagged, skipped messages");
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, command handler exiting");
                break;
            }
        }
    }
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

        if let Err(e) = client.say(channel.clone(), schedule.message.clone()).await {
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_prefill_threshold_validation() {
        // Threshold must be between 0.0 and 1.0
        assert!((0.0..=1.0).contains(&0.0));
        assert!((0.0..=1.0).contains(&0.5));
        assert!((0.0..=1.0).contains(&1.0));
        assert!(!(0.0..=1.0).contains(&-0.1));
        assert!(!(0.0..=1.0).contains(&1.1));
    }
}
