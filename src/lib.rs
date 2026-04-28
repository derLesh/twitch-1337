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
pub mod ping;
pub mod suspend;
pub mod twitch;
pub mod util;

use std::path::PathBuf;
use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use tokio::sync::{mpsc::UnboundedReceiver, oneshot};
use tokio::time::Duration;
use tracing::info;
use twitch_irc::{
    TwitchIRCClient,
    login::{LoginCredentials, RefreshingLoginCredentials},
    message::ServerMessage,
    transport::Transport,
};

use crate::{
    ai::llm::LlmClient,
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
pub use twitch::handlers::tracker_1337::PersonalBest;
pub use twitch::setup::{setup_and_verify_twitch_client, setup_twitch_client};
pub use twitch::tls::install_crypto_provider;
pub use twitch::token_storage::FileBasedTokenStorage;
pub use util::telemetry::install_tracing;
pub use util::{
    APP_USER_AGENT, MAX_RESPONSE_LENGTH, ensure_data_dir, get_config_path, get_data_dir,
    parse_flight_duration, resolve_berlin_time, truncate_response,
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

    // Build the memory bundle once so both `!ai` (extraction) and the
    // daily consolidation task share the same store handle + path.
    // Effective fallback resolution:
    // - model: extraction -> [ai], consolidation -> extraction -> [ai]
    // - reasoning_effort: extraction -> [ai], consolidation -> extraction -> [ai]
    let crate::ai::command::AiMemoryBundle {
        ai_memory,
        consolidation_model,
        consolidation_reasoning_effort,
    } = crate::ai::command::build_ai_memory(config.ai.as_ref(), llm.as_ref(), &data_dir);

    // Capture the store handle + path for the consolidation spawn before
    // `ai_memory` is moved into the command handler. Both `!ai` extraction
    // (in-handler) and consolidation (below) share these clones.
    let consolidation_handle = ai_memory.as_ref().map(|m| {
        (
            m.config.store.clone(),
            m.config.path.clone(),
            llm.as_ref()
                .expect("ai_memory only built when llm is Some")
                .clone(),
        )
    });
    let consolidation_settings = config.ai.as_ref().map(|a| a.consolidation.clone());

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
        ai_memory,
        whisper,
        aviation,
        aviation_for_commands,
    });

    let shutdown_notify = handlers.shutdown_notify.clone();

    // Daily memory consolidation pass. Shares the memory store handle with
    // the extractor so the pass sees any writes made since the last run, and
    // reuses `shutdown_notify` so Ctrl+C aborts the scheduler mid-sleep.
    if let (Some(ai), Some((store, path, llm_client)), Some(model)) = (
        consolidation_settings,
        consolidation_handle,
        consolidation_model,
    ) && ai.enabled
    {
        // Format is validated in `validate_config`, so this cannot fail here.
        let run_at = chrono::NaiveTime::parse_from_str(&ai.run_at, "%H:%M")
            .expect("ai.consolidation.run_at is validated at config load");
        ai::memory::spawn_consolidation(
            llm_client,
            ai::memory::consolidation::ConsolidationLlmConfig {
                model,
                reasoning_effort: consolidation_reasoning_effort,
            },
            store,
            path,
            run_at,
            Duration::from_secs(ai.timeout),
            shutdown_notify.clone(),
        );
        info!(
            run_at = %ai.run_at,
            "Daily AI memory consolidation scheduled"
        );
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
