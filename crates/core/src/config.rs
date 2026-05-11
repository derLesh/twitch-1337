//! Configuration types loaded from config.toml.
//!
//! These are kept in the library so that handler modules (and integration
//! tests) can reference them without going through the binary entry point.

use eyre::{Result, WrapErr, bail};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use tracing::{debug, info};

use crate::{ai::prefill, database};

fn default_expected_latency() -> u32 {
    100
}

#[derive(Debug, Clone, Deserialize)]
pub struct TwitchConfiguration {
    pub channel: String,
    pub username: String,
    pub refresh_token: SecretString,
    pub client_id: SecretString,
    pub client_secret: SecretString,
    #[serde(default = "default_expected_latency")]
    pub expected_latency: u32,
    #[serde(default)]
    pub hidden_admins: Vec<String>,
    #[serde(default)]
    pub admin_channel: Option<String>,
    #[serde(default)]
    pub ai_channel: Option<String>,
}

/// Which LLM backend to use.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiBackend {
    OpenAi,
    Ollama,
}

fn default_ai_timeout() -> u64 {
    30
}

fn default_history_length() -> u64 {
    crate::ai::chat_history::DEFAULT_HISTORY_LENGTH
}

fn default_ai_channel_history_length() -> u64 {
    50
}

fn default_emote_refresh_interval() -> u64 {
    3600
}

fn default_max_prompt_emotes() -> usize {
    12
}

fn default_min_baseline_emotes() -> usize {
    4
}

fn default_true() -> bool {
    true
}

fn default_soul_bytes() -> usize {
    4096
}

fn default_lore_bytes() -> usize {
    12_288
}

fn default_user_bytes() -> usize {
    4096
}

fn default_state_bytes() -> usize {
    2048
}

fn default_inject_budget() -> usize {
    24_576
}

fn default_max_state_files() -> usize {
    16
}

fn default_max_turn_rounds() -> usize {
    4
}

fn default_max_writes_per_turn() -> usize {
    8
}

fn default_dreamer_run_at() -> String {
    "04:00".to_string()
}

fn default_dreamer_timeout() -> u64 {
    120
}

fn default_dreamer_max_rounds() -> usize {
    20
}

fn default_max_rounds() -> usize {
    3
}

fn default_web_timeout() -> u64 {
    15
}

fn default_web_max_results() -> usize {
    5
}

fn default_web_cache_ttl_secs() -> u64 {
    300
}

fn default_web_cache_capacity() -> usize {
    100
}

fn default_web_base_url() -> String {
    "http://localhost:8080/search".to_string()
}

fn default_media_model() -> String {
    "~google/gemini-flash-latest".to_string()
}

fn default_media_timeout() -> u64 {
    60
}

fn default_max_image_size() -> bytesize::ByteSize {
    bytesize::ByteSize::mib(10)
}

fn default_max_pdf_size() -> bytesize::ByteSize {
    bytesize::ByteSize::mib(25)
}

fn default_max_audio_size() -> bytesize::ByteSize {
    bytesize::ByteSize::mib(25)
}

fn default_max_video_size() -> bytesize::ByteSize {
    bytesize::ByteSize::mib(50)
}

fn default_max_text_size() -> bytesize::ByteSize {
    bytesize::ByteSize::mib(1)
}

fn default_aviationstack_base_url() -> String {
    "https://api.aviationstack.com/v1".to_string()
}

fn default_aviationstack_timeout_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct AviationstackConfig {
    #[serde(default)]
    pub enabled: bool,
    pub api_key: SecretString,
    #[serde(default = "default_aviationstack_base_url")]
    pub base_url: String,
    #[serde(default = "default_aviationstack_timeout_secs")]
    pub timeout_secs: u64,
}

/// Byte-budget caps for the AI memory store. See `[ai.memory]` in
/// `config.toml.example`.
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfigSection {
    #[serde(default = "default_soul_bytes")]
    pub soul_bytes: usize,
    #[serde(default = "default_lore_bytes")]
    pub lore_bytes: usize,
    #[serde(default = "default_user_bytes")]
    pub user_bytes: usize,
    #[serde(default = "default_state_bytes")]
    pub state_bytes: usize,
    #[serde(default = "default_inject_budget")]
    pub inject_byte_budget: usize,
    #[serde(default = "default_max_state_files")]
    pub max_state_files: usize,
}

