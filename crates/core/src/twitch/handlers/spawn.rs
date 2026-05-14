//! Handler spawn factory.
//!
//! `run_bot` (in `src/lib.rs`) calls [`spawn_handlers`] once with everything
//! the long-running tasks need. The returned [`HandlerSet`] owns every
//! `JoinHandle` plus the shared `Arc<Notify>` post-spawn code uses to drain
//! the scheduled-message handler on shutdown.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32},
    },
};

use llm::LlmClient;
use tokio::{
    sync::{Notify, RwLock, broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{Duration, timeout},
};
use tracing::{error, info, warn};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::ServerMessage, transport::Transport,
};

use crate::{
    ai,
    aviation::{self, AviationClient},
    config::Configuration,
    database, ping,
    suspend::SuspensionManager,
    twitch::{
        handlers::{
            commands::{CommandHandlerConfig, run_generic_command_handler},
            latency::run_latency_handler,
            router::run_message_router,
            schedules::{
                load_schedules_from_config, run_config_watcher_service,
                run_scheduled_message_handler,
            },
            tracker_1337::{PersonalBest, run_1337_handler},
        },
        whisper::WhisperSender,
    },
    util::clock::Clock,
};

/// All `JoinHandle`s plus the shared state `run_bot` keeps after spawn.
pub(crate) struct HandlerSet {
    pub router: JoinHandle<()>,
    pub latency: JoinHandle<()>,
    pub tracker_1337: JoinHandle<()>,
    pub generic_commands: JoinHandle<()>,
    pub flight_tracker: JoinHandle<()>,
    pub config_watcher: Option<JoinHandle<()>>,
    pub scheduled_messages: Option<JoinHandle<()>>,
    /// Shared notify so consolidation/shutdown code can drain in-flight
    /// scheduled-message sends. Cloned, not consumed.
    pub shutdown_notify: Arc<Notify>,
}

/// Inputs for [`spawn_handlers`]. Grouped by handler.
pub(crate) struct SpawnDeps<T: Transport, L: LoginCredentials> {
    // Shared.
    pub client: Arc<TwitchIRCClient<T, L>>,
    pub incoming: tokio::sync::mpsc::UnboundedReceiver<ServerMessage>,
    pub config: Configuration,
    pub clock: Arc<dyn Clock>,
    pub data_dir: PathBuf,
    pub doener: Arc<crate::doener::DoeneratlasClient>,

    // 1337 tracker.
    pub leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,

    // Generic commands.
    pub ping_manager: Arc<RwLock<ping::PingManager>>,
    pub suspension_manager: Arc<SuspensionManager>,
    pub llm: Option<Arc<dyn LlmClient>>,
    pub ai_memory_v2: Option<ai::command::AiMemoryV2>,
    pub transcript: Option<crate::ai::memory::transcript::TranscriptWriter>,
    pub whisper: Option<Arc<dyn WhisperSender>>,

    // Flight tracker.
    pub aviation: Option<AviationClient>,
    pub aviation_for_commands: Option<AviationClient>,
    /// Pre-created sender half of the flight-tracker channel. Must be `Some`
    /// when `aviation` is `Some`, and `None` otherwise.
    pub aviation_tracker_tx: Option<mpsc::Sender<aviation::TrackerCommand>>,
    /// Pre-created receiver half of the flight-tracker channel. Consumed by
    /// `spawn_handlers` to start the flight-tracker task. Must be `Some` when
    /// `aviation` is `Some`, and `None` otherwise.
    pub aviation_tracker_rx: Option<mpsc::Receiver<aviation::TrackerCommand>>,

    // AI emote grounding.
    pub emote_provider: Option<Arc<crate::twitch::seventv::SevenTvEmoteProvider>>,

    // Shared IRC connectivity flag (latency monitor flips, web /healthz reads).
    pub irc_connected: Arc<AtomicBool>,

    // Dashboard-managed runtime settings (cooldowns, pings.cooldown, pings.public).
    pub settings: crate::settings::SettingsHandle,
}

