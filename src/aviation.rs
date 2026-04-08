use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Mutex;
use std::sync::Arc;
use tracing::{debug, error, warn};
use twitch_irc::message::PrivmsgMessage;

use crate::{APP_USER_AGENT, AuthenticatedTwitchClient, MAX_RESPONSE_LENGTH, truncate_response};

const ADSBDB_BASE_URL: &str = "https://api.adsbdb.com/v0";
const ADSBLOL_BASE_URL: &str = "https://api.adsb.lol/v2";
const UP_SEARCH_RADIUS_NM: u16 = 15;
const UP_COOLDOWN: Duration = Duration::from_secs(30);
const UP_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const UP_ADSBLOL_TIMEOUT: Duration = Duration::from_secs(10);
const UP_ADSBDB_TIMEOUT: Duration = Duration::from_secs(5);
const UP_NOMINATIM_TIMEOUT: Duration = Duration::from_secs(5);
const AIRLINE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const UP_MAX_CANDIDATES: usize = 10;
const UP_MAX_RESULTS: usize = 5;
const UP_CONE_REFERENCE_ALT_FT: f64 = 35_000.0;

// --- PLZ Lookup ---

const PLZ_DATA: &str = include_str!("../data/plz.csv");

fn plz_table() -> &'static HashMap<&'static str, (f64, f64)> {
    static TABLE: OnceLock<HashMap<&'static str, (f64, f64)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for line in PLZ_DATA.lines() {
            let mut parts = line.splitn(3, ',');
            let plz = parts.next().expect("malformed plz.csv: missing plz");
            let lat: f64 = parts
                .next()
                .expect("malformed plz.csv: missing lat")
                .parse()
                .expect("malformed plz.csv: invalid lat");
            let lon: f64 = parts
                .next()
                .expect("malformed plz.csv: missing lon")
                .parse()
                .expect("malformed plz.csv: invalid lon");
            map.insert(plz, (lat, lon));
        }
        map
    })
}

fn plz_to_coords(plz: &str) -> Option<(f64, f64)> {
    plz_table().get(plz).copied()
}

fn is_valid_plz(plz: &str) -> bool {
    plz.len() == 5 && plz.chars().all(|c| c.is_ascii_digit())
}

// --- Airport Lookup ---

const AIRPORT_DATA: &str = include_str!("../data/airports.csv");
const NOMINATIM_BASE_URL: &str = "https://nominatim.openstreetmap.org";

struct AirportData {
    by_icao: HashMap<String, (f64, f64, String)>,
    by_iata: HashMap<String, (f64, f64, String)>,
}

fn airport_data() -> &'static AirportData {
    static DATA: OnceLock<AirportData> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut by_icao = HashMap::new();
        let mut by_iata = HashMap::new();
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(AIRPORT_DATA.as_bytes());
        for result in reader.records() {
            let Ok(record) = result else { continue };
            if record.len() < 5 {
                continue;
            }
            let icao = record[0].trim();
            let iata = record[1].trim();
            let name = record[2].trim().to_string();
            let Ok(lat) = record[3].trim().parse::<f64>() else { continue };
            let Ok(lon) = record[4].trim().parse::<f64>() else { continue };

            // Only insert 4-letter codes into by_icao (codes are already uppercase in data)
            if icao.len() == 4 {
                by_icao.insert(icao.to_string(), (lat, lon, name.clone()));
            }
            // Insert non-empty IATA codes
            if iata.len() == 3 {
                by_iata.insert(iata.to_string(), (lat, lon, name));
            }
        }
        AirportData { by_icao, by_iata }
    })
}

fn icao_to_coords(code: &str) -> Option<(f64, f64, &'static str)> {
    airport_data().by_icao.get(code).map(|(lat, lon, name)| (*lat, *lon, name.as_str()))
}

pub(crate) fn iata_to_coords(code: &str) -> Option<(f64, f64, &'static str)> {
    airport_data().by_iata.get(code).map(|(lat, lon, name)| (*lat, *lon, name.as_str()))
}

// --- Airline IATA-to-ICAO Lookup ---

const AIRLINE_DATA: &str = include_str!("../data/airlines.csv");

/// Returns the static IATA→ICAO airline code table (lazy-initialized).
fn airline_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for line in AIRLINE_DATA.lines() {
            let Some((iata, icao)) = line.split_once(',') else {
                continue;
            };
            let iata = iata.trim();
            let icao = icao.trim();
            if iata.len() == 2 && icao.len() == 3 {
                map.insert(iata, icao);
            }
        }
        map
    })
}

