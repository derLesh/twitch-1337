# Design: Extended Location Resolution for !up Command

**Date**: 2026-03-25
**Status**: Draft

## Overview

Extend the `!up` command to accept ICAO codes, IATA codes, and free-text place names in addition to German postal codes (PLZ). Uses a layered resolver chain: offline lookups for PLZ and airport codes, Nominatim geocoding API as fallback for arbitrary place names.

## Requirements

- Accept 5-digit German PLZ (existing behavior, unchanged)
- Accept 4-letter ICAO airport codes (e.g., EDDF) — case-insensitive
- Accept 3-letter IATA airport codes (e.g., FRA) — case-insensitive
- Accept multi-word free-text place names (e.g., "Hauptbahnhof Stuttgart") — worldwide scope
- Deterministic priority: PLZ > ICAO > IATA > Nominatim
- Airport codes resolved offline via embedded dataset; free-text via OpenStreetMap Nominatim API

## Input Parsing

The `up_command` function currently takes `Option<&str>` (single word via `words.next()`). This changes to collecting all remaining words after `!up` into a single trimmed string. If the collected input is empty after trimming, return the usage message immediately.

New function signature:
```rust
pub async fn up_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    aviation_client: &AviationClient,
    input: &str,  // was: plz: Option<&str>
    cooldowns: &Arc<Mutex<HashMap<String, std::time::Instant>>>,
) -> Result<()>
```

Call site changes from `words.next()` to `words.collect::<Vec<_>>().join(" ")`.

### Cooldown sequencing

Cooldown is set **before** calling `resolve_location()`, since Nominatim is a network call and we must prevent abuse. This means even failed lookups consume the cooldown — a change from the current behavior where validation is cheap and cooldown is set after validation. This is acceptable: a user sending invalid inputs repeatedly should still be rate-limited.

## ResolvedLocation Type

```rust
struct ResolvedLocation {
    lat: f64,
    lon: f64,
    display_name: String,
}
```

The `display_name` is used in chat output (e.g., "Frankfurt am Main Airport", "53111", "Stuttgart"). For PLZ, the display name is the PLZ itself. For airport codes, it's the airport name from the dataset. For Nominatim, it's the first segment of the returned `display_name` (trimmed at first comma).

## Resolver Chain

```rust
enum ResolveResult {
    Found(ResolvedLocation),
    PlzNotFound,       // 5-digit input not in PLZ dataset
    NotFound,          // all resolvers exhausted
}

async fn resolve_location(
    input: &str,
    aviation_client: &AviationClient,
) -> Result<ResolveResult>
```

This is a **hybrid waterfall**: pattern-matched resolvers are tried first based on input shape, but Nominatim always serves as the final fallback regardless of input pattern. The chain tries resolvers in order and returns the first match:

1. **PLZ resolver** (offline): if input is 5 ASCII digits → lookup in PLZ HashMap. If found, return `Found`. If not found, return `PlzNotFound` (do not fall through — a 5-digit number is unambiguously a postal code attempt).
2. **ICAO resolver** (offline): if input is 4 ASCII letters → uppercase → lookup in `AirportData.by_icao`. If found, return `Found`. If not found, **fall through** to Nominatim.
3. **IATA resolver** (offline): if input is 3 ASCII letters → uppercase → lookup in `AirportData.by_iata`. If found, return `Found`. If not found, **fall through** to Nominatim.
4. **Nominatim resolver** (network): always tried as final fallback for any input that wasn't resolved by steps 1-3.

If Nominatim also returns no results, returns `NotFound`. Network errors propagate as `Err`.

## Airport Data Embedding

### Data source

OurAirports dataset from `https://github.com/davidmegginson/ourairports-data` (public domain). Contains ~80k airports worldwide including all sizes.

### Script: `scripts/update-airports.py`

Downloads OurAirports `airports.csv`, extracts columns: `ident` (ICAO), `iata_code`, `name`, `latitude_deg`, `longitude_deg`. Writes to `data/airports.csv` with format:

```
EDDF,FRA,Frankfurt am Main Airport,50.033333,8.570556
EDDM,MUC,Munich Airport,48.353783,11.786086
```

No header row (matching the existing `plz.csv` convention). Rows with empty ICAO codes are skipped. Empty IATA codes are preserved as empty strings (skipped during IATA map construction).

### Runtime loading

```rust
static AIRPORT_DATA: OnceLock<AirportData> = OnceLock::new();

struct AirportData {
    by_icao: HashMap<String, (f64, f64, String)>,  // ICAO -> (lat, lon, name)
    by_iata: HashMap<String, (f64, f64, String)>,  // IATA -> (lat, lon, name)
}
```

