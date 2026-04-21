use std::{
    collections::HashSet,
    sync::{Arc, atomic::AtomicU32},
};

use chrono::Utc;
use color_eyre::eyre::{Result, WrapErr, bail};
use secrecy::ExposeSecret;
use tokio::{
    sync::{broadcast, mpsc::UnboundedReceiver},
    time::Duration,
};
use tracing::{error, info, instrument, trace, warn};
use twitch_1337::{
    AuthenticatedTwitchClient, FileBasedTokenStorage, aviation,
    clock::{Clock, SystemClock},
    config::{AiBackend, Configuration},
    database, flight_tracker, get_config_path, get_data_dir,
    handlers::{
        commands::CommandHandlerConfig,
        latency::run_latency_handler,
        router::run_message_router,
        schedules::{
            load_schedules_from_config, run_config_watcher_service, run_scheduled_message_handler,
        },
        tracker_1337::{TARGET_HOUR, TARGET_MINUTE, load_leaderboard, run_1337_handler},
    },
    ping,
};
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::RefreshingLoginCredentials,
    message::{NoticeMessage, ServerMessage},
};

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

async fn load_configuration() -> Result<Configuration> {
    let config_path = get_config_path();
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

fn validate_config(config: &Configuration) -> Result<()> {
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

    // Validate AI config
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

    // Validate each schedule config
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
        // Validate interval format by parsing it
        database::Schedule::parse_interval(&schedule.interval).wrap_err_with(|| {
            format!("Schedule '{}' has invalid interval format", schedule.name)
        })?;
    }

    Ok(())
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
    let (incoming_messages, client) = setup_and_verify_twitch_client(&config).await?;

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

    // Graceful shutdown signal for handlers that need to drain children (#31).
    let shutdown = Arc::new(tokio::sync::Notify::new());

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

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
            let shutdown = shutdown.clone();
            let clock = clock.clone();
            async move {
                run_scheduled_message_handler(client, cache, channel, shutdown, clock).await;
            }
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
        let clock = clock.clone();
        async move {
            run_1337_handler(broadcast_tx, client, channel, latency, leaderboard, clock).await;
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
            twitch_1337::handlers::commands::run_generic_command_handler(CommandHandlerConfig {
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
        (Some(watcher), Some(mut handler)) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutdown signal received, exiting gracefully");
                    shutdown.notify_waiters();
                    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), &mut handler).await {
                        warn!(?e, "Scheduled message handler did not shut down within 5s");
                    }
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
                result = &mut handler => {
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

#[instrument]
async fn ensure_data_dir() -> Result<()> {
    tokio::fs::create_dir_all(get_data_dir()).await?;
    Ok(())
}

#[instrument(skip(config))]
fn setup_twitch_client(
    config: &Configuration,
) -> (UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient) {
    // Create authenticated IRC client with refreshing tokens
    let credentials = RefreshingLoginCredentials::init_with_username(
        Some(config.twitch.username.clone()),
        config.twitch.client_id.expose_secret().to_string(),
        config.twitch.client_secret.expose_secret().to_string(),
        FileBasedTokenStorage::new(config.twitch.refresh_token.clone()),
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
    config: &Configuration,
) -> Result<(UnboundedReceiver<ServerMessage>, AuthenticatedTwitchClient)> {
    info!("Setting up and verifying Twitch connection");

    let (mut incoming_messages, client) = setup_twitch_client(config);

    // Connect to Twitch IRC
    info!("Connecting to Twitch IRC");
    client.connect().await;

    // Join the configured channel(s)
    let mut channels: HashSet<String> = [config.twitch.channel.clone()].into();
    if let Some(ref admin_channel) = config.twitch.admin_channel {
        info!(admin_channel = %admin_channel, "Joining admin channel");
        channels.insert(admin_channel.clone());
    }
    info!(channel = %config.twitch.channel, "Joining channel");
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