/// Check if input looks like an IATA flight number (2 letters + 1-4 digits).
fn is_iata_flight_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 3 || bytes.len() > 6 {
        return false;
    }
    bytes[0].is_ascii_uppercase()
        && bytes[1].is_ascii_uppercase()
        && bytes[2..].iter().all(|b| b.is_ascii_digit())
}

fn is_icao_pattern(s: &str) -> bool {
    s.len() == 4 && s.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_iata_pattern(s: &str) -> bool {
    s.len() == 3 && s.chars().all(|c| c.is_ascii_alphabetic())
}

// --- Location Resolution ---

struct ResolvedLocation {
    lat: f64,
    lon: f64,
    display_name: String,
}

enum ResolveResult {
    Found(ResolvedLocation),
    PlzNotFound,
    NotFound,
}

// --- adsb.lol types ---

#[derive(Debug, Deserialize)]
pub(crate) struct AdsbLolResponse {
    #[serde(default)]
    pub(crate) ac: Vec<NearbyAircraft>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NearbyAircraft {
    pub(crate) hex: Option<String>,
    pub(crate) flight: Option<String>,
    pub(crate) t: Option<String>,
    pub(crate) alt_baro: Option<AltBaro>,
    pub(crate) lat: Option<f64>,
    pub(crate) lon: Option<f64>,
    pub(crate) gs: Option<f64>,
    pub(crate) baro_rate: Option<i64>,
    pub(crate) geom_rate: Option<i64>,
    pub(crate) squawk: Option<String>,
    pub(crate) nav_modes: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) enum AltBaro {
    Feet(i64),
    Ground,
}

impl<'de> Deserialize<'de> for AltBaro {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(AltBaro::Feet(i))
                } else {
                    Ok(AltBaro::Feet(n.as_f64().unwrap_or(0.0) as i64))
                }
            }
            serde_json::Value::String(s) if s == "ground" => Ok(AltBaro::Ground),
            _ => Ok(AltBaro::Ground),
        }
    }
}

// --- adsbdb types ---

#[derive(Debug, Deserialize)]
struct AdsbDbResponse {
    response: AdsbDbResponseInner,
}

#[derive(Debug, Deserialize)]
struct AdsbDbResponseInner {
    flightroute: Option<FlightRoute>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FlightRoute {
    pub(crate) origin: Airport,
    pub(crate) destination: Airport,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Airport {
    pub(crate) iata_code: String,
}

// --- adsbdb airline types ---

#[derive(Debug, Deserialize)]
struct AdsbDbAirlineResponse {
    response: Vec<AdsbDbAirline>,
}

#[derive(Debug, Deserialize)]
struct AdsbDbAirline {
    icao: String,
}

// --- Nominatim types ---

#[derive(Debug, Deserialize)]
struct NominatimResult {
    lat: String,
    lon: String,
    display_name: String,
}

// --- AviationClient ---

#[derive(Clone)]
pub struct AviationClient(reqwest::Client);

impl AviationClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .build()
            .wrap_err("Failed to build aviation HTTP client")?;
        Ok(Self(http))
    }

    async fn get_aircraft_nearby(
        &self,
        lat: f64,
        lon: f64,
        radius_nm: u16,
    ) -> Result<Vec<NearbyAircraft>> {
        let url = format!("{ADSBLOL_BASE_URL}/point/{lat}/{lon}/{radius_nm}");
        debug!(url = %url, "Fetching nearby aircraft from adsb.lol");

        let resp: AdsbLolResponse = self
            .0
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsb.lol")?
            .error_for_status()
            .wrap_err("adsb.lol returned error status")?
            .json()
            .await
            .wrap_err("Failed to parse adsb.lol response")?;

        debug!(count = resp.ac.len(), "Received aircraft from adsb.lol");
        Ok(resp.ac)
    }

    pub(crate) async fn get_aircraft_by_hex(&self, hex: &str) -> Result<Option<NearbyAircraft>> {
        let url = format!("{ADSBLOL_BASE_URL}/hex/{hex}");
        debug!(hex = %hex, "Fetching aircraft by hex from adsb.lol");

        let resp: AdsbLolResponse = self
            .0
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsb.lol")?
            .error_for_status()
            .wrap_err("adsb.lol returned error status")?
            .json()
            .await
            .wrap_err("Failed to parse adsb.lol response")?;

        Ok(resp.ac.into_iter().next())
    }

