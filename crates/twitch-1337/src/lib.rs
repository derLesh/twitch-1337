//! Twitch IRC bot library crate.
//!
//! The binary (`src/main.rs`) reads config, builds a production
//! `TwitchIRCClient`, and runs the bot. Integration tests (`tests/`) use a
//! fake transport, fake clock, and fake LLM against the same handlers.

pub mod ai;
pub mod aviation;
pub mod commands;
pub mod config;
pub mod cooldown;
pub mod database;
pub mod llm_factory;
pub mod ping;
pub mod suspend;
pub mod twitch;
pub mod util;

use std::path::PathBuf;
use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use llm::LlmClient;
use tokio::sync::{mpsc::UnboundedReceiver, oneshot};
use tracing::info;
use twitch_irc::{
    TwitchIRCClient,
    login::{LoginCredentials, RefreshingLoginCredentials},
    message::ServerMessage,
    transport::Transport,
};

use crate::{
    aviation::AviationClient,
    config::Configuration,
    twitch::handlers::{
        spawn::{SpawnDeps, spawn_handlers},
        tracker_1337::{TARGET_HOUR, TARGET_MINUTE, load_leaderboard},
    },
    twitch::whisper::WhisperSender,
    util::clock::Clock,
};

pub type AuthenticatedLoginCredentials =
    RefreshingLoginCredentials<crate::twitch::token_storage::FileBasedTokenStorage>;

/// Generic alias for any authenticated Twitch IRC client. The production
/// default is `SecureTCPTransport` + file-backed refreshing credentials.
pub type AuthenticatedTwitchClient<
    T = twitch_irc::SecureTCPTransport,
    L = AuthenticatedLoginCredentials,
> = TwitchIRCClient<T, L>;

pub use ai::chat_history::{
    ChatHistory, ChatHistoryBuffer, ChatHistoryEntry, ChatHistoryPage, ChatHistoryQuery,
    ChatHistorySource, DEFAULT_HISTORY_LENGTH, MAX_HISTORY_LENGTH, MAX_TOOL_RESULT_MESSAGES,
};
pub use config::{load_configuration, validate_config};
pub use twitch::{
    handlers::tracker_1337::PersonalBest,
    setup::{setup_and_verify_twitch_client, setup_twitch_client},
    token_storage::FileBasedTokenStorage,
};
pub use util::{
    APP_USER_AGENT, MAX_RESPONSE_LENGTH, ensure_data_dir, get_config_path, get_data_dir,
    install_crypto_provider, parse_flight_duration, resolve_berlin_time,
    telemetry::install_tracing, truncate_response,
};

/// Test-overridable services injected into [`run_bot`].
///
/// Production wires real implementations; integration tests wire fakes.
pub struct Services {
    pub clock: Arc<dyn Clock>,
    pub llm: Option<Arc<dyn LlmClient>>,
    pub aviation: Option<AviationClient>,
    pub whisper: Option<Arc<dyn WhisperSender>>,
    pub data_dir: PathBuf,
}

/// Run the bot until `shutdown` fires or a handler exits.
///
/// Shared entry point for `main.rs` (production) and integration tests.
/// Generic over `Transport` and `LoginCredentials` so tests can substitute a
/// `FakeTransport` without touching production code paths.
pub async fn run_bot<T, L>(
    client: Arc<TwitchIRCClient<T, L>>,
    incoming: UnboundedReceiver<ServerMessage>,
    config: Configuration,
    services: Services,
    shutdown: oneshot::Receiver<()>,
) -> Result<()>
where
    T: Transport + Send + Sync + 'static,
    L: LoginCredentials + Send + Sync + 'static,
{
    let Services {
        clock,
        llm,
        aviation,
        whisper,
        data_dir,
    } = services;

    let schedules_enabled = !config.schedules.is_empty();

    let leaderboard = Arc::new(tokio::sync::RwLock::new(load_leaderboard(&data_dir).await));

    let ping_manager = Arc::new(tokio::sync::RwLock::new(
        ping::PingManager::load(&data_dir).wrap_err("Failed to load ping manager")?,
    ));

    let suspension_manager = Arc::new(suspend::SuspensionManager::new());

    // Aviation is consumed by the flight tracker; clone first so commands (!up/!fl) also get it.
    let aviation_for_commands = aviation.clone();

    let ai_memory_v2 =
        crate::ai::command::build_ai_memory_v2(config.ai.as_ref(), &data_dir).await?;
    let transcript = ai_memory_v2.as_ref().map(|m| m.transcript.clone());

    // Clone before moving into SpawnDeps so the dreamer ritual can also use them.
    let ai_memory_v2_for_ritual = ai_memory_v2.clone();
    let llm_for_ritual = llm.clone();
    let ai_config_for_ritual = config.ai.clone();
    let channel_for_ritual = config.twitch.channel.clone();

    let handlers = spawn_handlers(SpawnDeps {
        client,
        incoming,
        config,
        clock,
        data_dir,
        leaderboard,
        ping_manager,
        suspension_manager,
        llm,
        ai_memory_v2,
        transcript,
        whisper,
        aviation,
        aviation_for_commands,
    });

    let shutdown_notify = handlers.shutdown_notify.clone();

    // Daily dreamer ritual.
    if let (Some(llm), Some(mem)) = (llm_for_ritual.as_ref(), &ai_memory_v2_for_ritual)
        && let Some(ref ai) = ai_config_for_ritual
        && ai.dreamer.enabled
    {
        let run_at = chrono::NaiveTime::parse_from_str(&ai.dreamer.run_at, "%H:%M")
            .expect("ai.dreamer.run_at validated at config load");
        crate::ai::memory::ritual::spawn_ritual(
            llm.clone(),
            mem.store.clone(),
            mem.transcript.clone(),
            crate::ai::memory::ritual::RitualConfig {
                model: ai.dreamer.model.clone().unwrap_or_else(|| ai.model.clone()),
                reasoning_effort: ai
                    .dreamer
                    .reasoning_effort
                    .clone()
                    .or_else(|| ai.reasoning_effort.clone()),
                run_at,
                timeout_secs: ai.dreamer.timeout_secs,
                max_rounds: ai.dreamer.max_rounds,
                max_writes_per_turn: ai.max_writes_per_turn,
                inject_byte_budget: ai.memory.inject_byte_budget,
                channel: channel_for_ritual,
            },
            shutdown_notify.clone(),
        );
        tracing::info!(run_at = %ai.dreamer.run_at, "Daily AI memory dreamer ritual scheduled");
    }

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
    info!("Bot is running. Press Ctrl+C to stop.");

    crate::twitch::handlers::spawn::await_shutdown(handlers, shutdown).await;

    info!("Bot shutdown complete");
    Ok(())
}
