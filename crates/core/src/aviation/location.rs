//! Embedded location data + IATA/ICAO predicates + location resolution.
//!
//! Static tables (PLZ, airports, airlines) are baked in via `include_str!`
//! and lazy-initialized into `OnceLock` `HashMap`s. Predicate helpers
//! (`is_iata_flight_number` etc.) are used by both `client.rs` (for
//! `resolve_callsign` / `aviationstack_query`) and `resolve_location`.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use eyre::Result;
use tracing::{trace, warn};

use super::AviationClient;

const UP_NOMINATIM_TIMEOUT: Duration = Duration::from_secs(5);

// --- PLZ Lookup ---

const PLZ_DATA: &str = include_str!("../../data/plz.csv");

fn plz_table() -> &'static HashMap<&'static str, (f64, f64)> {
    static TABLE: OnceLock<HashMap<&'static str, (f64, f64)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        trace!("Initializing PLZ lookup table from CSV data");
        let mut map = HashMap::new();
        for line in PLZ_DATA.lines() {
            let mut parts = line.splitn(3, ',');
            let (Some(plz), Some(lat_str), Some(lon_str)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Ok(lat) = lat_str.parse::<f64>() else {
                continue;
            };
            let Ok(lon) = lon_str.parse::<f64>() else {
                continue;
            };
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

const AIRPORT_DATA: &str = include_str!("../../data/airports.csv");

struct AirportData {
    by_icao: HashMap<String, (f64, f64, String)>,
    by_iata: HashMap<String, (f64, f64, String)>,
}

fn airport_data() -> &'static AirportData {
    static DATA: OnceLock<AirportData> = OnceLock::new();
    DATA.get_or_init(|| {
        trace!("Initializing airport lookup tables from CSV data");
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
            let Ok(lat) = record[3].trim().parse::<f64>() else {
                continue;
            };
            let Ok(lon) = record[4].trim().parse::<f64>() else {
                continue;
            };

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
    airport_data()
        .by_icao
        .get(code)
        .map(|(lat, lon, name)| (*lat, *lon, name.as_str()))
}

pub(crate) fn iata_to_coords(code: &str) -> Option<(f64, f64, &'static str)> {
    airport_data()
        .by_iata
        .get(code)
        .map(|(lat, lon, name)| (*lat, *lon, name.as_str()))
}

// --- Airline IATA-to-ICAO Lookup ---

const AIRLINE_DATA: &str = include_str!("../../data/airlines.csv");

/// Returns the static IATA→ICAO airline code table (lazy-initialized).
pub(super) fn airline_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        trace!("Initializing airline IATA→ICAO lookup table from CSV data");
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
pub(super) fn is_iata_flight_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 3 || bytes.len() > 6 {
        return false;
    }
    bytes[0].is_ascii_uppercase()
        && bytes[1].is_ascii_uppercase()
        && bytes[2..].iter().all(u8::is_ascii_digit)
}

pub(super) fn is_icao_flight_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 4 || bytes.len() > 7 {
        return false;
    }
    bytes[..3].iter().all(u8::is_ascii_uppercase)
        && bytes[..3].iter().all(u8::is_ascii_alphabetic)
        && bytes[3..].iter().all(u8::is_ascii_digit)
}

fn is_icao_pattern(s: &str) -> bool {
    s.len() == 4 && s.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_iata_pattern(s: &str) -> bool {
    s.len() == 3 && s.chars().all(|c| c.is_ascii_alphabetic())
}

// --- Location Resolution ---

pub(super) struct ResolvedLocation {
    pub lat: f64,
    pub lon: f64,
    pub display_name: String,
}

pub(super) enum ResolveResult {
    Found(ResolvedLocation),
    PlzNotFound,
    NotFound,
}

pub(super) async fn resolve_location(
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
    if is_icao_pattern(input)
        && let Some((lat, lon, name)) = icao_to_coords(&upper)
    {
        return Ok(ResolveResult::Found(ResolvedLocation {
            lat,
            lon,
            display_name: name.to_string(),
        }));
    }

    // 3. IATA: 3 ASCII letters — falls through to Nominatim on miss
    if is_iata_pattern(input)
        && let Some((lat, lon, name)) = iata_to_coords(&upper)
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airline_table_contains_known_mappings() {
        let table = airline_table();
        assert_eq!(table.get("TP"), Some(&"TAP"));
        assert_eq!(table.get("LH"), Some(&"DLH"));
        assert_eq!(table.get("BA"), Some(&"BAW"));
    }

    #[test]
    fn airline_table_is_nonempty() {
        assert!(airline_table().len() > 100);
    }

    #[test]
    fn is_iata_flight_number_valid() {
        assert!(is_iata_flight_number("TP247"));
        assert!(is_iata_flight_number("LH5765"));
        assert!(is_iata_flight_number("BA12"));
        assert!(is_iata_flight_number("AA1"));
        assert!(is_iata_flight_number("EI1234"));
    }

    #[test]
    fn is_iata_flight_number_rejects_icao() {
        assert!(!is_iata_flight_number("TAP247"));
        assert!(!is_iata_flight_number("DLH5765"));
        assert!(!is_iata_flight_number("BAW12"));
    }

    #[test]
    fn is_iata_flight_number_rejects_invalid() {
        assert!(!is_iata_flight_number(""));
        assert!(!is_iata_flight_number("T"));
        assert!(!is_iata_flight_number("TP"));
        assert!(!is_iata_flight_number("12345"));
        assert!(!is_iata_flight_number("ABCDEF"));
        assert!(!is_iata_flight_number("TP12345")); // too many digits
    }
}