    pub(crate) async fn get_aircraft_by_callsign(
        &self,
        callsign: &str,
    ) -> Result<Option<NearbyAircraft>> {
        let url = format!("{ADSBLOL_BASE_URL}/callsign/{callsign}");
        debug!(callsign = %callsign, "Fetching aircraft by callsign from adsb.lol");

        let resp: AdsbLolResponse = self
            .0
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsb.lol")?
            .error_for_status()
            .wrap_err("adsb.lol returned error status")?
            .json()
            .await
            .wrap_err("Failed to parse adsb.lol response")?;

        Ok(resp.ac.into_iter().next())
    }

    pub(crate) async fn get_flight_route(
        &self,
        callsign: &str,
    ) -> Result<Option<FlightRoute>> {
        let url = format!("{ADSBDB_BASE_URL}/callsign/{callsign}");
        debug!(callsign = %callsign, "Fetching flight route from adsbdb");

        let resp = self
            .0
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb response")?;

        Ok(body.response.flightroute)
    }

    /// Resolve a potential IATA flight number to an ICAO callsign.
    pub(crate) async fn resolve_callsign(&self, input: &str) -> String {
        if !is_iata_flight_number(input) {
            return input.to_string();
        }

        let (airline_iata, flight_num) = input.split_at(2);

        // Try static CSV lookup first
        if let Some(&icao) = airline_table().get(airline_iata) {
            debug!(iata = %airline_iata, icao = %icao, "Resolved airline code via CSV");
            return format!("{icao}{flight_num}");
        }

        // Fallback: query adsbdb airline API
        debug!(iata = %airline_iata, "Airline not in CSV, trying adsbdb API");
        match tokio::time::timeout(
            AIRLINE_LOOKUP_TIMEOUT,
            self.lookup_airline_icao(airline_iata),
        )
        .await
        {
            Ok(Ok(Some(icao))) => {
                warn!(
                    iata = %airline_iata,
                    icao = %icao,
                    "Resolved airline via adsbdb API — consider adding to airlines.csv"
                );
                format!("{icao}{flight_num}")
            }
            Ok(Ok(None)) => {
                debug!(iata = %airline_iata, "Airline not found in adsbdb");
                input.to_string()
            }
            Ok(Err(e)) => {
                warn!(error = ?e, iata = %airline_iata, "adsbdb airline lookup failed");
                input.to_string()
            }
            Err(_) => {
                warn!(iata = %airline_iata, "adsbdb airline lookup timed out");
                input.to_string()
            }
        }
    }

    /// Query adsbdb for an airline's ICAO code by IATA code.
    async fn lookup_airline_icao(&self, iata: &str) -> Result<Option<String>> {
        let url = format!("{ADSBDB_BASE_URL}/airline/{iata}");
        debug!(url = %url, "Fetching airline from adsbdb");

        let resp = self
            .0
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbAirlineResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb airline response")?;

        Ok(body.response.into_iter().next().map(|a| a.icao))
    }

    async fn geocode_nominatim(&self, query: &str) -> Result<Option<ResolvedLocation>> {
        let url = format!("{NOMINATIM_BASE_URL}/search");
        debug!(query = %query, "Geocoding via Nominatim");

        let resp = self
            .0
            .get(&url)
            .query(&[("q", query), ("format", "json"), ("limit", "1")])
            .send()
            .await
            .wrap_err("Failed to send request to Nominatim")?
            .error_for_status()
            .wrap_err("Nominatim returned error status")?;

        let results: Vec<NominatimResult> = resp
            .json()
            .await
            .wrap_err("Failed to parse Nominatim response")?;

        let Some(first) = results.into_iter().next() else {
            debug!(query = %query, "Nominatim returned no results");
            return Ok(None);
        };

        let lat: f64 = first.lat.parse().wrap_err("Invalid lat from Nominatim")?;
        let lon: f64 = first.lon.parse().wrap_err("Invalid lon from Nominatim")?;

        // Trim display_name to first comma-separated segment
        let display_name = first
            .display_name
            .split(',')
            .next()
            .unwrap_or(&first.display_name)
            .trim()
            .to_string();

        debug!(query = %query, lat = %lat, lon = %lon, display = %display_name, "Nominatim resolved");
        Ok(Some(ResolvedLocation { lat, lon, display_name }))
    }
}

