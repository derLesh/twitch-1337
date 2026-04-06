use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::time::Duration;
use tracing::{info, warn};
use twitch_irc::message::PrivmsgMessage;

use crate::aviation::{AltBaro, NearbyAircraft};

const FLIGHTS_FILENAME: &str = "flights.ron";

/// Maximum number of simultaneously tracked flights.
pub(crate) const MAX_TRACKED_FLIGHTS: usize = 12;

/// Maximum number of flights a single user can track.
pub(crate) const MAX_FLIGHTS_PER_USER: usize = 3;

/// Vertical rate above which we consider the aircraft climbing.
const CLIMB_RATE_THRESHOLD: i64 = 500; // ft/min
/// Vertical rate below which we consider the aircraft descending.
const DESCENT_RATE_THRESHOLD: i64 = -500; // ft/min
/// Absolute vertical rate below which cruise is possible.
const CRUISE_RATE_THRESHOLD: i64 = 300; // ft/min
/// Minimum altitude for cruise detection.
const CRUISE_MIN_ALTITUDE: i64 = 10_000; // ft
/// Maximum altitude for approach detection.
const APPROACH_MAX_ALTITUDE: i64 = 10_000; // ft
/// Maximum altitude considered "on ground" (when alt_baro is numeric).
const GROUND_MAX_ALTITUDE: i64 = 200; // ft
/// Maximum ground speed considered "on ground".
const GROUND_MAX_SPEED: f64 = 30.0; // knots
/// Minimum ground speed for takeoff detection.
const TAKEOFF_MIN_SPEED: f64 = 60.0; // knots
/// Number of stable polls required before declaring cruise.
const CRUISE_STABLE_POLLS: u32 = 2;
/// No data threshold before declaring tracking lost.
pub(crate) const TRACKING_LOST_THRESHOLD: Duration = Duration::from_secs(300); // 5 min
/// Time after tracking lost before auto-removing.
pub(crate) const TRACKING_LOST_REMOVAL: Duration = Duration::from_secs(1800); // 30 min

// Polling intervals
pub(crate) const POLL_FAST: Duration = Duration::from_secs(30);
pub(crate) const POLL_NORMAL: Duration = Duration::from_secs(60);
pub(crate) const POLL_SLOW: Duration = Duration::from_secs(120);
/// Timeout for a single adsb.lol request.
pub(crate) const POLL_TIMEOUT: Duration = Duration::from_secs(10);

// Divert detection
const DIVERT_BEARING_THRESHOLD: f64 = 90.0; // degrees
const DIVERT_CONSECUTIVE_POLLS: u32 = 3;

// Emergency squawk codes
const SQUAWK_HIJACK: &str = "7500";
const SQUAWK_RADIO_FAILURE: &str = "7600";
const SQUAWK_EMERGENCY: &str = "7700";

/// Identifies a flight either by callsign or ICAO24 hex code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) enum FlightIdentifier {
    Callsign(String),
    Hex(String),
}

impl FlightIdentifier {
    /// Parse user input into a FlightIdentifier.
    ///
    /// 6-character all-hex-digit strings are treated as ICAO24 hex codes.
    /// Everything else is treated as a callsign.
    pub(crate) fn parse(input: &str) -> Self {
        let input = input.trim().to_uppercase();
        if input.len() == 6 && input.chars().all(|c| c.is_ascii_hexdigit()) {
            FlightIdentifier::Hex(input)
        } else {
            FlightIdentifier::Callsign(input)
        }
    }

    /// Returns the display string (the callsign or hex value).
    pub(crate) fn as_str(&self) -> &str {
        match self {
            FlightIdentifier::Callsign(s) | FlightIdentifier::Hex(s) => s,
        }
    }

    /// Check if this identifier matches a given callsign or hex.
    pub(crate) fn matches(&self, callsign: Option<&str>, hex: Option<&str>) -> bool {
        match self {
            FlightIdentifier::Callsign(s) => callsign.is_some_and(|cs| cs.eq_ignore_ascii_case(s)),
            FlightIdentifier::Hex(s) => hex.is_some_and(|h| h.eq_ignore_ascii_case(s)),
        }
    }
}

impl std::fmt::Display for FlightIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Detected flight phase.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) enum FlightPhase {
    Unknown,
    Ground,
    Takeoff,
    Climb,
    Cruise,
    Descent,
    Approach,
    Landing,
}

