//! Daily 13:36–13:38 Berlin window monitor. Counts unique users who say
//! "1337" or "DANKIES" during the 13:37 minute, posts contextual stats at 13:38,
//! and persists the fastest sub-second times to the all-time leaderboard.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use chrono::{TimeDelta, Timelike, Utc};
use rand::seq::IndexedRandom as _;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{Mutex, broadcast},
};
use tracing::{debug, error, info, instrument, warn};
use twitch_irc::{
    TwitchIRCClient,
    login::LoginCredentials,
    message::{PrivmsgMessage, ServerMessage},
    transport::Transport,
};

use crate::{clock::Clock, resolve_berlin_time};

pub const TARGET_HOUR: u32 = 13;
pub const TARGET_MINUTE: u32 = 37;

/// Maximum number of unique users to track (prevents unbounded memory growth)
pub(crate) const MAX_USERS: usize = 10_000;

pub(crate) const LEADERBOARD_FILENAME: &str = "leaderboard.ron";

/// A user's personal best time for the 1337 challenge.
///
/// Tracks the fastest sub-1-second message time and the date it was achieved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalBest {
    /// Milliseconds after 13:37:00.000 (0-999)
    pub ms: u64,
    /// The date (Europe/Berlin) when this record was set
    pub date: chrono::NaiveDate,
}

/// Calculates the next occurrence of a daily time in Europe/Berlin timezone.
///
/// If the specified time has already passed today, returns tomorrow's occurrence.
pub(crate) fn calculate_next_occurrence(
    clock: &dyn Clock,
    hour: u32,
    minute: u32,
) -> chrono::DateTime<Utc> {
    let berlin_now = clock.now_utc().with_timezone(&chrono_tz::Europe::Berlin);

    let mut target = resolve_berlin_time(
        berlin_now
            .date_naive()
            .and_hms_opt(hour, minute, 0)
            .expect("Invalid hour/minute for Berlin time"),
    );

    if target <= berlin_now {
        target = resolve_berlin_time(
            (berlin_now + chrono::Duration::days(1))
                .date_naive()
                .and_hms_opt(hour, minute, 0)
                .expect("Invalid hour/minute for Berlin time"),
        );
    }

    target.with_timezone(&Utc)
}

/// Sleeps until the next occurrence of a daily time in Europe/Berlin timezone.
#[instrument(skip(clock))]
pub(crate) async fn wait_until_schedule(clock: &dyn Clock, hour: u32, minute: u32) {
    let next_run = calculate_next_occurrence(clock, hour, minute);
    let now = clock.now_utc();

    if next_run > now {
        info!(
            next_run_utc = ?next_run,
            next_run_berlin = ?next_run.with_timezone(&chrono_tz::Europe::Berlin),
            wait_seconds = (next_run - now).num_seconds(),
            "Sleeping until next scheduled time"
        );

        clock.sleep_until(next_run).await;
    }
}

/// Sleeps until a specific time today in Europe/Berlin timezone.
///
/// If the target time has already passed, returns immediately.
#[instrument(skip(clock))]
pub(crate) async fn sleep_until_hms(
    clock: &dyn Clock,
    hour: u32,
    minute: u32,
    second: u32,
    expected_latency: u32,
) {
    let now = clock.now_utc().with_timezone(&chrono_tz::Europe::Berlin);
    let target = resolve_berlin_time(
        now.date_naive()
            .and_hms_opt(hour, minute, second)
            .expect("Invalid stats time"),
    )
    .with_timezone(&Utc)
        - TimeDelta::milliseconds(i64::from(expected_latency));

    let now_utc = clock.now_utc();
    if target > now_utc {
        info!(
            wait_seconds = (target - now_utc).num_seconds(),
            "Waiting until 13:38 to post stats"
        );
        clock.sleep_until(target).await;
    }
}

/// Checks if a given user is a clanker
///
/// Returns true if the login name matches any bot in the ignore list.
pub(crate) fn is_clanker(login: &str) -> bool {
    [
        "supibot",
        "potatbotat",
        "streamelements",
        "koknuts",
        "thedagothur",
    ]
    .contains(&login)
}

