//! Configuration types loaded from config.toml.
//!
//! These are kept in the library so that handler modules (and integration
//! tests) can reference them without going through the binary entry point.

use eyre::{Result, WrapErr, bail};
use secrecy::SecretString;
use serde::Deserialize;
use tracing::info;

use crate::{database, prefill};

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
}

/// Which LLM backend to use.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiBackend {
    OpenAi,
    Ollama,
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

fn default_true() -> bool {
    true
}

pub(crate) fn default_max_user() -> usize {
    50
}

fn default_max_lore() -> usize {
    50
}

fn default_max_pref() -> usize {
    50
}

fn default_half_life() -> u32 {
    30
}

fn default_max_rounds() -> usize {
    3
}

fn default_run_at() -> String {
    "04:00".to_string()
}

fn default_consolidation_timeout() -> u64 {
    120
}

/// Per-scope caps + decay policy for the AI memory store. See
/// `[ai.memory]` in `config.toml.example`.
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfigSection {
    /// Enable persistent AI memory (default: false)
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_user")]
    pub max_user: usize,
    #[serde(default = "default_max_lore")]
    pub max_lore: usize,
    #[serde(default = "default_max_pref")]
    pub max_pref: usize,
    #[serde(default = "default_half_life")]
    pub half_life_days: u32,
}

impl Default for MemoryConfigSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_user: default_max_user(),
            max_lore: default_max_lore(),
            max_pref: default_max_pref(),
            half_life_days: default_half_life(),
        }
    }
}

/// Knobs for the per-turn memory extractor. `model` / `timeout` fall back
/// to the main `[ai]` values when omitted. See `[ai.extraction]`.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractionConfigSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
}

impl Default for ExtractionConfigSection {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            timeout: None,
            max_rounds: default_max_rounds(),
        }
    }
}

/// Knobs for the daily memory-consolidation pass. See `[ai.consolidation]`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConsolidationConfigSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_run_at")]
    pub run_at: String,
    #[serde(default = "default_consolidation_timeout")]
    pub timeout: u64,
}

impl Default for ConsolidationConfigSection {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            run_at: default_run_at(),
            timeout: default_consolidation_timeout(),
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
    /// System prompt sent to the model
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    /// Template for the user message. Use `{message}` and `{chat_history}` as placeholders.
    #[serde(default = "default_instruction_template")]
    pub instruction_template: String,
    /// Timeout for AI requests in seconds (default: 30)
    #[serde(default = "default_ai_timeout")]
    pub timeout: u64,
    /// Number of recent chat messages to include as context (0 = disabled, max 100)
    #[serde(default)]
    pub history_length: u64,
    /// Optional: Prefill chat history from a rustlog-compatible API at startup
    #[serde(default)]
    pub history_prefill: Option<prefill::HistoryPrefillConfig>,
    /// Per-scope caps + decay for the memory store.
    #[serde(default)]
    pub memory: MemoryConfigSection,
    /// Per-turn extractor knobs.
    #[serde(default)]
    pub extraction: ExtractionConfigSection,
    /// Daily consolidation pass knobs.
    #[serde(default)]
    pub consolidation: ConsolidationConfigSection,
    /// Deprecated: replaced by `memory.max_user`. Logged as a warning if set.
    #[serde(default)]
    pub max_memories: Option<usize>,
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
pub struct CooldownsConfig {
    #[serde(default = "default_ai_cooldown")]
    pub ai: u64,
    #[serde(default = "default_up_cooldown")]
    pub up: u64,
    #[serde(default = "default_feedback_cooldown")]
    pub feedback: u64,
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
pub struct Configuration {
    pub twitch: TwitchConfiguration,
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
            },
            pings: PingsConfig::default(),
            cooldowns: CooldownsConfig::default(),
            suspend: SuspendConfig::default(),
            ai: None,
            schedules: Vec::new(),
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

    if config.pings.public {
        info!("Pings can be triggered by anyone");
    } else {
        info!("Pings can only be triggered by members");
    }

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

    if !(1..=7 * 86400).contains(&config.suspend.default_duration_secs) {
        bail!(
            "suspend.default_duration_secs must be between 1 and 604800 (7 days) (got {})",
            config.suspend.default_duration_secs
        );
    }

    if let Some(ref ai) = config.ai
        && matches!(ai.backend, AiBackend::OpenAi)
        && ai.api_key.is_none()
    {
        bail!("AI backend 'openai' requires an api_key");
    }

    if let Some(ref ai) = config.ai
        && ai.history_length > 100
    {
        bail!(
            "ai.history_length must be <= 100 (got {})",
            ai.history_length
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

    if let Some(ref ai) = config.ai
        && ai.memory.enabled
        && let Some(n) = ai.max_memories
        && !(1..=200).contains(&n)
    {
        bail!("ai.max_memories must be between 1 and 200 (got {n})");
    }

    // Parsed again at scheduler spawn; bail here so a typo doesn't take the
    // bot down after startup.
    if let Some(ref ai) = config.ai {
        chrono::NaiveTime::parse_from_str(&ai.consolidation.run_at, "%H:%M").wrap_err_with(
            || {
                format!(
                    "ai.consolidation.run_at must be HH:MM (got {:?})",
                    ai.consolidation.run_at
                )
            },
        )?;
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consolidation_run_at_parses() {
        let s = ConsolidationConfigSection::default();
        let t = chrono::NaiveTime::parse_from_str(&s.run_at, "%H:%M").unwrap();
        assert_eq!(t.format("%H:%M").to_string(), "04:00");
    }

    fn ai_with_run_at(run_at: &str) -> AiConfig {
        AiConfig {
            backend: AiBackend::Ollama,
            api_key: None,
            base_url: None,
            model: "x".into(),
            system_prompt: default_system_prompt(),
            instruction_template: default_instruction_template(),
            timeout: default_ai_timeout(),
            history_length: 0,
            history_prefill: None,
            memory: MemoryConfigSection::default(),
            extraction: ExtractionConfigSection::default(),
            consolidation: ConsolidationConfigSection {
                run_at: run_at.into(),
                ..ConsolidationConfigSection::default()
            },
            max_memories: None,
        }
    }

    #[test]
    fn validate_rejects_malformed_run_at() {
        let mut c = Configuration::test_default();
        c.ai = Some(ai_with_run_at("not-a-time"));
        let err = validate_config(&c).unwrap_err();
        assert!(
            format!("{err:#}").contains("ai.consolidation.run_at"),
            "got: {err:#}"
        );
    }

    #[test]
    fn validate_accepts_well_formed_run_at() {
        let mut c = Configuration::test_default();
        c.ai = Some(ai_with_run_at("04:00"));
        validate_config(&c).unwrap();
    }
}
