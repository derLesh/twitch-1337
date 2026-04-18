use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};
use twitch_irc::message::PrivmsgMessage;

use crate::AuthenticatedTwitchClient;
use crate::aviation::{AltBaro, AviationClient, NearbyAircraft, iata_to_coords};

const FLIGHTS_FILENAME: &str = "flights.ron";

/// Maximum number of simultaneously tracked flights.
pub const MAX_TRACKED_FLIGHTS: usize = 12;

/// Maximum number of flights a single user can track.
pub const MAX_FLIGHTS_PER_USER: usize = 3;

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
pub const TRACKING_LOST_THRESHOLD: Duration = Duration::from_secs(300); // 5 min
/// Time after tracking lost before auto-removing.
pub const TRACKING_LOST_REMOVAL: Duration = Duration::from_secs(1800); // 30 min

// Polling intervals
pub const POLL_FAST: Duration = Duration::from_secs(30);
pub const POLL_NORMAL: Duration = Duration::from_secs(60);
pub const POLL_SLOW: Duration = Duration::from_secs(120);
/// Timeout for a single adsb.lol request.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for adsbdb route fetch.
const ROUTE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

// Divert detection
const DIVERT_BEARING_THRESHOLD: f64 = 90.0; // degrees
const DIVERT_CONSECUTIVE_POLLS: u32 = 3;

// Emergency squawk codes
const SQUAWK_HIJACK: &str = "7500";
const SQUAWK_RADIO_FAILURE: &str = "7600";
const SQUAWK_EMERGENCY: &str = "7700";

/// Identifies a flight either by callsign or ICAO24 hex code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FlightIdentifier {
    Callsign(String),
    Hex(String),
}

impl FlightIdentifier {
    /// Parse user input into a FlightIdentifier.
    ///
    /// 6-character all-hex-digit strings are treated as ICAO24 hex codes.
    /// Everything else is treated as a callsign.
    pub fn parse(input: &str) -> Self {
        let input = input.trim().to_uppercase();
        if input.len() == 6 && input.chars().all(|c| c.is_ascii_hexdigit()) {
            FlightIdentifier::Hex(input)
        } else {
            FlightIdentifier::Callsign(input)
        }
    }

    /// Returns the display string (the callsign or hex value).
    pub fn as_str(&self) -> &str {
        match self {
            FlightIdentifier::Callsign(s) | FlightIdentifier::Hex(s) => s,
        }
    }