impl Default for MemoryConfigSection {
    fn default() -> Self {
        Self {
            soul_bytes: default_soul_bytes(),
            lore_bytes: default_lore_bytes(),
            user_bytes: default_user_bytes(),
            state_bytes: default_state_bytes(),
            inject_byte_budget: default_inject_budget(),
            max_state_files: default_max_state_files(),
        }
    }
}

/// Knobs for the nightly dreamer pass (replaces the old consolidation).
/// See `[ai.dreamer]` in `config.toml.example`.
#[derive(Debug, Clone, Deserialize)]
pub struct DreamerConfigSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default = "default_dreamer_run_at")]
    pub run_at: String,
    #[serde(default = "default_dreamer_timeout")]
    pub timeout_secs: u64,
    /// Max tool-call rounds the ritual is allowed. The dreamer touches every
    /// memory file plus the day's transcript, so this is intentionally larger
    /// than `ai.max_turn_rounds` (which caps a single chat turn).
    #[serde(default = "default_dreamer_max_rounds")]
    pub max_rounds: usize,
}

impl Default for DreamerConfigSection {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            reasoning_effort: None,
            run_at: default_dreamer_run_at(),
            timeout_secs: default_dreamer_timeout(),
            max_rounds: default_dreamer_max_rounds(),
        }
    }
}

/// Knobs for optional 7TV emote grounding in the `!ai` prompt.
///
/// Disabled by default. When enabled, the bot loads the current 7TV channel
/// set, optionally global 7TV emotes, and intersects that catalog with a
/// manually maintained glossary before adding emote hints to the model prompt.
#[derive(Debug, Clone, Deserialize)]
pub struct AiEmotesConfigSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub include_global: bool,
    #[serde(default = "default_emote_refresh_interval")]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_max_prompt_emotes")]
    pub max_prompt_emotes: usize,
    /// Floor for emotes injected even when nothing in the turn matches.
    /// Filled in glossary order so the model always sees a baseline vocabulary;
    /// scoring emotes (recent in chat or context-matched against the user
    /// instruction) stack on top up to `max_prompt_emotes`.
    #[serde(default = "default_min_baseline_emotes")]
    pub min_baseline_emotes: usize,
    /// Optional override for tests or private mirrors. Defaults to
    /// `https://7tv.io/v3` when omitted.
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Default for AiEmotesConfigSection {
    fn default() -> Self {
        Self {
            enabled: false,
            include_global: true,
            refresh_interval_secs: default_emote_refresh_interval(),
            max_prompt_emotes: default_max_prompt_emotes(),
            min_baseline_emotes: default_min_baseline_emotes(),
            base_url: None,
        }
    }
}

/// Tool-calling web access for `!ai` (`web_search` and `read_url`).
#[derive(Debug, Clone, Deserialize)]
pub struct AiWebConfigSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_base_url")]
    pub base_url: String,
    #[serde(default = "default_web_timeout")]
    pub timeout: u64,
    #[serde(default = "default_web_max_results")]
    pub max_results: usize,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
    #[serde(default = "default_web_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_web_cache_capacity")]
    pub cache_capacity: usize,
}

impl Default for AiWebConfigSection {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_web_base_url(),
            timeout: default_web_timeout(),
            max_results: default_web_max_results(),
            max_rounds: default_max_rounds(),
            cache_ttl_secs: default_web_cache_ttl_secs(),
            cache_capacity: default_web_cache_capacity(),
        }
    }
}

/// Multimodal sub-agent for `read_url`. Reuses `[ai].api_key` and
/// `[ai].base_url`; only the model and per-type size caps differ.
#[derive(Debug, Clone, Deserialize)]
pub struct AiMediaConfig {
    #[serde(default = "default_media_model")]
    pub model: String,
    #[serde(default = "default_media_timeout")]
    pub timeout: u64,
    #[serde(default = "default_max_image_size")]
    pub max_image_size: bytesize::ByteSize,
    #[serde(default = "default_max_pdf_size")]
    pub max_pdf_size: bytesize::ByteSize,
    #[serde(default = "default_max_audio_size")]
    pub max_audio_size: bytesize::ByteSize,
    #[serde(default = "default_max_video_size")]
    pub max_video_size: bytesize::ByteSize,
    #[serde(default = "default_max_text_size")]
    pub max_text_size: bytesize::ByteSize,
}

impl Default for AiMediaConfig {
    fn default() -> Self {
        Self {
            model: default_media_model(),
            timeout: default_media_timeout(),
            max_image_size: default_max_image_size(),
            max_pdf_size: default_max_pdf_size(),
            max_audio_size: default_max_audio_size(),
            max_video_size: default_max_video_size(),
            max_text_size: default_max_text_size(),
        }
    }
}