async fn resolve_location(
    input: &str,
    aviation_client: &AviationClient,
) -> Result<ResolveResult> {
    // 1. PLZ: 5 ASCII digits — no fallthrough on miss
    if is_valid_plz(input) {
        return match plz_to_coords(input) {
            Some((lat, lon)) => Ok(ResolveResult::Found(ResolvedLocation {
                lat,
                lon,
                display_name: input.to_string(),
            })),
            None => Ok(ResolveResult::PlzNotFound),
        };
    }

    // Uppercase once for airport code lookups (keys stored uppercase)
    let upper = input.to_uppercase();

    // 2. ICAO: 4 ASCII letters — falls through to Nominatim on miss
    if is_icao_pattern(input) && let Some((lat, lon, name)) = icao_to_coords(&upper) {
        return Ok(ResolveResult::Found(ResolvedLocation {
            lat,
            lon,
            display_name: name.to_string(),
        }));
    }

    // 3. IATA: 3 ASCII letters — falls through to Nominatim on miss
    if is_iata_pattern(input) && let Some((lat, lon, name)) = iata_to_coords(&upper) {
        return Ok(ResolveResult::Found(ResolvedLocation {
            lat,
            lon,
            display_name: name.to_string(),
        }));
    }

    // 4. Nominatim: universal fallback
    let result = tokio::time::timeout(
        UP_NOMINATIM_TIMEOUT,
        aviation_client.geocode_nominatim(input),
    )
    .await;

    match result {
        Ok(Ok(Some(location))) => Ok(ResolveResult::Found(location)),
        Ok(Ok(None)) => Ok(ResolveResult::NotFound),
        Ok(Err(e)) => Err(e.wrap_err("Nominatim geocoding failed")),
        Err(_) => {
            warn!(input = %input, "Nominatim request timed out");
            Err(eyre::eyre!("Nominatim request timed out"))
        }
    }
}

// --- Command ---

fn cone_distance_nm(ac: &NearbyAircraft, center_lat: f64, center_lon: f64) -> Option<f64> {
    let (Some(ac_lat), Some(ac_lon), Some(alt)) = (ac.lat, ac.lon, &ac.alt_baro) else {
        return None;
    };
    let alt_ft = match alt {
        AltBaro::Feet(ft) if *ft > 0 => *ft as f64,
        _ => return None,
    };
    let distance = random_flight::geo::haversine_distance_nm(center_lat, center_lon, ac_lat, ac_lon);
    let max_distance = alt_ft * UP_SEARCH_RADIUS_NM as f64 / UP_CONE_REFERENCE_ALT_FT;
    if distance <= max_distance { Some(distance) } else { None }
}

fn format_altitude(alt: &Option<AltBaro>) -> String {
    match alt {
        Some(AltBaro::Feet(ft)) if *ft >= 1000 => format!("FL{}", ft / 100),
        Some(AltBaro::Feet(ft)) => format!("{ft}ft"),
        Some(AltBaro::Ground) => "GND".to_string(),
        None => "?".to_string(),
    }
}