    /// Check if this identifier matches a given callsign or hex.
    pub fn matches(&self, callsign: Option<&str>, hex: Option<&str>) -> bool {
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
pub enum FlightPhase {
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
pub struct TrackedFlight {
    pub identifier: FlightIdentifier,
    pub callsign: Option<String>,
    pub hex: Option<String>,
    pub phase: FlightPhase,
    pub route: Option<(String, String)>, // (origin IATA, dest IATA)
    pub aircraft_type: Option<String>,

    // Latest known data
    pub altitude_ft: Option<i64>,
    pub vertical_rate_fpm: Option<i64>,
    pub ground_speed_kts: Option<f64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub squawk: Option<String>,

    // Tracking metadata
    pub tracked_by: String,
    pub tracked_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
    pub last_phase_change: Option<DateTime<Utc>>,
    pub polls_since_change: u32,

    // Divert detection
    #[serde(default)]
    pub divert_consecutive_polls: u32,

    // Destination coordinates (for divert detection)
    #[serde(default)]
    pub dest_lat: Option<f64>,
    #[serde(default)]
    pub dest_lon: Option<f64>,
}

/// Persisted state of all tracked flights.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlightTrackerState {
    pub flights: Vec<TrackedFlight>,
}

/// Loads tracked flights from the RON file.
///
/// Returns an empty state if the file doesn't exist or is corrupted.
pub(crate) async fn load_tracker_state(data_dir: &Path) -> FlightTrackerState {
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

/// Saves tracked flights to the RON file using atomic write+rename.
pub(crate) async fn save_tracker_state(data_dir: &Path, state: &FlightTrackerState) {
    let path = data_dir.join(FLIGHTS_FILENAME);
    let tmp_path = path.with_extension("ron.tmp");
    match ron::to_string(state) {
        Ok(serialized) => {
            if let Err(e) = fs::write(&tmp_path, serialized.as_bytes()).await {
                tracing::error!(error = ?e, "Failed to write flight tracker state tmp");
            } else if let Err(e) = fs::rename(&tmp_path, &path).await {
                tracing::error!(error = ?e, "Failed to rename flight tracker state");
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
pub enum TrackerCommand {
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

    // Descent: significant negative vertical rate
    if let Some(vr) = vrate
        && vr < DESCENT_RATE_THRESHOLD
    {
        // Approach: already descending and below threshold altitude, or approach mode active
        if let Some(alt_val) = alt
            && (alt_val < APPROACH_MAX_ALTITUDE
                || has_approach_mode
                || matches!(flight.phase, FlightPhase::Approach))
        {
            return FlightPhase::Approach;
        }
        return FlightPhase::Descent;
    }

    // Approach from nav_modes even without strong descent rate
    if has_approach_mode && vrate.unwrap_or(0) < 0 {
        return FlightPhase::Approach;
    }

    // Cruise: stable altitude above minimum, low vertical rate for enough polls
    if let (Some(alt_val), Some(vr)) = (alt, vrate)
        && alt_val > CRUISE_MIN_ALTITUDE
        && vr.abs() < CRUISE_RATE_THRESHOLD
        && flight.polls_since_change >= CRUISE_STABLE_POLLS
    {
        return FlightPhase::Cruise;
    }

    // Climb: positive vertical rate
    if let Some(vr) = vrate
        && vr > CLIMB_RATE_THRESHOLD
    {
        return FlightPhase::Climb;
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

// --- Helpers ---

/// Finds a tracked flight by searching identifier, callsign, and hex (case-insensitive).
fn find_flight_index(flights: &[TrackedFlight], query: &str) -> Option<usize> {
    let upper = query.to_uppercase();
    flights.iter().position(|f| {
        f.identifier.as_str().eq_ignore_ascii_case(&upper)
            || f.callsign
                .as_ref()
                .is_some_and(|cs| cs.eq_ignore_ascii_case(&upper))
            || f.hex
                .as_ref()
                .is_some_and(|h| h.eq_ignore_ascii_case(&upper))
    })
}

/// Formats a duration as "Xh Ym" or "Ym".
fn format_duration_hm(d: chrono::TimeDelta) -> String {
    let hours = d.num_hours();
    let mins = d.num_minutes() % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else {
        format!("{mins}m")
    }
}

// --- Chat message formatting ---

/// Formats altitude as FL (flight level) or feet.
fn format_alt(alt_ft: Option<i64>) -> String {
    match alt_ft {
        Some(ft) if ft >= 1000 => format!("FL{}", ft / 100),
        Some(ft) => format!("{ft}ft"),
        None => "?".to_string(),
    }
}

/// Formats route as "ORIG->DEST" or empty string if unknown.
fn format_route(route: &Option<(String, String)>) -> String {
    match route {
        Some((orig, dest)) => format!(" {orig}\u{2192}{dest}"),
        None => String::new(),
    }
}

/// Formats the flight prefix: "DLH123 (A320) FRA->MUC" with graceful degradation.
fn format_flight_prefix(flight: &TrackedFlight) -> String {
    let name = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str());
    let typ = flight
        .aircraft_type
        .as_ref()
        .map(|t| format!(" ({t})"))
        .unwrap_or_default();
    let route = format_route(&flight.route);
    format!("{name}{typ}{route}")
}

pub(crate) fn msg_track_started(flight: &TrackedFlight) -> String {
    format!("Tracke {} Okayge", format_flight_prefix(flight))
}

pub(crate) fn msg_takeoff(flight: &TrackedFlight) -> String {
    format!("{} ist gestartet! \u{2708}", format_flight_prefix(flight))
}

pub(crate) fn msg_cruise(flight: &TrackedFlight) -> String {
    format!(
        "{} cruist auf {}",
        format_flight_prefix(flight),
        format_alt(flight.altitude_ft)
    )
}

pub(crate) fn msg_descent(flight: &TrackedFlight) -> String {
    format!("{} hat Descent eingeleitet", format_flight_prefix(flight))
}

pub(crate) fn msg_approach(flight: &TrackedFlight) -> String {
    format!("{} ist im Approach", format_flight_prefix(flight))
}

pub(crate) fn msg_landing(flight: &TrackedFlight) -> String {
    let duration = Utc::now().signed_duration_since(flight.tracked_at);
    format!(
        "{} ist gelandet! Flugzeit: {}",
        format_flight_prefix(flight),
        format_duration_hm(duration)
    )
}

pub(crate) fn msg_squawk_emergency(flight: &TrackedFlight, code: &str, meaning: &str) -> String {
    format!(
        "\u{26a0} {} squawkt {code}! ({meaning})",
        format_flight_prefix(flight)
    )
}

pub(crate) fn msg_possible_divert(flight: &TrackedFlight) -> String {
    format!(
        "\u{26a0} {} scheint zu diverten!",
        format_flight_prefix(flight)
    )
}

pub(crate) fn msg_tracking_lost(flight: &TrackedFlight) -> String {
    let name = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str());
    format!("{name} Signal verloren, wird nicht mehr getrackt")
}

pub(crate) fn msg_flight_status(flight: &TrackedFlight) -> String {
    let prefix = format_flight_prefix(flight);
    let alt = format_alt(flight.altitude_ft);
    let speed = flight
        .ground_speed_kts
        .map(|gs| format!(" | {gs:.0}kts"))
        .unwrap_or_default();
    let squawk = flight
        .squawk
        .as_ref()
        .map(|s| format!(" | Squawk {s}"))
        .unwrap_or_default();
    let elapsed = Utc::now().signed_duration_since(flight.tracked_at);
    let tracking_time = format!("seit {} getrackt", format_duration_hm(elapsed));
    format!(
        "{prefix} | {} {alt}{speed}{squawk} | {tracking_time}",
        flight.phase
    )
}

pub(crate) fn msg_flights_list(flights: &[TrackedFlight]) -> String {
    if flights.is_empty() {
        return "Keine Fl\u{00fc}ge getrackt".to_string();
    }
    let parts: Vec<String> = flights
        .iter()
        .map(|f| {
            let name = f.callsign.as_deref().unwrap_or(f.identifier.as_str());
            let alt = format_alt(f.altitude_ft);
            format!("{name} ({} {alt})", f.phase)
        })
        .collect();
    format!("Getrackte Fl\u{00fc}ge: {}", parts.join(" | "))
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

// --- Handler ---

/// Main flight tracker handler. Processes commands, polls adsb.lol, detects
/// state changes, and posts chat messages.
pub async fn run_flight_tracker(
    mut cmd_rx: mpsc::Receiver<TrackerCommand>,
    client: Arc<AuthenticatedTwitchClient>,
    channel: String,
    aviation_client: AviationClient,
    data_dir: PathBuf,
) {
    let mut state = load_tracker_state(&data_dir).await;
    info!(flights = state.flights.len(), "Flight tracker started");

    loop {
        if state.flights.is_empty() {
            // No flights tracked: block until we get a command
            let Some(cmd) = cmd_rx.recv().await else {
                info!("Flight tracker command channel closed, shutting down");
                return;
            };
            process_command(cmd, &mut state, &client, &aviation_client, &data_dir).await;
        } else {
            // Drain all pending commands without blocking
            while let Ok(cmd) = cmd_rx.try_recv() {
                process_command(cmd, &mut state, &client, &aviation_client, &data_dir).await;
            }

            // Poll all flights
            poll_all_flights(&mut state, &client, &channel, &aviation_client, &data_dir).await;

            // Sleep with adaptive interval, but wake up early for commands
            let interval = compute_poll_interval(&state.flights);
            debug!(
                interval_secs = interval.as_secs(),
                flights = state.flights.len(),
                "Sleeping until next poll"
            );

            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            process_command(
                                cmd,
                                &mut state,
                                &client,
                                &aviation_client,
                                &data_dir,
                            )
                            .await;
                        }
                        None => {
                            info!("Flight tracker command channel closed, shutting down");
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn process_command(
    cmd: TrackerCommand,
    state: &mut FlightTrackerState,
    client: &Arc<AuthenticatedTwitchClient>,
    aviation_client: &AviationClient,
    data_dir: &Path,
) {
    match cmd {
        TrackerCommand::Track {
            identifier,
            requested_by,
            reply_to,
        } => {
            handle_track(
                identifier,
                &requested_by,
                &reply_to,
                state,
                client,
                aviation_client,
                data_dir,
            )
            .await;
        }
        TrackerCommand::Untrack {
            identifier,
            requested_by,
            is_mod,
            reply_to,
        } => {
            handle_untrack(
                &identifier,
                &requested_by,
                is_mod,
                &reply_to,
                state,
                client,
                data_dir,
            )
            .await;
        }
        TrackerCommand::Status {
            identifier,
            reply_to,
        } => {
            handle_status(identifier.as_deref(), &reply_to, state, client).await;
        }
    }
}

async fn handle_track(
    identifier: FlightIdentifier,
    requested_by: &str,
    reply_to: &PrivmsgMessage,
    state: &mut FlightTrackerState,
    client: &Arc<AuthenticatedTwitchClient>,
    aviation_client: &AviationClient,
    data_dir: &Path,
) {
    // Check global limit
    if state.flights.len() >= MAX_TRACKED_FLIGHTS {
        let msg = format!("Maximal {MAX_TRACKED_FLIGHTS} Flüge gleichzeitig FDM");
        if let Err(e) = client.say_in_reply_to(reply_to, msg).await {
            error!(error = ?e, "Failed to send limit message");
        }
        return;
    }

    // Check per-user limit
    let user_count = state
        .flights
        .iter()
        .filter(|f| f.tracked_by == requested_by)
        .count();
    if user_count >= MAX_FLIGHTS_PER_USER {
        let msg = format!("Du trackst schon {MAX_FLIGHTS_PER_USER} Flüge FDM");
        if let Err(e) = client.say_in_reply_to(reply_to, msg).await {
            error!(error = ?e, "Failed to send per-user limit message");
        }
        return;
    }

    // Check for duplicates
    let already_tracked = state.flights.iter().any(|f| {
        f.identifier == identifier || identifier.matches(f.callsign.as_deref(), f.hex.as_deref())
    });
    if already_tracked {
        let msg = format!("{} wird schon getrackt FDM", identifier);
        if let Err(e) = client.say_in_reply_to(reply_to, msg).await {
            error!(error = ?e, "Failed to send duplicate message");
        }
        return;
    }

    // Query adsb.lol to verify flight exists
    let ac_result = match &identifier {
        FlightIdentifier::Hex(hex) => {
            tokio::time::timeout(POLL_TIMEOUT, aviation_client.get_aircraft_by_hex(hex)).await
        }
        FlightIdentifier::Callsign(cs) => {
            // Translate IATA flight numbers (e.g. TP247 → TAP247) before querying.
            // This runs outside POLL_TIMEOUT: adsbdb fallback has its own 5s timeout.
            let resolved = aviation_client.resolve_callsign(cs).await;
            tokio::time::timeout(
                POLL_TIMEOUT,
                aviation_client.get_aircraft_by_callsign(&resolved),
            )
            .await
        }
    };

    let ac = match ac_result {
        Ok(Ok(Some(ac))) => ac,
        Ok(Ok(None)) => {
            let msg = format!("{} nicht gefunden auf adsb.lol FDM", identifier);
            if let Err(e) = client.say_in_reply_to(reply_to, msg).await {
                error!(error = ?e, "Failed to send not-found message");
            }
            return;
        }
        Ok(Err(e)) => {
            error!(error = ?e, identifier = %identifier, "adsb.lol lookup failed");
            if let Err(e) = client
                .say_in_reply_to(reply_to, "adsb.lol Anfrage fehlgeschlagen FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send error message");
            }
            return;
        }
        Err(_) => {
            if let Err(e) = client
                .say_in_reply_to(reply_to, "adsb.lol Anfrage Timeout FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send timeout message");
            }
            return;
        }
    };

    // Extract data from the aircraft
    let callsign = ac
        .flight
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let hex = ac.hex.clone();
    let aircraft_type = ac.t.clone();
    let now = Utc::now();

    let mut flight = TrackedFlight {
        identifier: identifier.clone(),
        callsign: callsign.clone(),
        hex: hex.clone(),
        phase: FlightPhase::Unknown,
        route: None,
        aircraft_type,
        altitude_ft: altitude_ft(&ac),
        vertical_rate_fpm: vertical_rate(&ac),
        ground_speed_kts: ac.gs,
        lat: ac.lat,
        lon: ac.lon,
        squawk: ac.squawk.clone(),
        tracked_by: requested_by.to_string(),
        tracked_at: now,
        last_seen: Some(now),
        last_phase_change: None,
        polls_since_change: 0,
        divert_consecutive_polls: 0,
        dest_lat: None,
        dest_lon: None,
    };

    // Detect initial phase
    flight.phase = detect_phase(&flight, &ac);

    // Fetch route if we have a callsign
    if let Some(cs) = &callsign {
        match tokio::time::timeout(ROUTE_FETCH_TIMEOUT, aviation_client.get_flight_route(cs)).await
        {
            Ok(Ok(Some(route))) => {
                let origin = route.origin.iata_code.clone();
                let dest = route.destination.iata_code.clone();

                // Resolve destination coordinates for divert detection
                if let Some((lat, lon, _)) = iata_to_coords(&dest) {
                    flight.dest_lat = Some(lat);
                    flight.dest_lon = Some(lon);
                }

                flight.route = Some((origin, dest));
            }
            Ok(Ok(None)) => {
                debug!(callsign = %cs, "No route found for flight");
            }
            Ok(Err(e)) => {
                warn!(error = ?e, callsign = %cs, "Failed to fetch route");
            }
            Err(_) => {
                warn!(callsign = %cs, "Route fetch timed out");
            }
        }
    }

    let response = msg_track_started(&flight);
    state.flights.push(flight);
    save_tracker_state(data_dir, state).await;

    info!(identifier = %identifier, requested_by = %requested_by, "Flight tracking started");
    if let Err(e) = client.say_in_reply_to(reply_to, response).await {
        error!(error = ?e, "Failed to send track started message");
    }
}

async fn handle_untrack(
    identifier: &str,
    requested_by: &str,
    is_mod: bool,
    reply_to: &PrivmsgMessage,
    state: &mut FlightTrackerState,
    client: &Arc<AuthenticatedTwitchClient>,
    data_dir: &Path,
) {
    let Some(idx) = find_flight_index(&state.flights, identifier) else {
        if let Err(e) = client
            .say_in_reply_to(reply_to, format!("{identifier} nicht gefunden FDM"))
            .await
        {
            error!(error = ?e, "Failed to send not-found message");
        }
        return;
    };

    // Check permissions: only the user who tracked it or mods can untrack
    let flight = &state.flights[idx];
    if flight.tracked_by != requested_by && !is_mod {
        if let Err(e) = client
            .say_in_reply_to(
                reply_to,
                "Nur der Tracker oder Mods können das untracking machen FDM".to_string(),
            )
            .await
        {
            error!(error = ?e, "Failed to send permission message");
        }
        return;
    }

    let name = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str())
        .to_string();
    state.flights.remove(idx);
    save_tracker_state(data_dir, state).await;

    info!(identifier = %identifier, requested_by = %requested_by, "Flight untracked");
    if let Err(e) = client
        .say_in_reply_to(reply_to, format!("{name} wird nicht mehr getrackt Okayge"))
        .await
    {
        error!(error = ?e, "Failed to send untrack message");
    }
}

async fn handle_status(
    identifier: Option<&str>,
    reply_to: &PrivmsgMessage,
    state: &FlightTrackerState,
    client: &Arc<AuthenticatedTwitchClient>,
) {
    let response = match identifier {
        None => msg_flights_list(&state.flights),
        Some(id) => match find_flight_index(&state.flights, id) {
            Some(idx) => msg_flight_status(&state.flights[idx]),
            None => format!("{id} nicht gefunden FDM"),
        },
    };

    if let Err(e) = client.say_in_reply_to(reply_to, response).await {
        error!(error = ?e, "Failed to send status message");
    }
}

async fn poll_all_flights(
    state: &mut FlightTrackerState,
    client: &Arc<AuthenticatedTwitchClient>,
    channel: &str,
    aviation_client: &AviationClient,
    data_dir: &Path,
) {
    let now = Utc::now();
    let mut changed = false;
    let mut removals: Vec<usize> = Vec::new();
    let mut messages: Vec<String> = Vec::new();

    // Phase 1: Fetch all aircraft data in parallel using JoinSet
    use crate::aviation::NearbyAircraft;
    type PollResult =
        Result<Result<Option<NearbyAircraft>, eyre::Report>, tokio::time::error::Elapsed>;

    let mut join_set = tokio::task::JoinSet::new();
    for (idx, flight) in state.flights.iter().enumerate() {
        let ac = aviation_client.clone();
        let id = flight.identifier.clone();
        let hex = flight.hex.clone();
        join_set.spawn(async move {
            let result: PollResult = match &id {
                FlightIdentifier::Hex(h) => {
                    tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_hex(h)).await
                }
                FlightIdentifier::Callsign(cs) => {
                    if let Some(h) = &hex {
                        tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_hex(h)).await
                    } else {
                        tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_callsign(cs)).await
                    }
                }
            };
            (idx, result)
        });
    }

    // Collect results indexed by flight position
    let mut fetch_results: Vec<Option<PollResult>> =
        (0..state.flights.len()).map(|_| None).collect();
    while let Some(res) = join_set.join_next().await {
        if let Ok((idx, poll_result)) = res {
            fetch_results[idx] = Some(poll_result);
        }
    }

    // Phase 2: Process results sequentially (mutates state)
    let removal_threshold =
        chrono::TimeDelta::from_std(TRACKING_LOST_REMOVAL).unwrap_or(chrono::TimeDelta::zero());
    let lost_threshold =
        chrono::TimeDelta::from_std(TRACKING_LOST_THRESHOLD).unwrap_or(chrono::TimeDelta::zero());

    #[allow(clippy::needless_range_loop)]
    for idx in 0..state.flights.len() {
        let Some(ac_result) = fetch_results[idx].take() else {
            continue;
        };
        let flight = &mut state.flights[idx];

        let ac = match ac_result {
            Ok(Ok(Some(ac))) => ac,
            Ok(Ok(None)) => {
                // Aircraft not found -- check tracking lost threshold
                if let Some(last_seen) = flight.last_seen {
                    let lost_duration = now.signed_duration_since(last_seen);
                    if lost_duration >= removal_threshold {
                        info!(
                            identifier = %flight.identifier,
                            "Removing flight: tracking lost for {}s",
                            lost_duration.num_seconds()
                        );
                        messages.push(msg_tracking_lost(flight));
                        removals.push(idx);
                    } else if lost_duration >= lost_threshold {
                        debug!(
                            identifier = %flight.identifier,
                            last_seen_secs_ago = lost_duration.num_seconds(),
                            "Flight not visible on adsb.lol"
                        );
                    }
                }
                continue;
            }
            Ok(Err(e)) => {
                warn!(error = ?e, identifier = %flight.identifier, "adsb.lol poll failed");
                continue;
            }
            Err(_) => {
                warn!(identifier = %flight.identifier, "adsb.lol poll timed out");
                continue;
            }
        };

        // Aircraft found -- update last_seen (don't mark changed for just this)
        flight.last_seen = Some(now);

        // Resolve callsign/hex/type if not yet known
        if flight.callsign.is_none()
            && let Some(cs) = ac
                .flight
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        {
            debug!(identifier = %flight.identifier, callsign = %cs, "Resolved callsign");
            flight.callsign = Some(cs.clone());
            changed = true;

            // Try to fetch route now that we have a callsign
            if flight.route.is_none()
                && let Ok(Ok(Some(route))) =
                    tokio::time::timeout(ROUTE_FETCH_TIMEOUT, aviation_client.get_flight_route(&cs))
                        .await
            {
                let origin = route.origin.iata_code.clone();
                let dest = route.destination.iata_code.clone();
                if let Some((lat, lon, _)) = iata_to_coords(&dest) {
                    flight.dest_lat = Some(lat);
                    flight.dest_lon = Some(lon);
                }
                flight.route = Some((origin, dest));
            }
        }
        if flight.hex.is_none()
            && let Some(hex) = &ac.hex
        {
            debug!(identifier = %flight.identifier, hex = %hex, "Resolved hex");
            flight.hex = Some(hex.clone());
        }
        if flight.aircraft_type.is_none()
            && let Some(t) = &ac.t
        {
            flight.aircraft_type = Some(t.clone());
        }

        // Check squawk changes
        if let Some(new_squawk) = &ac.squawk {
            let squawk_changed = flight.squawk.as_ref() != Some(new_squawk);
            if squawk_changed && let Some(meaning) = emergency_squawk_meaning(new_squawk) {
                messages.push(msg_squawk_emergency(flight, new_squawk, meaning));
            }
        }

        // Store previous position for divert detection
        let prev_lat = flight.lat;
        let prev_lon = flight.lon;

        // Update flight data
        flight.altitude_ft = altitude_ft(&ac);
        flight.vertical_rate_fpm = vertical_rate(&ac);
        flight.ground_speed_kts = ac.gs;
        flight.lat = ac.lat;
        flight.lon = ac.lon;
        flight.squawk = ac.squawk.clone();

        // Detect phase
        let new_phase = detect_phase(flight, &ac);
        let old_phase = flight.phase;

        if new_phase != old_phase {
            flight.phase = new_phase;
            flight.last_phase_change = Some(now);
            flight.polls_since_change = 0;
            changed = true;

            // Post phase change messages
            match new_phase {
                FlightPhase::Takeoff => messages.push(msg_takeoff(flight)),
                FlightPhase::Cruise => messages.push(msg_cruise(flight)),
                FlightPhase::Descent => messages.push(msg_descent(flight)),
                FlightPhase::Approach => messages.push(msg_approach(flight)),
                FlightPhase::Landing => messages.push(msg_landing(flight)),
                _ => {}
            }

            // Landing transitions to Ground after posting
            if new_phase == FlightPhase::Landing {
                flight.phase = FlightPhase::Ground;
            }
        } else {
            flight.polls_since_change += 1;
        }

        // Divert detection during Descent/Approach
        if matches!(flight.phase, FlightPhase::Descent | FlightPhase::Approach) {
            if let (
                Some(dest_lat),
                Some(dest_lon),
                Some(cur_lat),
                Some(cur_lon),
                Some(p_lat),
                Some(p_lon),
            ) = (
                flight.dest_lat,
                flight.dest_lon,
                flight.lat,
                flight.lon,
                prev_lat,
                prev_lon,
            ) {
                // Ground track: bearing from previous to current position
                let ground_track =
                    random_flight::geo::initial_bearing(p_lat, p_lon, cur_lat, cur_lon);
                // Bearing to destination
                let bearing_to_dest =
                    random_flight::geo::initial_bearing(cur_lat, cur_lon, dest_lat, dest_lon);

                // Angular difference
                let mut diff = (ground_track - bearing_to_dest).abs();
                if diff > 180.0 {
                    diff = 360.0 - diff;
                }

                if diff > DIVERT_BEARING_THRESHOLD {
                    flight.divert_consecutive_polls += 1;
                    if flight.divert_consecutive_polls == DIVERT_CONSECUTIVE_POLLS {
                        messages.push(msg_possible_divert(flight));
                    }
                } else {
                    flight.divert_consecutive_polls = 0;
                }
            }
        } else {
            flight.divert_consecutive_polls = 0;
        }
    }

    // Remove flights in reverse order to preserve indices
    for idx in removals.into_iter().rev() {
        state.flights.remove(idx);
        changed = true;
    }

    // Post messages
    for msg in messages {
        if let Err(e) = client.say(channel.to_string(), msg).await {
            error!(error = ?e, "Failed to send flight tracker message");
        }
    }

    // Persist if any state changed
    if changed {
        save_tracker_state(data_dir, state).await;
    }
}