impl AiMediaConfig {
    pub fn cap_for(&self, bucket: crate::ai::content::detect::Bucket) -> bytesize::ByteSize {
        use crate::ai::content::detect::Bucket;
        match bucket {
            Bucket::Image => self.max_image_size,
            Bucket::Pdf => self.max_pdf_size,
            Bucket::Audio => self.max_audio_size,
            Bucket::Video => self.max_video_size,
            Bucket::Text => self.max_text_size,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiConfig {
    /// Backend type: "openai" or "ollama"
    pub backend: AiBackend,
    /// API key (required for openai, not used for ollama)
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Base URL for the API (optional, has per-backend defaults)
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model name to use
    pub model: String,
    /// Timeout for AI requests in seconds (default: 30)
    #[serde(default = "default_ai_timeout")]
    pub timeout: u64,
    /// Optional reasoning effort hint. Values are provider/model-specific.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Number of recent chat messages to keep in the local tool-readable buffer.
    #[serde(default = "default_history_length")]
    pub history_length: u64,
    /// Capacity of the rolling buffer recording messages from `twitch.ai_channel`.
    /// Allocated only when `twitch.ai_channel` is set.
    #[serde(default = "default_ai_channel_history_length")]
    pub ai_channel_history_length: u64,
    /// Optional: Prefill chat history from a rustlog-compatible API at startup
    #[serde(default)]
    pub history_prefill: Option<prefill::HistoryPrefillConfig>,
    /// Byte-budget caps for the memory store.
    #[serde(default)]
    pub memory: MemoryConfigSection,
    /// Max tool-call rounds per `!ai` request (default: 4).
    #[serde(default = "default_max_turn_rounds")]
    pub max_turn_rounds: usize,
    /// Max memory writes the model may make per turn (default: 8).
    #[serde(default = "default_max_writes_per_turn")]
    pub max_writes_per_turn: usize,
    /// Nightly dreamer pass knobs.
    #[serde(default)]
    pub dreamer: DreamerConfigSection,
    /// Optional 7TV emote glossary prompt grounding.
    #[serde(default)]
    pub emotes: AiEmotesConfigSection,
    /// Multimodal sub-agent for `read_url`.
    #[serde(default)]
    pub media: AiMediaConfig,
    /// Optional web tool surface for `!ai` (`web_search`, `read_url`).
    #[serde(default)]
    pub web: AiWebConfigSection,
}

fn validate_reasoning_effort(path: &str, value: Option<&str>) -> Result<()> {
    if let Some(v) = value
        && v.trim().is_empty()
    {
        bail!("{path} cannot be empty when specified");
    }
    Ok(())
}

fn default_cooldown() -> u64 {
    300
}

fn default_ai_cooldown() -> u64 {
    30
}

fn default_news_cooldown() -> u64 {
    60
}

fn default_up_cooldown() -> u64 {
    30
}

fn default_feedback_cooldown() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
pub struct CooldownsConfig {
    #[serde(default = "default_ai_cooldown")]
    pub ai: u64,
    #[serde(default = "default_news_cooldown")]
    pub news: u64,
    #[serde(default = "default_up_cooldown")]
    pub up: u64,
    #[serde(default = "default_feedback_cooldown")]
    pub feedback: u64,
}

impl Default for CooldownsConfig {
    fn default() -> Self {
        Self {
            ai: default_ai_cooldown(),
            news: default_news_cooldown(),
            up: default_up_cooldown(),
            feedback: default_feedback_cooldown(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PingsConfig {
    #[serde(default = "default_cooldown")]
    pub cooldown: u64,
    #[serde(default)]
    pub public: bool,
}

impl Default for PingsConfig {
    fn default() -> Self {
        Self {
            cooldown: default_cooldown(),
            public: false,
        }
    }
}

fn default_suspend_duration() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize)]
pub struct SuspendConfig {
    #[serde(default = "default_suspend_duration")]
    pub default_duration_secs: u64,
}

impl Default for SuspendConfig {
    fn default() -> Self {
        Self {
            default_duration_secs: default_suspend_duration(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

/// Configuration for a scheduled message loaded from config.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleConfig {
    pub name: String,
    pub message: String,
    /// Interval in "hh:mm" format (e.g., "01:30" for 1 hour 30 minutes)
    pub interval: String,
    /// Start date in ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
    #[serde(default)]
    pub start_date: Option<String>,
    /// End date in ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
    #[serde(default)]
    pub end_date: Option<String>,
    /// Daily active time start in HH:MM format
    #[serde(default)]
    pub active_time_start: Option<String>,
    /// Daily active time end in HH:MM format
    #[serde(default)]
    pub active_time_end: Option<String>,
    /// Whether the schedule is enabled (default: true)
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_bind")]
    pub bind_addr: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default = "default_web_session_secret")]
    pub session_secret: SecretString,
    #[serde(default = "default_session_ttl", with = "humantime_serde")]
    pub session_ttl: std::time::Duration,
    #[serde(default = "default_mod_check_refresh", with = "humantime_serde")]
    pub mod_check_refresh: std::time::Duration,
}

fn default_web_bind() -> String {
    "127.0.0.1:8080".to_owned()
}
fn default_session_ttl() -> std::time::Duration {
    std::time::Duration::from_secs(7 * 24 * 60 * 60)
}
fn default_mod_check_refresh() -> std::time::Duration {
    std::time::Duration::from_secs(300)
}
fn default_web_session_secret() -> SecretString {
    SecretString::new(String::new().into())
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: default_web_bind(),
            public_url: String::new(),
            session_secret: default_web_session_secret(),
            session_ttl: default_session_ttl(),
            mod_check_refresh: default_mod_check_refresh(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Configuration {
    pub twitch: TwitchConfiguration,
    #[serde(default)]
    pub aviationstack: Option<AviationstackConfig>,
    #[serde(default)]
    pub pings: PingsConfig,
    #[serde(default)]
    pub cooldowns: CooldownsConfig,
    #[serde(default)]
    pub suspend: SuspendConfig,
    #[serde(default)]
    pub ai: Option<AiConfig>,
    #[serde(default)]
    pub schedules: Vec<ScheduleConfig>,
    #[serde(default)]
    pub web: WebConfig,
}

#[cfg(any(test, feature = "testing"))]
impl Configuration {
    /// Minimal configuration suitable for integration tests. Channel =
    /// "test_chan", username = "bot", no AI, no schedules, default ping
    /// cooldown. Tests override fields via `TestBotBuilder::with_config`.
    pub fn test_default() -> Self {
        Self {
            twitch: TwitchConfiguration {
                channel: "test_chan".to_owned(),
                username: "bot".to_owned(),
                refresh_token: SecretString::new("test".into()),
                client_id: SecretString::new("test".into()),
                client_secret: SecretString::new("test".into()),
                expected_latency: 100,
                hidden_admins: Vec::new(),
                admin_channel: None,
                ai_channel: None,
            },
            aviationstack: None,
            pings: PingsConfig::default(),
            cooldowns: CooldownsConfig::default(),
            suspend: SuspendConfig::default(),
            ai: None,
            schedules: Vec::new(),
            web: WebConfig::default(),
        }
    }
}

/// Load and validate configuration from the standard config path.
pub async fn load_configuration() -> Result<Configuration> {
    let config_path = crate::get_config_path();
    let data = tokio::fs::read_to_string(&config_path)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to read config file: {}\nPlease create config.toml from config.toml.example",
                config_path.display()
            )
        })?;

    info!("Loading configuration from {}", config_path.display());

    let config: Configuration =
        toml::from_str(&data).wrap_err("Failed to parse config.toml - check for syntax errors")?;

    validate_config(&config)?;

    debug!(public = config.pings.public, "Ping trigger policy");

    Ok(config)
}

/// Validate config fields beyond what serde can express.
pub fn validate_config(config: &Configuration) -> Result<()> {
    if config.twitch.channel.trim().is_empty() {
        bail!("twitch.channel cannot be empty");
    }

    if config.twitch.username.trim().is_empty() {
        bail!("twitch.username cannot be empty");
    }

    if config.twitch.expected_latency > 1000 {
        bail!(
            "twitch.expected_latency must be <= 1000ms (got {})",
            config.twitch.expected_latency
        );
    }

    if let Some(ref admin_ch) = config.twitch.admin_channel {
        if admin_ch.trim().is_empty() {
            bail!("twitch.admin_channel cannot be empty when specified");
        }
        if admin_ch == &config.twitch.channel {
            bail!("twitch.admin_channel must be different from twitch.channel");
        }
    }

    if let Some(ref ai_ch) = config.twitch.ai_channel {
        if ai_ch.trim().is_empty() {
            bail!("twitch.ai_channel cannot be empty when specified");
        }
        if ai_ch == &config.twitch.channel {
            bail!("twitch.ai_channel must be different from twitch.channel");
        }
        // Cross-check: admin_channel block above cannot see ai_channel, so the
        // admin_channel == ai_channel guard lives here. Keep this branch second.
        if let Some(ref admin_ch) = config.twitch.admin_channel
            && ai_ch == admin_ch
        {
            bail!("twitch.ai_channel must be different from twitch.admin_channel");
        }
    }

    if !(1..=7 * 86400).contains(&config.suspend.default_duration_secs) {
        bail!(
            "suspend.default_duration_secs must be between 1 and 604800 (7 days) (got {})",
            config.suspend.default_duration_secs
        );
    }

    if let Some(ref aviationstack) = config.aviationstack {
        if aviationstack.enabled && aviationstack.api_key.expose_secret().trim().is_empty() {
            bail!("aviationstack.api_key cannot be empty when aviationstack is enabled");
        }
        if aviationstack.base_url.trim().is_empty() {
            bail!("aviationstack.base_url cannot be empty");
        }
        reqwest::Url::parse(&aviationstack.base_url).wrap_err_with(|| {
            format!(
                "aviationstack.base_url must be a valid URL (got {:?})",
                aviationstack.base_url
            )
        })?;
        if aviationstack.timeout_secs == 0 {
            bail!("aviationstack.timeout_secs must be > 0");
        }
    }

    if let Some(ref ai) = config.ai
        && matches!(ai.backend, AiBackend::OpenAi)
        && ai.api_key.is_none()
    {
        bail!("AI backend 'openai' requires an api_key");
    }

    if let Some(ref ai) = config.ai
        && ai.history_length > crate::ai::chat_history::MAX_HISTORY_LENGTH
    {
        bail!(
            "ai.history_length must be <= {} (got {})",
            crate::ai::chat_history::MAX_HISTORY_LENGTH,
            ai.history_length
        );
    }

    if let Some(ref ai) = config.ai
        && ai.ai_channel_history_length > crate::ai::chat_history::MAX_HISTORY_LENGTH
    {
        bail!(
            "ai.ai_channel_history_length must be <= {} (got {})",
            crate::ai::chat_history::MAX_HISTORY_LENGTH,
            ai.ai_channel_history_length
        );
    }

    if let Some(ref ai) = config.ai
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

    if let Some(ref ai) = config.ai
        && ai.history_prefill.is_some()
        && ai.history_length == 0
    {
        bail!("ai.history_prefill requires history_length > 0");
    }

    if let Some(ref ai) = config.ai {
        if !(1..=20).contains(&ai.max_turn_rounds) {
            bail!(
                "ai.max_turn_rounds must be 1..=20 (got {})",
                ai.max_turn_rounds
            );
        }
        if !(1..=64).contains(&ai.max_writes_per_turn) {
            bail!(
                "ai.max_writes_per_turn must be 1..=64 (got {})",
                ai.max_writes_per_turn
            );
        }
        if !(1..=200).contains(&ai.dreamer.max_rounds) {
            bail!(
                "ai.dreamer.max_rounds must be 1..=200 (got {})",
                ai.dreamer.max_rounds
            );
        }
        validate_reasoning_effort(
            "ai.dreamer.reasoning_effort",
            ai.dreamer.reasoning_effort.as_deref(),
        )?;
        chrono::NaiveTime::parse_from_str(&ai.dreamer.run_at, "%H:%M").wrap_err_with(|| {
            format!(
                "ai.dreamer.run_at must be HH:MM (got {:?})",
                ai.dreamer.run_at
            )
        })?;
        if ai.dreamer.timeout_secs == 0 {
            bail!("ai.dreamer.timeout_secs must be > 0");
        }
        if ai.memory.inject_byte_budget < ai.memory.soul_bytes + ai.memory.lore_bytes {
            bail!("ai.memory.inject_byte_budget must be >= soul_bytes + lore_bytes");
        }
    }

    if let Some(ref ai) = config.ai {
        validate_reasoning_effort("ai.reasoning_effort", ai.reasoning_effort.as_deref())?;
        if ai.web.base_url.trim().is_empty() {
            bail!("ai.web.base_url cannot be empty");
        }
        reqwest::Url::parse(&ai.web.base_url).wrap_err_with(|| {
            format!(
                "ai.web.base_url must be a valid URL (got {:?})",
                ai.web.base_url
            )
        })?;
        if !(1..=10).contains(&ai.web.max_results) {
            bail!(
                "ai.web.max_results must be between 1 and 10 (got {})",
                ai.web.max_results
            );
        }
        if !(1..=6).contains(&ai.web.max_rounds) {
            bail!(
                "ai.web.max_rounds must be between 1 and 6 (got {})",
                ai.web.max_rounds
            );
        }
        if ai.web.cache_capacity == 0 {
            bail!("ai.web.cache_capacity must be > 0");
        }
    }
    if let Some(ref ai) = config.ai
        && ai.emotes.enabled
    {
        if ai.emotes.refresh_interval_secs == 0 {
            bail!("ai.emotes.refresh_interval_secs must be > 0");
        }
        if !(1..=200).contains(&ai.emotes.max_prompt_emotes) {
            bail!(
                "ai.emotes.max_prompt_emotes must be between 1 and 200 (got {})",
                ai.emotes.max_prompt_emotes
            );
        }
        if ai.emotes.min_baseline_emotes > ai.emotes.max_prompt_emotes {
            bail!(
                "ai.emotes.min_baseline_emotes ({}) must be <= max_prompt_emotes ({})",
                ai.emotes.min_baseline_emotes,
                ai.emotes.max_prompt_emotes
            );
        }
        if let Some(ref base_url) = ai.emotes.base_url
            && base_url.trim().is_empty()
        {
            bail!("ai.emotes.base_url cannot be empty when specified");
        }
    }

    for schedule in &config.schedules {
        if schedule.name.trim().is_empty() {
            bail!("Schedule name cannot be empty");
        }
        if schedule.message.trim().is_empty() {
            bail!("Schedule '{}' message cannot be empty", schedule.name);
        }
        if schedule.interval.trim().is_empty() {
            bail!("Schedule '{}' interval cannot be empty", schedule.name);
        }
        database::Schedule::parse_interval(&schedule.interval).wrap_err_with(|| {
            format!("Schedule '{}' has invalid interval format", schedule.name)
        })?;
    }

    if config.web.enabled {
        let secret = config.web.session_secret.expose_secret();
        let secret_bytes_len = hex::decode(secret).map(|b| b.len()).unwrap_or(0);
        if secret_bytes_len < 32 {
            bail!("web.session_secret must be ≥32 bytes hex when web.enabled = true");
        }
        if !config.web.public_url.starts_with("https://") {
            bail!(
                "web.public_url must start with https:// when web.enabled = true (got {:?})",
                config.web.public_url
            );
        }
        let ttl = config.web.session_ttl.as_secs();
        if !(3600..=2_592_000).contains(&ttl) {
            bail!("web.session_ttl must be between 1h and 30d (got {ttl}s)");
        }
        let refresh = config.web.mod_check_refresh.as_secs();
        if !(30..=3600).contains(&refresh) {
            bail!("web.mod_check_refresh must be between 30s and 1h (got {refresh}s)");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_memory_v2_defaults() {
        let s = MemoryConfigSection::default();
        assert_eq!(s.soul_bytes, 4096);
        assert_eq!(s.lore_bytes, 12288);
        assert_eq!(s.user_bytes, 4096);
        assert_eq!(s.state_bytes, 2048);
        assert_eq!(s.inject_byte_budget, 24576);
        assert_eq!(s.max_state_files, 16);
    }

    #[test]
    fn ai_dreamer_defaults() {
        let d = DreamerConfigSection::default();
        assert!(d.enabled);
        assert_eq!(d.run_at, "04:00");
        assert_eq!(d.timeout_secs, 120);
        assert!(d.model.is_none());
    }

    #[test]
    fn ai_top_level_v2_defaults() {
        let ai: AiConfig = toml::from_str(
            r#"
            backend = "ollama"
            model = "x"
        "#,
        )
        .unwrap();
        assert_eq!(ai.max_turn_rounds, 4);
        assert_eq!(ai.max_writes_per_turn, 8);
    }

    fn ai_with_run_at(run_at: &str) -> AiConfig {
        AiConfig {
            backend: AiBackend::Ollama,
            api_key: None,
            base_url: None,
            model: "x".into(),
            timeout: default_ai_timeout(),
            reasoning_effort: None,
            history_length: default_history_length(),
            ai_channel_history_length: default_ai_channel_history_length(),
            history_prefill: None,
            memory: MemoryConfigSection::default(),
            max_turn_rounds: default_max_turn_rounds(),
            max_writes_per_turn: default_max_writes_per_turn(),
            dreamer: DreamerConfigSection {
                run_at: run_at.into(),
                ..DreamerConfigSection::default()
            },
            emotes: AiEmotesConfigSection::default(),
            media: AiMediaConfig::default(),
            web: AiWebConfigSection::default(),
        }
    }

    #[test]
    fn validate_rejects_malformed_run_at() {
        let mut c = Configuration::test_default();
        c.ai = Some(ai_with_run_at("not-a-time"));
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.dreamer.run_at"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_accepts_well_formed_run_at() {
        let mut c = Configuration::test_default();
        c.ai = Some(ai_with_run_at("04:00"));
        validate_config(&c).unwrap();
    }

    #[test]
    fn ai_defaults_keep_tool_history_enabled() {
        let ai: AiConfig = toml::from_str(
            r#"
            backend = "ollama"
            model = "x"
            "#,
        )
        .unwrap();

        assert_eq!(
            ai.history_length,
            crate::ai::chat_history::DEFAULT_HISTORY_LENGTH
        );
    }

    #[test]
    fn validate_rejects_history_length_above_max() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.history_length = crate::ai::chat_history::MAX_HISTORY_LENGTH + 1;
        c.ai = Some(ai);

        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.history_length"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_accepts_history_length_200() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.history_length = 200;
        c.ai = Some(ai);

        validate_config(&c).unwrap();
    }

    #[test]
    fn ai_emotes_default_disabled() {
        assert!(!AiEmotesConfigSection::default().enabled);
        assert!(AiEmotesConfigSection::default().include_global);
    }

    #[test]
    fn validate_rejects_invalid_emote_settings() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.emotes.enabled = true;
        ai.emotes.max_prompt_emotes = 0;
        c.ai = Some(ai);

        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.emotes.max_prompt_emotes"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_accepts_enabled_emote_settings() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.emotes.enabled = true;
        ai.emotes.refresh_interval_secs = 60;
        ai.emotes.max_prompt_emotes = 40;
        c.ai = Some(ai);

        validate_config(&c).unwrap();
    }

    #[test]
    fn validate_rejects_empty_reasoning_effort() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.reasoning_effort = Some("   ".into());
        c.ai = Some(ai);

        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.reasoning_effort"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_accepts_non_empty_dreamer_reasoning_effort() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.reasoning_effort = Some("medium".into());
        ai.dreamer.reasoning_effort = Some("high".into());
        c.ai = Some(ai);

        validate_config(&c).unwrap();
    }

    #[test]
    fn validate_rejects_web_max_results_out_of_range() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.web.max_results = 0;
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(format!("{err:#}").contains("ai.web.max_results"));
    }

    #[test]
    fn validate_rejects_web_invalid_base_url() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.web.base_url = "not a url".into();
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(format!("{err:#}").contains("ai.web.base_url"));
    }

    #[test]
    fn ai_channel_must_differ_from_main_channel() {
        let mut config = Configuration::test_default();
        config.twitch.ai_channel = Some(config.twitch.channel.clone());
        let err = validate_config(&config).unwrap_err().to_string();
        assert!(
            err.contains("ai_channel must be different from twitch.channel"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ai_channel_must_differ_from_admin_channel() {
        let mut config = Configuration::test_default();
        config.twitch.admin_channel = Some("admins".into());
        config.twitch.ai_channel = Some("admins".into());
        let err = validate_config(&config).unwrap_err().to_string();
        assert!(
            err.contains("ai_channel must be different from twitch.admin_channel"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ai_channel_cannot_be_blank_when_set() {
        let mut config = Configuration::test_default();
        config.twitch.ai_channel = Some("   ".into());
        let err = validate_config(&config).unwrap_err().to_string();
        assert!(
            err.contains("ai_channel cannot be empty when specified"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ai_channel_some_distinct_value_validates() {
        let mut config = Configuration::test_default();
        config.twitch.ai_channel = Some("ai_chan".into());
        validate_config(&config).expect("distinct ai_channel must validate");
    }

    #[test]
    fn ai_media_defaults_when_section_absent() {
        let ai: AiConfig = toml::from_str(
            r#"
                backend = "openai"
                api_key = "k"
                model = "m"
            "#,
        )
        .expect("parse");

        assert_eq!(ai.media.model, "~google/gemini-flash-latest");
        assert_eq!(ai.media.timeout, 60);
        assert_eq!(ai.media.max_image_size.as_u64(), 10 * 1024 * 1024);
        assert_eq!(ai.media.max_pdf_size.as_u64(), 25 * 1024 * 1024);
        assert_eq!(ai.media.max_audio_size.as_u64(), 25 * 1024 * 1024);
        assert_eq!(ai.media.max_video_size.as_u64(), 50 * 1024 * 1024);
        assert_eq!(ai.media.max_text_size.as_u64(), 1024 * 1024);
    }

    #[test]
    fn ai_media_parses_human_readable_sizes() {
        let ai: AiConfig = toml::from_str(
            r#"
                backend = "openai"
                api_key = "k"
                model = "m"

                [media]
                model = "openai/gpt-4o-mini"
                timeout = 90
                max_image_size = "5 MB"
                max_pdf_size = "15 MB"
                max_audio_size = "20 MB"
                max_video_size = "100 MB"
                max_text_size = "512 KB"
            "#,
        )
        .expect("parse");

        assert_eq!(ai.media.model, "openai/gpt-4o-mini");
        assert_eq!(ai.media.timeout, 90);
        assert_eq!(ai.media.max_image_size.as_u64(), 5 * 1000 * 1000);
        assert_eq!(ai.media.max_text_size.as_u64(), 512 * 1000);
    }

    #[test]
    fn validate_rejects_max_turn_rounds_out_of_range() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.max_turn_rounds = 0;
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.max_turn_rounds"),
            "got: {err:#}"
        );

        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.max_turn_rounds = 21;
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.max_turn_rounds"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_rejects_max_writes_per_turn_out_of_range() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.max_writes_per_turn = 0;
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.max_writes_per_turn"),
            "got: {err:#}"
        );

        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.max_writes_per_turn = 65;
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.max_writes_per_turn"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_rejects_inject_budget_below_soul_plus_lore() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.memory.soul_bytes = 4096;
        ai.memory.lore_bytes = 12_288;
        ai.memory.inject_byte_budget = 8192; // less than 4096 + 12288
        c.ai = Some(ai);
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("inject_byte_budget"),
            "got: {err:#}"
        );
    }

    #[test]
    fn ai_channel_history_length_default_is_50() {
        let ai: AiConfig = toml::from_str(
            r#"
            backend = "ollama"
            model = "x"
            "#,
        )
        .unwrap();
        assert_eq!(ai.ai_channel_history_length, 50);
    }

    #[test]
    fn validate_rejects_ai_channel_history_length_above_max() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.ai_channel_history_length = crate::ai::chat_history::MAX_HISTORY_LENGTH + 1;
        c.ai = Some(ai);

        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.ai_channel_history_length"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_accepts_ai_channel_history_length_50() {
        let mut c = Configuration::test_default();
        let mut ai = ai_with_run_at("04:00");
        ai.ai_channel_history_length = 50;
        c.ai = Some(ai);
        validate_config(&c).unwrap();
    }

    #[test]
    fn web_disabled_skips_validation() {
        let cfg = Configuration::test_default();
        assert!(!cfg.web.enabled);
        validate_config(&cfg).expect("disabled web validates trivially");
    }

    #[test]
    fn web_enabled_requires_https_public_url() {
        let mut cfg = Configuration::test_default();
        cfg.web.enabled = true;
        cfg.web.session_secret = secrecy::SecretString::new("00".repeat(32).into());
        cfg.web.public_url = "http://insecure".into();
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("public_url"), "{err}");
    }

    #[test]
    fn web_enabled_requires_32_byte_secret() {
        let mut cfg = Configuration::test_default();
        cfg.web.enabled = true;
        cfg.web.session_secret = secrecy::SecretString::new("ab".into());
        cfg.web.public_url = "https://bot.test".into();
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("session_secret"), "{err}");
    }

    #[test]
    fn web_enabled_validates_ttl_range() {
        let mut cfg = Configuration::test_default();
        cfg.web.enabled = true;
        cfg.web.session_secret = secrecy::SecretString::new("00".repeat(32).into());
        cfg.web.public_url = "https://bot.test".into();
        cfg.web.session_ttl = std::time::Duration::from_secs(10); // below 1h
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("session_ttl"), "{err}");
    }

    #[test]
    fn ai_media_cap_for_bucket_returns_correct_field() {
        use crate::ai::content::detect::Bucket;
        let cfg = AiMediaConfig::default();
        assert_eq!(cfg.cap_for(Bucket::Image), cfg.max_image_size);
        assert_eq!(cfg.cap_for(Bucket::Pdf), cfg.max_pdf_size);
        assert_eq!(cfg.cap_for(Bucket::Audio), cfg.max_audio_size);
        assert_eq!(cfg.cap_for(Bucket::Video), cfg.max_video_size);
        assert_eq!(cfg.cap_for(Bucket::Text), cfg.max_text_size);
    }
}