/// Determines if a message should be counted as a valid 1337 message.
///
/// Filters out clanker messages and checks for keywords "1337" or "DANKIES".
pub(crate) fn is_valid_1337_message(message: &PrivmsgMessage) -> bool {
    if is_clanker(&message.sender.login) {
        return false;
    }
    message.message_text.contains("DANKIES") || message.message_text.contains("1337")
}

/// Probability of a meme override firing when the count qualifies.
const MEME_OVERRIDE_CHANCE: f32 = 0.10;

/// Generates a stats message based on the number of users who said 1337.
///
/// Returns a contextual message with emotes based on participation level.
pub(crate) fn generate_stats_message(count: usize, user_list: &[String]) -> String {
    if let Some(meme) = meme_override(count, &mut rand::rng()) {
        return meme;
    }
    match count {
        0 => one_of(&["Erm", "fuh"]).to_string(),
        1 => one_of(&[
            format!(
                "@{} zumindest einer {}",
                user_list
                    .first()
                    .expect("Count should equal user list length"),
                one_of(&["fuh", "uhh"])
            ),
            format!(
                "War wohl zu viel verlangt {}",
                one_of(&["BRUHSIT", "UltraMad", "Madeg"])
            ),
        ])
        .clone(),
        2..=3 if !user_list.iter().any(|u| u == "gargoyletec") => {
            format!(
                "{count}{} gnocci {}",
                one_of(&[" und nichtmal", ", aber wo", " und ohne"]),
                one_of(&["Sadding", "Sadge", "Sadeg", "SadgeCry", "Saddies"])
            )
        }
        2..=3 => format!(
            "{count}, {}",
            one_of(&[
                "geht besser Okayge",
                "verbesserungswürdig Waiting",
                "unterdurchschnittlich Waiting",
                "ausbaufähig Waiting",
                "entspricht nicht ganz den Erwartungen Waiting",
                "bemüht",
                "anpassungsfähig YEP"
            ])
        ),
        4 => one_of(&[
            "3.6, nicht gut, nicht dramatisch".to_string(),
            format!("{count}, {}", one_of(&["Standard Performance", "solide"])),
        ])
        .clone(),
        5..=7 => {
            format!(
                "{count}, gute Auslese {}",
                one_of(&["bieberbutzemanPepe", "peepoHappy"])
            )
        }
        _ => {
            format!(
                "{count}, {}",
                one_of(&["insane quota Pag", "rekordverdächtig WICKED"])
            )
        }
    }
}

/// Returns a random element from an array.
///
/// Used for adding variety to bot responses by randomly selecting from predefined options.
pub(crate) fn one_of<const L: usize, T>(array: &[T; L]) -> &T {
    array.choose(&mut rand::rng()).unwrap()
}

/// Small-chance meme overrides keyed to specific counts. Each qualifying rule
/// rolls independently; first success wins. Counts with no rule fall through
/// to the regular `match` in `generate_stats_message`.
fn meme_override(count: usize, rng: &mut impl rand::RngExt) -> Option<String> {
    if count == 7 && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(
            one_of(&[
                "777 FORSEN 777 FORSEN 777 FORSEN 777",
                "777 FORSEN 777 FORSEN 777",
                "777 FORSEN 777",
            ])
            .to_string(),
        );
    }
    if matches!(count, 6 | 7) && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(one_of(&["6-7", "6-7 ICANT", "6-7 OOOO"]).to_string());
    }
    if count == 3 && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(one_of(&["3Head", "3Head Clueless", "3Head erm"]).to_string());
    }
    if count == 4 && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(one_of(&["4Head Keepo", "4Head", "4Head FeelsGoodMan"]).to_string());
    }
    if count == 5 && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(
            one_of(&[
                "5Head 🍷 Ah yes, I can feel my head throbbing with knowledge and wisdom as I sip upon this Sauvignon blanc",
                "5Head Galaxybrain",
                "5Head",
            ])
            .to_string(),
        );
    }
    if count == 10 && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(one_of(&["10/10 IGN", "Perfect 10 Clap", "10, masterpiece Clap"]).to_string());
    }
    if count == 13 && rng.random::<f32>() < MEME_OVERRIDE_CHANCE {
        return Some(one_of(&["unlucky 13 monkaW", "13 monkaW", "13, spooky monkaS"]).to_string());
    }
    None
}