impl std::fmt::Display for FlightPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlightPhase::Unknown => write!(f, "Unknown"),
            FlightPhase::Ground => write!(f, "Ground"),
            FlightPhase::Takeoff => write!(f, "Takeoff"),
            FlightPhase::Climb => write!(f, "Climb"),
            FlightPhase::Cruise => write!(f, "Cruise"),
            FlightPhase::Descent => write!(f, "Descent"),
            FlightPhase::Approach => write!(f, "Approach"),
            FlightPhase::Landing => write!(f, "Landing"),
        }
    }
}

/// State of a single tracked flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TrackedFlight {
    pub(crate) identifier: FlightIdentifier,
    pub(crate) callsign: Option<String>,
    pub(crate) hex: Option<String>,
    pub(crate) phase: FlightPhase,
    pub(crate) route: Option<(String, String)>,   // (origin IATA, dest IATA)
    pub(crate) aircraft_type: Option<String>,

    // Latest known data
    pub(crate) altitude_ft: Option<i64>,
    pub(crate) vertical_rate_fpm: Option<i64>,
    pub(crate) ground_speed_kts: Option<f64>,
    pub(crate) lat: Option<f64>,
    pub(crate) lon: Option<f64>,
    pub(crate) squawk: Option<String>,

    // Tracking metadata
    pub(crate) tracked_by: String,
    pub(crate) tracked_at: DateTime<Utc>,
    pub(crate) last_seen: Option<DateTime<Utc>>,
    pub(crate) last_phase_change: Option<DateTime<Utc>>,
    pub(crate) polls_since_change: u32,

    // Divert detection
    #[serde(default)]
    pub(crate) divert_consecutive_polls: u32,

    // Destination coordinates (for divert detection)
    #[serde(default)]
    pub(crate) dest_lat: Option<f64>,
    #[serde(default)]
    pub(crate) dest_lon: Option<f64>,
}

/// Persisted state of all tracked flights.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct FlightTrackerState {
    pub(crate) flights: Vec<TrackedFlight>,
}