/// Spawn every long-running handler task in the order they currently
/// appear in `run_bot`. Returns a [`HandlerSet`] owning all handles plus
/// shared `Notify` / `tracker_tx`.
pub(crate) fn spawn_handlers<T, L>(deps: SpawnDeps<T, L>) -> HandlerSet
where
    T: Transport + Send + Sync + 'static,
    L: LoginCredentials + Send + Sync + 'static,
{
    let SpawnDeps {
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
        aviation_tracker_tx,
        aviation_tracker_rx,
        emote_provider,
        irc_connected,
        settings,
    } = deps;

    let schedules_enabled = !config.schedules.is_empty();

    // Flight tracker: spawned first so `tracker_tx` exists for the command handler.
    // The channel is pre-created by the caller (production: main.rs; tests: TestBotBuilder)
    // so the sender Arc can be shared with WebState before handlers are spawned.
    let (tracker_tx, flight_tracker) = match (aviation, aviation_tracker_rx) {
        (Some(av), Some(rx)) => {
            let tx = aviation_tracker_tx.expect("tracker_tx must be Some when aviation is Some");
            let handle = tokio::spawn({
                let client = client.clone();
                let channel = config.twitch.channel.clone();
                let dir = data_dir.clone();
                let clk = clock.clone();
                async move {
                    aviation::run_flight_tracker(rx, client, channel, av, dir, clk).await;
                }
            });
            (Some(tx), handle)
        }
        _ => (None, tokio::spawn(std::future::pending::<()>())),
    };

    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    let router = tokio::spawn(run_message_router(incoming, broadcast_tx.clone()));

    // Notify lets the scheduled-message handler drain in-flight sends before exiting.
    let shutdown_notify = Arc::new(Notify::new());

    let (config_watcher, scheduled_messages) = if schedules_enabled {
        info!(
            count = config.schedules.len(),
            "Schedules configured, starting scheduled message system"
        );
        let initial_schedules = load_schedules_from_config(&config);
        info!(
            loaded = initial_schedules.len(),
            "Loaded initial schedules from config"
        );

        let mut cache = database::ScheduleCache::new();
        cache.update(initial_schedules);
        let schedule_cache = Arc::new(RwLock::new(cache));

        let watcher = tokio::spawn({
            let cache = schedule_cache.clone();
            async move {
                run_config_watcher_service(cache).await;
            }
        });

        let handler = tokio::spawn({
            let client = client.clone();
            let cache = schedule_cache.clone();
            let channel = config.twitch.channel.clone();
            let notify = shutdown_notify.clone();
            let clk = clock.clone();
            async move {
                run_scheduled_message_handler(client, cache, channel, notify, clk).await;
            }
        });

        (Some(watcher), Some(handler))
    } else {
        info!("No schedules configured, scheduled messages disabled");
        (None, None)
    };

    let latency_value = Arc::new(AtomicU32::new(config.twitch.expected_latency));

    let latency = tokio::spawn({
        let client = client.clone();
        let btx = broadcast_tx.clone();
        let lat = latency_value.clone();
        let conn = irc_connected.clone();
        async move {
            run_latency_handler(client, btx, lat, conn).await;
        }
    });

    let _transcript_tap = if let Some(t) = transcript {
        let rx = broadcast_tx.subscribe();
        let w = Arc::new(t);
        let ch = config.twitch.channel.clone();
        Some(tokio::spawn(async move {
            crate::twitch::handlers::transcript::run_transcript_tap(rx, w, ch).await;
        }))
    } else {
        None
    };

    let tracker_1337 = tokio::spawn({
        let btx = broadcast_tx.clone();
        let client = client.clone();
        let channel = config.twitch.channel.clone();
        let lat = latency_value.clone();
        let lb = leaderboard.clone();
        let clk = clock.clone();
        let dd = data_dir.clone();
        async move {
            run_1337_handler(btx, client, channel, lat, lb, clk, dd).await;
        }
    });

    let generic_commands = tokio::spawn({
        let btx = broadcast_tx.clone();
        let client = client.clone();
        async move {
            run_generic_command_handler(CommandHandlerConfig {
                broadcast_tx: btx,
                client,
                ai_config: config.ai.clone(),
                llm,
                ai_memory_v2,
                leaderboard,
                ping_manager,
                hidden_admin_ids: config.twitch.hidden_admins.clone(),
                settings: settings.clone(),
                tracker_tx,
                aviation_client: aviation_for_commands,
                whisper,
                admin_channel: config.twitch.admin_channel.clone(),
                ai_channel: config.twitch.ai_channel.clone(),
                bot_username: config.twitch.username.clone(),
                channel: config.twitch.channel.clone(),
                data_dir: data_dir.clone(),
                doener: doener.clone(),
                suspension_manager: suspension_manager.clone(),
                suspend: config.suspend.clone(),
                emote_provider,
            })
            .await;
        }
    });

    HandlerSet {
        router,
        latency,
        tracker_1337,
        generic_commands,
        flight_tracker,
        config_watcher,
        scheduled_messages,
        shutdown_notify,
    }
}

/// Awaits whichever happens first: shutdown signal, or any handler exiting.
///
/// On shutdown, notifies `shutdown_notify` (so the scheduled-message handler
/// drains in-flight `say()` calls) and waits up to 5s for the scheduled
/// handler before returning.
///
/// Optional handlers (`config_watcher`, `scheduled_messages`) get replaced
/// with `tokio::spawn(std::future::pending::<()>())` when absent so every
/// `select!` arm is a real `JoinHandle<()>`. This mirrors the existing
/// fallback used for `flight_tracker` when no aviation client is wired.
pub(crate) async fn await_shutdown(handlers: HandlerSet, shutdown: oneshot::Receiver<()>) {
    let HandlerSet {
        router,
        latency,
        tracker_1337,
        generic_commands,
        flight_tracker,
        config_watcher,
        scheduled_messages,
        shutdown_notify,
    } = handlers;

    let watcher = config_watcher.unwrap_or_else(|| tokio::spawn(std::future::pending::<()>()));
    let has_sched = scheduled_messages.is_some();
    let mut sched =
        scheduled_messages.unwrap_or_else(|| tokio::spawn(std::future::pending::<()>()));

    tokio::select! {
        _ = shutdown => {
            info!("Shutdown signal received, exiting gracefully");
            // Always wake any waiters (scheduled messages, web dashboard).
            shutdown_notify.notify_waiters();
            if has_sched
                && let Err(e) = timeout(Duration::from_secs(5), &mut sched).await
            {
                warn!(?e, "Scheduled message handler did not shut down within 5s");
            }
        }
        result = router => { error!("Message router exited unexpectedly: {result:?}"); }
        result = watcher => { error!("Config watcher service exited unexpectedly: {result:?}"); }
        result = tracker_1337 => { error!("1337 handler exited unexpectedly: {result:?}"); }
        result = generic_commands => { error!("Generic Command Handler exited unexpectedly: {result:?}"); }
        result = latency => { error!("Latency handler exited unexpectedly: {result:?}"); }
        result = flight_tracker => { error!("Flight tracker exited unexpectedly: {result:?}"); }
        result = &mut sched => { error!("Scheduled message handler exited unexpectedly: {result:?}"); }
    }
}