Loaded via `include_str!("../data/airports.csv")`, parsed on first access (same pattern as PLZ data). Keys stored as uppercase. Not all airports have IATA codes; those are only inserted into `by_icao`.

**Note on the `ident` field**: OurAirports uses `ident` for ICAO-style codes, but small airports may have non-standard local identifiers (e.g., US FAA codes). During map construction, only 4-letter `ident` codes are inserted into `by_icao` (filter by length to avoid unreachable entries). All entries with valid IATA codes are inserted into `by_iata` regardless.

### Binary size impact

~80k airports at ~50-60 bytes per CSV line adds ~4-5MB of embedded string data. This roughly doubles the current ~6MB binary. This is an accepted trade-off for comprehensive coverage.

## Nominatim Integration

### Method

```rust
impl AviationClient {
    async fn geocode_nominatim(&self, query: &str) -> Result<Option<ResolvedLocation>>
}
```

### Request

```
GET https://nominatim.openstreetmap.org/search?q={query}&format=json&limit=1
```

Built using reqwest's `.query(&[("q", input), ("format", "json"), ("limit", "1")])` method (not string formatting) to ensure proper URL encoding of spaces and special characters in multi-word queries.

The existing `APP_USER_AGENT` header on the reqwest client satisfies Nominatim's usage policy. Nominatim also requires max 1 request/second — the existing 30s per-user cooldown on `!up` makes this a non-issue in practice.

### Response

```rust
struct NominatimResult {
    lat: String,   // parsed to f64
    lon: String,   // parsed to f64
    display_name: String,
}
```

Takes the first result. `display_name` is trimmed to the first comma-separated segment for concise chat output (e.g., "Stuttgart, Baden-Württemberg, Germany" becomes "Stuttgart").

### Timeout

5 seconds (defined as `UP_NOMINATIM_TIMEOUT` constant, matching the existing `UP_ADSBDB_TIMEOUT`).

### Error handling

- No results → returns `None`
- Request failure → logged as warning, propagated as error

## Error Messages

| Scenario | Chat response |
|----------|--------------|
| Empty input (no argument) | `"Benutzung: !up <PLZ/ICAO/IATA/Ort> FDM"` |
| PLZ not found in dataset | `"Kenne ich nicht die PLZ FDM"` |
| All resolvers exhausted (including Nominatim) | `"Kenne ich nicht FDM"` |
| Nominatim/API failure | `"Da ist was schiefgelaufen FDM"` |

Note: ICAO/IATA codes that don't match the embedded dataset fall through to Nominatim rather than producing code-specific errors. Only PLZ has a specific "not found" message because invalid PLZs don't fall through (a 5-digit number is unambiguously a postal code attempt).

## Edge Cases

- **3-letter IATA vs place name**: "FRA" is tried as IATA first. If someone means a place called "Fra", they can type the full name.
- **4-letter non-ICAO words**: "BIER" checked as ICAO, not found, falls through to Nominatim which geocodes it as a place name.
- **Case insensitivity**: Input is uppercased before airport code lookup. "fra", "Fra", "FRA" all resolve to Frankfurt Airport.
- **Multi-word input with leading/trailing spaces**: Trimmed before processing.

## Chat Output Format

The output format is unchanged except the location label uses `ResolvedLocation.display_name`:

```
✈ 3 Flieger über Frankfurt am Main Airport: DLH456 (A321) TXL→CDG FL350 | ...
✈ 5 Flieger über 53111: DLH456 (A321) TXL→CDG FL350 | ...
✈ 2 Flieger über Stuttgart: EWG123 (A320) STR→BCN FL340 | ...
Nix los über Frankfurt am Main Airport
```

## Scope of Changes

### New files
- `data/airports.csv` — embedded airport dataset
- `scripts/update-airports.py` — download/transform script

### Modified: `src/main.rs` (aviation module)
- Add `ResolvedLocation` struct
- Add `AirportData` struct with `OnceLock` and two HashMaps
- Add `resolve_location()` resolver chain function
- Add `geocode_nominatim()` method on `AviationClient`
- Add `NominatimResult` response struct
- Modify `up_command()`: collect multi-word input, use resolver chain, pass display_name to response formatting
- Update error messages

### Unchanged
- adsb.lol / adsbdb API calls and types
- Aircraft lookup logic, altitude formatting, cooldowns
- Dockerfile (already copies `data/`)
- All other handlers and modules