/// Loads the all-time leaderboard from disk.
///
/// Returns an empty HashMap if the file doesn't exist or is corrupted.
pub async fn load_leaderboard(data_dir: &std::path::Path) -> HashMap<String, PersonalBest> {
    let path = data_dir.join(LEADERBOARD_FILENAME);
    match fs::read_to_string(&path).await {
        Ok(contents) => match ron::from_str::<HashMap<String, PersonalBest>>(&contents) {
            Ok(leaderboard) => {
                info!(
                    entries = leaderboard.len(),
                    "Loaded leaderboard from {}",
                    path.display()
                );
                leaderboard
            }
            Err(e) => {
                warn!(error = ?e, "Failed to parse leaderboard file, starting fresh");
                HashMap::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("No leaderboard file found, starting fresh");
            HashMap::new()
        }
        Err(e) => {
            warn!(error = ?e, "Failed to read leaderboard file, starting fresh");
            HashMap::new()
        }
    }
}

/// Saves the all-time leaderboard to disk using atomic write+rename.
///
/// Logs an error and continues if the write fails.
pub(crate) async fn save_leaderboard(
    leaderboard: &HashMap<String, PersonalBest>,
    data_dir: &std::path::Path,
) {
    let path = data_dir.join(LEADERBOARD_FILENAME);
    let tmp_path = path.with_extension("ron.tmp");
    match ron::to_string(leaderboard) {
        Ok(serialized) => {
            if let Err(e) = fs::write(&tmp_path, serialized.as_bytes()).await {
                error!(error = ?e, "Failed to write leaderboard tmp file");
            } else if let Err(e) = fs::rename(&tmp_path, &path).await {
                error!(error = ?e, "Failed to rename leaderboard file");
            } else {
                info!(
                    entries = leaderboard.len(),
                    "Saved leaderboard to {}",
                    path.display()
                );
            }
        }
        Err(e) => {
            error!(error = ?e, "Failed to serialize leaderboard");
        }
    }
}

/// Monitors broadcast messages and tracks users who say 1337 during the target minute.
///
/// Runs in a loop until the broadcast channel closes or an error occurs.
/// Only tracks messages sent during the configured TARGET_HOUR:TARGET_MINUTE.
#[instrument(skip(broadcast_rx, total_users))]
pub(crate) async fn monitor_1337_messages(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    total_users: Arc<Mutex<HashMap<String, Option<u64>>>>,
) {
    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                let local = privmsg
                    .server_timestamp
                    .with_timezone(&chrono_tz::Europe::Berlin);
                if (local.hour(), local.minute()) != (TARGET_HOUR, TARGET_MINUTE) {
                    continue;
                }

                if is_valid_1337_message(&privmsg) {
                    let mut users = total_users.lock().await;
                    if users.len() >= MAX_USERS {
                        error!(max = MAX_USERS, "User limit reached");
                    } else if !users.contains_key(privmsg.sender.login.as_str()) {
                        let username = privmsg.sender.login.clone();
                        let ms_since_minute = u64::from(local.second()) * 1000
                            + u64::from(local.timestamp_subsec_millis());
                        if ms_since_minute < 1000 {
                            debug!(user = %username, ms = ms_since_minute, "User said 1337 at 13:37 (sub-second)");
                            users.insert(username, Some(ms_since_minute));
                        } else {
                            debug!(user = %username, "User said 1337 at 13:37");
                            users.insert(username, None);
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "1337 handler lagged, skipped messages");
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, 1337 monitor exiting");
                break;
            }
        }
    }
}

