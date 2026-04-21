use std::{path::Path, sync::Arc};

use color_eyre::eyre::{Result, WrapErr};
use tracing::{debug, error, info, instrument, warn};
use twitch_irc::{TwitchIRCClient, login::LoginCredentials, transport::Transport};

use crate::{clock::Clock, config::Configuration, database, get_config_path};

/// Parse a datetime string in ISO 8601 format (YYYY-MM-DDTHH:MM:SS).
pub(crate) fn parse_datetime(s: &str) -> Result<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").wrap_err_with(|| {
        format!(
            "Invalid datetime format '{}' (expected YYYY-MM-DDTHH:MM:SS)",
            s
        )
    })
}

/// Parse a time string in HH:MM format.
pub(crate) fn parse_time(s: &str) -> Result<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M")
        .wrap_err_with(|| format!("Invalid time format '{}' (expected HH:MM)", s))
}

/// Convert a ScheduleConfig from config.toml into a database::Schedule.
pub(crate) fn schedule_config_to_schedule(
    config: &crate::config::ScheduleConfig,
) -> Result<database::Schedule> {
    let interval = database::Schedule::parse_interval(&config.interval)?;

    let start_date = config
        .start_date
        .as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let end_date = config
        .end_date
        .as_ref()
        .map(|s| parse_datetime(s))
        .transpose()?;

    let active_time_start = config
        .active_time_start
        .as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let active_time_end = config
        .active_time_end
        .as_ref()
        .map(|s| parse_time(s))
        .transpose()?;

    let schedule = database::Schedule {
        name: config.name.clone(),
        start_date,
        end_date,
        active_time_start,
        active_time_end,
        interval,
        message: config.message.clone(),
    };

    schedule.validate()?;

    Ok(schedule)
}

/// Load schedules from the Configuration struct.
/// Filters out disabled schedules and validates all enabled ones.
pub fn load_schedules_from_config(config: &Configuration) -> Vec<database::Schedule> {
    let mut schedules = Vec::new();

    for schedule_config in &config.schedules {
        if !schedule_config.enabled {
            debug!(schedule = %schedule_config.name, "Skipping disabled schedule");
            continue;
        }

        match schedule_config_to_schedule(schedule_config) {
            Ok(schedule) => schedules.push(schedule),
            Err(e) => {
                error!(
                    schedule = %schedule_config.name,
                    error = ?e,
                    "Failed to parse schedule config, skipping"
                );
            }
        }
    }

    schedules
}

/// Reload configuration from config.toml and extract schedules.
/// Returns None if config cannot be loaded or parsed.
pub(crate) fn reload_schedules_from_config() -> Option<Vec<database::Schedule>> {
    let config_path = get_config_path();
    let data = match std::fs::read_to_string(&config_path) {
        Ok(data) => data,
        Err(e) => {
            error!(error = ?e, path = %config_path.display(), "Failed to read config for reload");
            return None;
        }
    };

    let config: Configuration = match toml::from_str(&data) {
        Ok(config) => config,
        Err(e) => {
            error!(error = ?e, path = %config_path.display(), "Failed to parse config for reload");
            return None;
        }
    };

    Some(load_schedules_from_config(&config))
}