pub async fn up_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    aviation_client: &AviationClient,
    input: &str,
    cooldowns: &Arc<Mutex<HashMap<String, std::time::Instant>>>,
) -> Result<()> {
    let user = &privmsg.sender.login;
    let input = input.trim();

    // Empty input
    if input.is_empty() {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Benutzung: !up <PLZ/ICAO/IATA/Ort> FDM".to_string())
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    }

    // Check cooldown
    {
        let cooldowns_guard = cooldowns.lock().await;
        if let Some(last_use) = cooldowns_guard.get(user) {
            let elapsed = last_use.elapsed();
            if elapsed < UP_COOLDOWN {
                let remaining = UP_COOLDOWN - elapsed;
                debug!(user = %user, remaining_secs = remaining.as_secs(), "!up on cooldown");
                if let Err(e) = client
                    .say_in_reply_to(privmsg, "Bitte warte noch ein bisschen Waiting".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send cooldown message");
                }
                return Ok(());
            }
        }
    }

    // Set cooldown before resolver (Nominatim is a network call)
    {
        let mut cooldowns_guard = cooldowns.lock().await;
        cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
    }

    // Resolve location
    let location = match resolve_location(input, aviation_client).await {
        Ok(ResolveResult::Found(loc)) => loc,
        Ok(ResolveResult::PlzNotFound) => {
            if let Err(e) = client
                .say_in_reply_to(privmsg, "Kenne ich nicht die PLZ FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send unknown PLZ message");
            }
            return Ok(());
        }
        Ok(ResolveResult::NotFound) => {
            if let Err(e) = client
                .say_in_reply_to(privmsg, "Kenne ich nicht FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send not-found message");
            }
            return Ok(());
        }
        Err(e) => {
            error!(error = ?e, input = %input, "Location resolution failed");
            if let Err(e) = client
                .say_in_reply_to(privmsg, "Da ist was schiefgelaufen FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send error message");
            }
            return Ok(());
        }
    };

    let ResolvedLocation { lat, lon, display_name } = &location;
    debug!(input = %input, lat = %lat, lon = %lon, display = %display_name, "Looking up aircraft");

    // Wrap entire API flow in overall timeout
    let result = tokio::time::timeout(UP_COMMAND_TIMEOUT, async {
        // Fetch nearby aircraft
        let aircraft = tokio::time::timeout(
            UP_ADSBLOL_TIMEOUT,
            aviation_client.get_aircraft_nearby(*lat, *lon, UP_SEARCH_RADIUS_NM),
        )
        .await
        .map_err(|_| eyre::eyre!("adsb.lol request timed out"))?
        .wrap_err("adsb.lol request failed")?;

        // Filter by cone visibility, then by callsign
        let candidates: Vec<_> = aircraft
            .iter()
            .filter_map(|ac| {
                let distance_nm = cone_distance_nm(ac, *lat, *lon)?;
                let callsign = ac.flight.as_ref()?.trim();
                if callsign.is_empty() {
                    return None;
                }
                Some((callsign.to_string(), ac, distance_nm))
            })
            .take(UP_MAX_CANDIDATES)
            .collect();

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch routes concurrently
        let mut join_set = tokio::task::JoinSet::new();
        for (callsign, ac, distance_nm) in &candidates {
            let av_client = aviation_client.clone();
            let cs = callsign.clone();
            let icao_type = ac.t.clone();
            let alt = ac.alt_baro.clone();
            let dist = *distance_nm;
            let (ac_lat, ac_lon) = (
                ac.lat.expect("lat guaranteed by cone_distance_nm"),
                ac.lon.expect("lon guaranteed by cone_distance_nm"),
            );
            let bearing = random_flight::geo::initial_bearing(*lat, *lon, ac_lat, ac_lon);
            let direction = match random_flight::geo::cardinal_direction(bearing) {
                "N" => "↑", "NE" => "↗", "E" => "→", "SE" => "↘",
                "S" => "↓", "SW" => "↙", "W" => "←", "NW" => "↖",
                _ => "?",
            };
            join_set.spawn(async move {
                let route = tokio::time::timeout(
                    UP_ADSBDB_TIMEOUT,
                    av_client.get_flight_route(&cs),
                )
                .await;

                match route {
                    Ok(Ok(Some(fr))) => Some((cs, icao_type, alt, fr, dist, direction)),
                    Ok(Ok(None)) => None,
                    Ok(Err(e)) => {
                        warn!(callsign = %cs, error = ?e, "adsbdb lookup failed");
                        None
                    }
                    Err(_) => {
                        warn!(callsign = %cs, "adsbdb lookup timed out");
                        None
                    }
                }
            });
        }

        let mut results = Vec::new();
        while let Some(res) = join_set.join_next().await {
            if let Ok(Some(entry)) = res {
                results.push(entry);
            }
        }

        Ok::<_, eyre::Report>(results)
    })
    .await;

    let response = match result {
        Ok(Ok(entries)) if entries.is_empty() => {
            format!("Nix los über {display_name}")
        }
        Ok(Ok(entries)) => {
            let total = entries.len();
            let parts: Vec<String> = entries
                .iter()
                .take(UP_MAX_RESULTS)
                .map(|(cs, icao_type, alt, route, dist, direction)| {
                    let typ = icao_type.as_deref().unwrap_or("?");
                    format!(
                        "{cs} ({typ}) {origin}→{dest} {alt} {dist:.1}nm {direction}",
                        origin = route.origin.iata_code,
                        dest = route.destination.iata_code,
                        alt = format_altitude(alt),
                    )
                })
                .collect();
            let joined = parts.join(" | ");
            let msg = format!("✈ {total} Flieger über {display_name}: {joined}");
            truncate_response(&msg, MAX_RESPONSE_LENGTH)
        }
        Ok(Err(e)) => {
            error!(error = ?e, input = %input, "!up command failed");
            "Da ist was schiefgelaufen FDM".to_string()
        }
        Err(_) => {
            error!(input = %input, "!up command timed out");
            "Da ist was schiefgelaufen FDM".to_string()
        }
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response).await {
        error!(error = ?e, "Failed to send !up response");
    }

    Ok(())
}