/// Handler for the daily 1337 tracking feature.
///
/// Monitors messages during the 13:37 window, tracks unique users, and posts stats at 13:38.
/// Runs continuously, resetting state daily.
#[instrument(skip(broadcast_tx, client, channel, latency, clock))]
pub async fn run_1337_handler<T, L>(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<TwitchIRCClient<T, L>>,
    channel: String,
    latency: Arc<AtomicU32>,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    clock: Arc<dyn Clock>,
    data_dir: std::path::PathBuf,
) where
    T: Transport,
    L: LoginCredentials,
{
    info!("1337 handler started");

    loop {
        // Wait until 13:36 to start monitoring
        wait_until_schedule(clock.as_ref(), TARGET_HOUR, TARGET_MINUTE - 1).await;

        info!("Starting daily 1337 monitoring session");

        // Fresh HashMap for today's users (maps username -> ms_since_minute if sub-second)
        let total_users = Arc::new(Mutex::new(HashMap::with_capacity(MAX_USERS)));

        // Spawn message monitoring subtask
        let mut monitor_handle = tokio::spawn({
            let total_users = total_users.clone();
            // Subscribe fresh when we wake up - only see messages from now on
            let broadcast_rx = broadcast_tx.subscribe();

            async move {
                monitor_1337_messages(broadcast_rx, total_users).await;
            }
        });

        // Wait until 13:36:30 to send reminder
        sleep_until_hms(
            clock.as_ref(),
            TARGET_HOUR,
            TARGET_MINUTE - 1,
            30,
            latency.load(Ordering::Relaxed),
        )
        .await;

        info!("Posting reminder to channel");
        if let Err(e) = client.say(channel.clone(), "PausersHype".to_string()).await {
            error!(error = ?e, "Failed to send reminder message");
        }

        // Wait until 13:38 to post stats
        sleep_until_hms(
            clock.as_ref(),
            TARGET_HOUR,
            TARGET_MINUTE + 1,
            0,
            latency.load(Ordering::Relaxed),
        )
        .await;

        // Abort the monitor and wait for it to fully stop before reading results
        monitor_handle.abort();
        let _ = (&mut monitor_handle).await;

        // Get user list, count, and fastest
        let (count, user_list, fastest) = {
            let users = total_users.lock().await;
            let count = users.len();
            let mut user_vec: Vec<String> = users.keys().cloned().collect();
            user_vec.sort();

            let fastest: Option<(String, u64)> = users
                .iter()
                .filter_map(|(name, ms)| ms.map(|ms| (name.clone(), ms)))
                .min_by_key(|(_, ms)| *ms);

            (count, user_vec, fastest)
        };

        let mut message = generate_stats_message(count, &user_list);

        if let Some((ref fastest_user, fastest_ms)) = fastest {
            let leaderboard_guard = leaderboard.read().await;
            let previous_pb = leaderboard_guard.get(fastest_user).map(|pb| pb.ms);
            let is_record = previous_pb.is_none_or(|best| fastest_ms < best);
            drop(leaderboard_guard);

            message.push_str(&format!(
                " | {fastest_user} war mass schnellste mit {fastest_ms}ms"
            ));
            if is_record {
                message.push_str(" - neuer Rekord!");
            }
        }

        // Update leaderboard with today's sub-1s times
        let today = clock
            .now_utc()
            .with_timezone(&chrono_tz::Europe::Berlin)
            .date_naive();
        {
            let users = total_users.lock().await;
            let mut leaderboard_guard = leaderboard.write().await;
            for (username, timing) in users.iter() {
                if let Some(ms) = timing {
                    let update = match leaderboard_guard.get(username) {
                        Some(existing) => *ms < existing.ms,
                        None => true,
                    };
                    if update {
                        leaderboard_guard.insert(
                            username.clone(),
                            PersonalBest {
                                ms: *ms,
                                date: today,
                            },
                        );
                    }
                }
            }
            save_leaderboard(&leaderboard_guard, &data_dir).await;
        }

        // Post stats message
        info!(count = count, "Posting stats to channel");
        if let Err(e) = client.say(channel.clone(), message).await {
            error!(error = ?e, count = count, "Failed to send stats message");
        } else {
            info!("Stats posted successfully");
        }

        info!("Daily 1337 session completed, waiting for next day");
    }
}