/// Config file watcher service that monitors config.toml for changes.
/// Uses notify-debouncer-mini with 2 second debounce to avoid rapid reloads.
#[instrument(skip(cache))]
pub async fn run_config_watcher_service(cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>) {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
    use std::time::Duration as StdDuration;

    info!("Config watcher service started");

    // Create channel for receiving file change events
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);

    // Get absolute path to config file for watching
    let config_path = match std::fs::canonicalize(get_config_path()) {
        Ok(p) => p,
        Err(e) => {
            error!(error = ?e, "Failed to get absolute path for config.toml");
            return;
        }
    };

    // Held until this function returns; drop wakes the blocking thread.
    // `spawn_blocking` drop does NOT unpark threads — explicit signal required.
    let (_shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn blocking task for the file watcher (notify is sync)
    let watcher_config_path = config_path.clone();
    let mut watcher_handle = tokio::task::spawn_blocking(move || {
        let tx = tx;
        let config_path = watcher_config_path;

        // Create debouncer with 2 second timeout
        let mut debouncer = match new_debouncer(
            StdDuration::from_secs(2),
            move |res: Result<
                Vec<notify_debouncer_mini::DebouncedEvent>,
                notify_debouncer_mini::notify::Error,
            >| {
                match res {
                    Ok(events) => {
                        for event in events {
                            debug!(path = ?event.path, "File change event received");
                            // Use blocking_send since we're in a sync context
                            if tx.blocking_send(()).is_err() {
                                // Channel closed, watcher should stop
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, "File watcher error");
                    }
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                error!(error = ?e, "Failed to create file watcher");
                return;
            }
        };

        // Watch the config file's parent directory
        let watch_path = config_path.parent().unwrap_or(Path::new("."));
        if let Err(e) = debouncer
            .watcher()
            .watch(watch_path, RecursiveMode::NonRecursive)
        {
            error!(error = ?e, path = ?watch_path, "Failed to watch config directory");
            return;
        }

        info!(path = ?watch_path, "Watching for config changes");

        let _ = shutdown_rx.blocking_recv();
    });

    // Main loop: handle file change events
    loop {
        tokio::select! {
            Some(()) = rx.recv() => {
                info!("Config file changed, reloading schedules");

                if let Some(schedules) = reload_schedules_from_config() {
                    let mut cache_guard = cache.write().await;
                    let old_count = cache_guard.schedules.len();
                    cache_guard.update(schedules);

                    info!(
                        old_count,
                        new_count = cache_guard.schedules.len(),
                        version = cache_guard.version,
                        "Schedules reloaded from config"
                    );
                } else {
                    warn!("Failed to reload config, keeping existing schedules");
                }
            }
            _ = &mut watcher_handle => {
                error!("File watcher task exited unexpectedly");
                break;
            }
        }
    }
}

/// Run a single schedule task.
/// This task will run the schedule at its configured interval,
/// checking if it's still active before each post.
#[instrument(skip(client, cache, channel, clock), fields(schedule = %schedule.name))]
pub(crate) async fn run_schedule_task<T, L>(
    schedule: database::Schedule,
    client: Arc<TwitchIRCClient<T, L>>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
    shutdown: Arc<tokio::sync::Notify>,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    use tokio::time::{Duration, sleep};

    let interval_duration = Duration::from_secs(schedule.interval.num_seconds() as u64);

    info!(
        schedule = %schedule.name,
        interval_seconds = schedule.interval.num_seconds(),
        "Schedule task started"
    );

    loop {
        // Bail on shutdown so in-flight sends below aren't torn apart when the runtime stops.
        tokio::select! {
            () = sleep(interval_duration) => {}
            () = shutdown.notified() => {
                info!(schedule = %schedule.name, "Shutdown received, stopping task");
                break;
            }
        }

        // Check if schedule still exists in cache
        let still_exists = {
            let cache_guard = cache.read().await;
            cache_guard
                .schedules
                .iter()
                .any(|s| s.name == schedule.name)
        };

        if !still_exists {
            info!(
                schedule = %schedule.name,
                "Schedule no longer in cache, stopping task"
            );
            break;
        }

        // Check if schedule is currently active (respects date range and time window)
        let now = clock.now_utc().with_timezone(&chrono_tz::Europe::Berlin);

        if !schedule.is_active(now) {
            debug!(
                schedule = %schedule.name,
                "Schedule not active at current time, skipping post"
            );
            continue;
        }

        // Post the message
        info!(
            schedule = %schedule.name,
            message = %schedule.message,
            "Posting scheduled message"
        );

        if let Err(e) = client.say(channel.clone(), schedule.message.clone()).await {
            error!(
                error = ?e,
                schedule = %schedule.name,
                "Failed to send scheduled message"
            );
        } else {
            debug!(schedule = %schedule.name, "Scheduled message posted successfully");
        }
    }

    info!(schedule = %schedule.name, "Schedule task exiting");
}

/// Dynamic scheduled message handler that monitors cache for changes.
/// Spawns and stops tasks dynamically based on cache updates.
#[instrument(skip(client, cache, channel, clock))]
pub async fn run_scheduled_message_handler<T, L>(
    client: Arc<TwitchIRCClient<T, L>>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
    shutdown: Arc<tokio::sync::Notify>,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    use std::collections::HashMap;
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, interval};

    info!("Dynamic scheduled message handler started");

    // Track running tasks by schedule name
    let mut running_tasks: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut current_version = 0u64;

    // Monitor cache for changes every 30 seconds
    let mut check_interval = interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            _ = check_interval.tick() => {}
            () = shutdown.notified() => {
                info!("Scheduled message handler: shutdown received, awaiting children");
                for (name, handle) in running_tasks.drain() {
                    handle.abort();
                    if let Err(e) = handle.await
                        && !e.is_cancelled()
                    {
                        warn!(schedule = %name, error = ?e, "Schedule task join error");
                    }
                }
                return;
            }
        }

        let (schedules, version) = {
            let cache_guard = cache.read().await;
            (cache_guard.schedules.clone(), cache_guard.version)
        };

        // Check if cache version has changed
        if version != current_version {
            info!(
                old_version = current_version,
                new_version = version,
                schedule_count = schedules.len(),
                "Cache version changed, updating tasks"
            );

            current_version = version;

            // Build set of schedule names that should be running
            let desired_schedules: HashMap<String, database::Schedule> =
                schedules.into_iter().map(|s| (s.name.clone(), s)).collect();

            // Stop tasks for schedules that no longer exist or have changed
            running_tasks.retain(|name, handle| {
                if !desired_schedules.contains_key(name) {
                    info!(schedule = %name, "Stopping task for removed/changed schedule");
                    handle.abort();
                    false
                } else {
                    true
                }
            });

            // Start tasks for new schedules
            for (name, schedule) in desired_schedules {
                let channel = channel.clone();
                let shutdown = shutdown.clone();
                let clock = clock.clone();
                running_tasks.entry(name.clone()).or_insert_with(|| {
                    info!(schedule = %name, "Starting task for new schedule");

                    tokio::spawn(run_schedule_task(
                        schedule,
                        client.clone(),
                        cache.clone(),
                        channel,
                        shutdown,
                        clock,
                    ))
                });
            }

            info!(active_tasks = running_tasks.len(), "Task update complete");
        }
    }
}
