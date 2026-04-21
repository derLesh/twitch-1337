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

fn default_max_memories() -> usize {
    50
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
    /// Enable persistent AI memory (default: false)
    #[serde(default)]
    pub memory_enabled: bool,
    /// Maximum number of stored memories (default: 50)
    #[serde(default = "default_max_memories")]
    pub max_memories: usize,
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
    pub ai: Option<AiConfig>,
    #[serde(default)]
    pub schedules: Vec<ScheduleConfig>,
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
        && ai.memory_enabled
        && !(1..=200).contains(&ai.max_memories)
    {
        bail!(
            "ai.max_memories must be between 1 and 200 (got {})",
            ai.max_memories
        );
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