/// Loads tracked flights from the RON file.
///
/// Returns an empty state if the file doesn't exist or is corrupted.
pub(crate) async fn load_tracker_state(data_dir: &PathBuf) -> FlightTrackerState {
    let path = data_dir.join(FLIGHTS_FILENAME);
    match fs::read_to_string(&path).await {
        Ok(contents) => match ron::from_str::<FlightTrackerState>(&contents) {
            Ok(state) => {
                info!(
                    flights = state.flights.len(),
                    "Loaded flight tracker state from {}",
                    path.display()
                );
                state
            }
            Err(e) => {
                warn!(error = ?e, "Failed to parse flight tracker state, starting fresh");
                FlightTrackerState::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("No flight tracker state file found, starting fresh");
            FlightTrackerState::default()
        }
        Err(e) => {
            warn!(error = ?e, "Failed to read flight tracker state, starting fresh");
            FlightTrackerState::default()
        }
    }
}

/// Saves tracked flights to the RON file.
pub(crate) async fn save_tracker_state(data_dir: &PathBuf, state: &FlightTrackerState) {
    let path = data_dir.join(FLIGHTS_FILENAME);
    match ron::to_string(state) {
        Ok(serialized) => {
            if let Err(e) = fs::write(&path, serialized.as_bytes()).await {
                tracing::error!(error = ?e, "Failed to write flight tracker state");
            } else {
                tracing::debug!(
                    flights = state.flights.len(),
                    "Saved flight tracker state to {}",
                    path.display()
                );
            }
        }
        Err(e) => {
            tracing::error!(error = ?e, "Failed to serialize flight tracker state");
        }
    }
}

/// Commands sent from chat command handlers to the flight tracker task.
pub(crate) enum TrackerCommand {
    Track {
        identifier: FlightIdentifier,
        requested_by: String,
        reply_to: PrivmsgMessage,
    },
    Untrack {
        identifier: String,
        requested_by: String,
        is_mod: bool,
        reply_to: PrivmsgMessage,
    },
    Status {
        identifier: Option<String>,
        reply_to: PrivmsgMessage,
    },
}

// --- Phase detection helpers ---

fn is_on_ground(ac: &NearbyAircraft) -> bool {
    match &ac.alt_baro {
        Some(AltBaro::Ground) => true,
        Some(AltBaro::Feet(ft)) => {
            *ft < GROUND_MAX_ALTITUDE && ac.gs.unwrap_or(0.0) < GROUND_MAX_SPEED
        }
        None => false,
    }
}

pub(crate) fn altitude_ft(ac: &NearbyAircraft) -> Option<i64> {
    match &ac.alt_baro {
        Some(AltBaro::Feet(ft)) => Some(*ft),
        Some(AltBaro::Ground) => Some(0),
        None => None,
    }
}

pub(crate) fn vertical_rate(ac: &NearbyAircraft) -> Option<i64> {
    ac.baro_rate.or(ac.geom_rate)
}

/// Determines the new flight phase based on current ADS-B data and previous state.
pub(crate) fn detect_phase(flight: &TrackedFlight, ac: &NearbyAircraft) -> FlightPhase {
    let on_ground = is_on_ground(ac);
    let alt = altitude_ft(ac);
    let vrate = vertical_rate(ac);
    let gs = ac.gs.unwrap_or(0.0);
    let has_approach_mode = ac
        .nav_modes
        .as_ref()
        .is_some_and(|modes| modes.iter().any(|m| m == "approach"));

    // Landing: was airborne, now on ground
    if on_ground && !matches!(flight.phase, FlightPhase::Ground | FlightPhase::Unknown) {
        return FlightPhase::Landing;
    }

    // Ground: on ground (or was Landing which transitions here)
    if on_ground {
        return FlightPhase::Ground;
    }

    // Takeoff: was on ground, now speed > threshold and climbing
    if matches!(flight.phase, FlightPhase::Ground | FlightPhase::Unknown)
        && gs > TAKEOFF_MIN_SPEED
        && vrate.unwrap_or(0) > 0
    {
        return FlightPhase::Takeoff;
    }

    // Approach: descending below threshold altitude or approach mode active
    if let Some(alt_val) = alt {
        if (alt_val < APPROACH_MAX_ALTITUDE || has_approach_mode) && vrate.unwrap_or(0) < 0 {
            return FlightPhase::Approach;
        }
    }

    // Descent: negative vertical rate above approach altitude
    if let Some(vr) = vrate {
        if vr < DESCENT_RATE_THRESHOLD {
            return FlightPhase::Descent;
        }
    }

    // Cruise: stable altitude above minimum, low vertical rate for enough polls
    if let (Some(alt_val), Some(vr)) = (alt, vrate) {
        if alt_val > CRUISE_MIN_ALTITUDE
            && vr.abs() < CRUISE_RATE_THRESHOLD
            && flight.polls_since_change >= CRUISE_STABLE_POLLS
        {
            return FlightPhase::Cruise;
        }
    }

    // Climb: positive vertical rate
    if let Some(vr) = vrate {
        if vr > CLIMB_RATE_THRESHOLD {
            return FlightPhase::Climb;
        }
    }

    // If we were in a phase and conditions don't clearly match something else, stay
    flight.phase
}

/// Returns a human-readable meaning if the squawk is an emergency code.
pub(crate) fn emergency_squawk_meaning(squawk: &str) -> Option<&'static str> {
    match squawk {
        SQUAWK_HIJACK => Some("Hijack"),
        SQUAWK_RADIO_FAILURE => Some("Radio Failure"),
        SQUAWK_EMERGENCY => Some("Emergency"),
        _ => None,
    }
}

/// Determines the polling interval based on all tracked flights.
pub(crate) fn compute_poll_interval(flights: &[TrackedFlight]) -> Duration {
    if flights.is_empty() {
        return POLL_SLOW;
    }

    let needs_fast = flights.iter().any(|f| {
        f.polls_since_change < 5
            || matches!(
                f.phase,
                FlightPhase::Takeoff | FlightPhase::Approach | FlightPhase::Landing
            )
    });

    if needs_fast {
        return POLL_FAST;
    }

    let needs_normal = flights
        .iter()
        .any(|f| matches!(f.phase, FlightPhase::Climb | FlightPhase::Descent));

    if needs_normal {
        return POLL_NORMAL;
    }

    POLL_SLOW
}
