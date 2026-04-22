//! Twitch IRC bot library crate.
//!
//! The binary (`src/main.rs`) reads config, builds a production
//! `TwitchIRCClient`, and runs the bot. Integration tests (`tests/`) use a
//! fake transport, fake clock, and fake LLM against the same handlers.

pub mod aviation;
pub mod clock;
pub mod commands;
pub mod config;
pub mod cooldown;
pub mod database;
pub mod flight_tracker;
pub mod handlers;
pub mod llm;
pub mod memory;
pub mod ping;
pub mod prefill;
pub mod suspend;
pub mod telemetry;
pub mod token_storage;
pub mod twitch_setup;
pub mod util;

use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicU32};

use eyre::{Result, WrapErr as _};
use tokio::sync::{broadcast, mpsc::UnboundedReceiver, oneshot};
use tokio::time::Duration;
use tracing::{error, info, warn};
use twitch_irc::{
    TwitchIRCClient,
    login::{LoginCredentials, RefreshingLoginCredentials},
    message::ServerMessage,
    transport::Transport,
};

use crate::{
    aviation::AviationClient,
    clock::Clock,
    config::Configuration,
    handlers::{
        commands::{CommandHandlerConfig, run_generic_command_handler},
        latency::run_latency_handler,
        router::run_message_router,
        schedules::{
            load_schedules_from_config, run_config_watcher_service, run_scheduled_message_handler,
        },
        tracker_1337::{TARGET_HOUR, TARGET_MINUTE, load_leaderboard, run_1337_handler},
    },
    llm::LlmClient,
};

/// Generic alias for any authenticated Twitch IRC client. The production
/// default is `SecureTCPTransport` + file-backed refreshing credentials.
pub type AuthenticatedTwitchClient<
    T = twitch_irc::SecureTCPTransport,
    L = RefreshingLoginCredentials<crate::token_storage::FileBasedTokenStorage>,
> = TwitchIRCClient<T, L>;

pub use config::{load_configuration, validate_config};
pub use handlers::tracker_1337::PersonalBest;
pub use telemetry::install_tracing;
pub use token_storage::FileBasedTokenStorage;
pub use twitch_setup::{setup_and_verify_twitch_client, setup_twitch_client};
pub use util::{
    APP_USER_AGENT, ChatHistory, MAX_RESPONSE_LENGTH, ensure_data_dir, get_config_path,
    get_data_dir, parse_flight_duration, resolve_berlin_time, truncate_response,
};

/// Test-overridable services injected into [`run_bot`].
///
/// Production wires real implementations; integration tests wire fakes.
pub struct Services {
    pub clock: Arc<dyn Clock>,
    pub llm: Option<Arc<dyn LlmClient>>,
    pub aviation: Option<AviationClient>,
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
        data_dir,
    } = services;

    let schedules_enabled = !config.schedules.is_empty();

    let leaderboard = Arc::new(tokio::sync::RwLock::new(load_leaderboard(&data_dir).await));

    let ping_manager = Arc::new(tokio::sync::RwLock::new(
        ping::PingManager::load(&data_dir).wrap_err("Failed to load ping manager")?,
    ));

    // Aviation is consumed by the flight tracker; clone first so commands (!up/!fl) also get it.
    let aviation_for_commands = aviation.clone();

    let (tracker_tx, handler_flight_tracker) = match aviation {
        Some(av) => {
            let (tx, rx) = tokio::sync::mpsc::channel::<flight_tracker::TrackerCommand>(32);
            let handle = tokio::spawn({
                let client = client.clone();
                let channel = config.twitch.channel.clone();
                let dir = data_dir.clone();
                let clk = clock.clone();
                async move {
                    flight_tracker::run_flight_tracker(rx, client, channel, av, dir, clk).await;
                }
            });
            (Some(tx), handle)
        }
        None => (None, tokio::spawn(std::future::pending::<()>())),
    };

    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    let router_handle = tokio::spawn(run_message_router(incoming, broadcast_tx.clone()));

    // Notify lets the scheduled-message handler drain in-flight sends before exiting.
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());

    let (watcher_service, handler_scheduled_messages) = if schedules_enabled {
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
        let schedule_cache = Arc::new(tokio::sync::RwLock::new(cache));

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

    let suspension_manager = Arc::new(suspend::SuspensionManager::new());

    let latency = Arc::new(AtomicU32::new(config.twitch.expected_latency));

    let handler_latency = tokio::spawn({
        let client = client.clone();
        let btx = broadcast_tx.clone();
        let lat = latency.clone();
        async move {
            run_latency_handler(client, btx, lat).await;
        }
    });

    let handler_1337 = tokio::spawn({
        let btx = broadcast_tx.clone();
        let client = client.clone();
        let channel = config.twitch.channel.clone();
        let lat = latency.clone();
        let lb = leaderboard.clone();
        let clk = clock.clone();
        let dd = data_dir.clone();
        async move {
            run_1337_handler(btx, client, channel, lat, lb, clk, dd).await;
        }
    });

    let handler_generic_commands = tokio::spawn({
        let btx = broadcast_tx.clone();
        let client = client.clone();
        async move {
            run_generic_command_handler(CommandHandlerConfig {
                broadcast_tx: btx,
                client,
                ai_config: config.ai.clone(),
                llm,
                leaderboard,
                ping_manager,
                hidden_admin_ids: config.twitch.hidden_admins.clone(),
                default_cooldown: Duration::from_secs(config.pings.cooldown),
                pings_public: config.pings.public,
                cooldowns: config.cooldowns.clone(),
                tracker_tx,
                aviation_client: aviation_for_commands,
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

    match (watcher_service, handler_scheduled_messages) {
        (Some(watcher), Some(mut sched_handler)) => {
            tokio::select! {
                _ = shutdown => {
                    info!("Shutdown signal received, exiting gracefully");
                    shutdown_notify.notify_waiters();
                    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), &mut sched_handler).await {
                        warn!(?e, "Scheduled message handler did not shut down within 5s");
                    }
                }
                result = router_handle => { error!("Message router exited unexpectedly: {result:?}"); }
                result = watcher => { error!("Config watcher service exited unexpectedly: {result:?}"); }
                result = handler_1337 => { error!("1337 handler exited unexpectedly: {result:?}"); }
                result = handler_generic_commands => { error!("Generic Command Handler exited unexpectedly: {result:?}"); }
                result = handler_latency => { error!("Latency handler exited unexpectedly: {result:?}"); }
                result = handler_flight_tracker => { error!("Flight tracker exited unexpectedly: {result:?}"); }
                result = &mut sched_handler => { error!("Scheduled message handler exited unexpectedly: {result:?}"); }
            }
        }
        _ => {
            tokio::select! {
                _ = shutdown => {
                    info!("Shutdown signal received, exiting gracefully");
                }
                result = router_handle => { error!("Message router exited unexpectedly: {result:?}"); }
                result = handler_1337 => { error!("1337 handler exited unexpectedly: {result:?}"); }
                result = handler_generic_commands => { error!("Generic Command Handler exited unexpectedly: {result:?}"); }
                result = handler_latency => { error!("Latency handler exited unexpectedly: {result:?}"); }
                result = handler_flight_tracker => { error!("Flight tracker exited unexpectedly: {result:?}"); }
            }
        }
    }

    info!("Bot shutdown complete");
    Ok(())
}
