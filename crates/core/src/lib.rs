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
pub mod doener;
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
    pub doener: Arc<crate::doener::DoenerClient>,
    pub whisper: Option<Arc<dyn WhisperSender>>,
    pub data_dir: PathBuf,
    /// Optional override for the 7TV emote glossary TOML. Production leaves
    /// this `None` so the baked glossary is used; integration tests inject
    /// custom fixtures.
    pub emote_glossary_override: Option<String>,
    /// Shared connectivity flag flipped by the latency monitor. Read by the
    /// web dashboard's `/healthz` endpoint.
    pub irc_connected: Arc<std::sync::atomic::AtomicBool>,
    /// Optional callback that spawns the embedded web dashboard task.
    ///
    /// `core` no longer depends on the `web` crate (the cycle would break
    /// builds), so the binary is the only place that can construct
    /// `WebState` + spawn `run_web`. Tests leave this `None` and the bot
    /// runs without a dashboard.
    ///
    /// The closure receives the shared shutdown `Notify` (so axum's
    /// graceful-shutdown future fires when handlers wind down) and is
    /// expected to return a `JoinHandle` that resolves when the web task
    /// exits.
    pub web_spawner: Option<WebSpawner>,
    /// Shared ping manager. Constructed by the bin so the same `Arc` can be
    /// handed both to the IRC handler set (via `SpawnDeps`) and to
    /// `WebState` (for the dashboard CRUD routes).
    pub ping_manager: Arc<tokio::sync::RwLock<crate::ping::PingManager>>,
    /// Shared v2 memory store. Constructed by the bin so the bot's `!ai`
    /// turn / dreamer ritual and the dashboard memory editor write through
    /// the *same* per-path mutex map. Two independent stores against the
    /// same on-disk tree would silently race past each other's locks.
    pub memory_store: crate::ai::memory::store::MemoryStore,
}

pub type WebSpawner =
    Box<dyn FnOnce(Arc<tokio::sync::Notify>) -> tokio::task::JoinHandle<()> + Send + 'static>;

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
        doener,
        whisper,
        data_dir,
        emote_glossary_override,
        irc_connected,
        web_spawner,
        ping_manager,
        memory_store,
    } = services;

    let schedules_enabled = !config.schedules.is_empty();

    let leaderboard = Arc::new(tokio::sync::RwLock::new(load_leaderboard(&data_dir).await));

    let suspension_manager = Arc::new(suspend::SuspensionManager::new());

    // Aviation is consumed by the flight tracker; clone first so commands (!up/!fl) also get it.
    let aviation_for_commands = aviation.clone();

    let ai_memory_v2 =
        crate::ai::command::build_ai_memory_v2(config.ai.as_ref(), memory_store).await?;
    let transcript = ai_memory_v2.as_ref().map(|m| m.transcript.clone());

    // 7TV emote provider: built once at startup so malformed glossary TOML
    // (baked or test-injected) fails fast instead of silently disabling emotes.
    let emote_provider = match (llm.as_ref(), config.ai.as_ref()) {
        (Some(_), Some(ai)) if ai.emotes.enabled => {
            let glossary_toml = emote_glossary_override
                .as_deref()
                .unwrap_or(crate::twitch::seventv::BAKED_GLOSSARY_TOML);
            let provider =
                crate::twitch::seventv::SevenTvEmoteProvider::new(ai.emotes.clone(), glossary_toml)
                    .wrap_err("Failed to initialize 7TV emote provider")?;
            tracing::info!("7TV emote glossary prompt grounding enabled");
            Some(Arc::new(provider))
        }
        _ => None,
    };

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
        doener,
        leaderboard,
        ping_manager,
        suspension_manager,
        llm,
        ai_memory_v2,
        transcript,
        whisper,
        aviation,
        aviation_for_commands,
        emote_provider,
        irc_connected: irc_connected.clone(),
    });

    let shutdown_notify = handlers.shutdown_notify.clone();

    // Optional embedded web dashboard. The bin builds and supplies the
    // spawn closure so this crate stays independent of `twitch_1337_web`.
    let web_handle = web_spawner.map(|spawner| spawner(shutdown_notify.clone()));

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
    crate::twitch::handlers::spawn::await_shutdown(handlers, shutdown).await;

    // After handler shutdown, drain the web task. The graceful shutdown future
    // is wired to the same `shutdown_notify` notified by `await_shutdown`, so
    // axum::serve has already begun winding down.
    if let Some(handle) = web_handle
        && let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await
    {
        tracing::warn!(target: "twitch_1337_web", ?e, "Web task did not shut down within 5s");
    }

    info!("Bot shutdown complete");
    Ok(())
}
