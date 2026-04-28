//! Handler spawn factory.
//!
//! `run_bot` (in `src/lib.rs`) calls [`spawn_handlers`] once with everything
//! the long-running tasks need. The returned [`HandlerSet`] owns every
//! `JoinHandle` plus the shared `Arc<Notify>` post-spawn code uses to drain
//! the scheduled-message handler on shutdown.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU32},
};

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

    // 1337 tracker.
    pub leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,

    // Generic commands.
    pub ping_manager: Arc<RwLock<ping::PingManager>>,
    pub suspension_manager: Arc<SuspensionManager>,
    pub llm: Option<Arc<dyn ai::llm::LlmClient>>,
    pub ai_memory: Option<ai::command::AiMemory>,
    pub whisper: Option<Arc<dyn WhisperSender>>,

    // Flight tracker.
    pub aviation: Option<AviationClient>,
    pub aviation_for_commands: Option<AviationClient>,
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
        leaderboard,
        ping_manager,
        suspension_manager,
        llm,
        ai_memory,
        whisper,
        aviation,
        aviation_for_commands,
    } = deps;

    let schedules_enabled = !config.schedules.is_empty();

    // Flight tracker: spawned first so `tracker_tx` exists for the command handler.
    let (tracker_tx, flight_tracker) = match aviation {
        Some(av) => {
            let (tx, rx) = mpsc::channel::<aviation::TrackerCommand>(32);
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
        None => (None, tokio::spawn(std::future::pending::<()>())),
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
        async move {
            run_latency_handler(client, btx, lat).await;
        }
    });

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
                ai_memory,
                leaderboard,
                ping_manager,
                hidden_admin_ids: config.twitch.hidden_admins.clone(),
                default_cooldown: Duration::from_secs(config.pings.cooldown),
                pings_public: config.pings.public,
                cooldowns: config.cooldowns.clone(),
                tracker_tx,
                aviation_client: aviation_for_commands,
                whisper,
                admin_channel: config.twitch.admin_channel.clone(),
                bot_username: config.twitch.username.clone(),
                channel: config.twitch.channel.clone(),
                data_dir: data_dir.clone(),
                suspension_manager: suspension_manager.clone(),
                suspend: config.suspend.clone(),
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
            if has_sched {
                shutdown_notify.notify_waiters();
                if let Err(e) = timeout(Duration::from_secs(5), &mut sched).await {
                    warn!(?e, "Scheduled message handler did not shut down within 5s");
                }
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
