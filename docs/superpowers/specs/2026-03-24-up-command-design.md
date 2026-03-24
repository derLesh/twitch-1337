# Design: `!up <plz>` Command — Aircraft Overhead Lookup

**Date:** 2026-03-24
**Status:** Approved

## Overview

A chat command `!up <german_zip_code>` that shows aircraft currently flying overhead with their routes. Combines two free, keyless APIs (adsb.lol for live positions, adsbdb for route data) with an embedded German postal code lookup table.

## User Experience

**Input:** `!up 60313`

**Output:** `✈ 3 aircraft near 60313: RJA33 AMM→MST FL320 | RYR47AW ACE→BER FL370 | BEL9SW BRU→FRA FL320`

**Error responses (German, matching bot tone):**
- Invalid PLZ format: `"Das ist keine gültige PLZ FDM"`
- Unknown PLZ: `"Kenne ich nicht die PLZ FDM"`
- No aircraft or no routes found: `"Nix los über {plz}"`
- API failure: `"Da ist was schiefgelaufen FDM"` (details logged at error level)

**Cooldown:** 30 seconds per user.

## External APIs

### adsb.lol — Live Aircraft Positions

- **Endpoint:** `GET https://api.adsb.lol/v2/point/{lat}/{lon}/{radius_nm}`
- **Auth:** None
- **Rate limits:** Dynamic, unspecified
- **License:** ODbL 1.0
- **Timeout:** 10 seconds
- **Fields used per aircraft:** `hex`, `flight` (callsign), `t` (aircraft type), `alt_baro` (altitude)
- **Search radius:** 15 NM (~28 km)
- **Note:** Does NOT include route/origin/destination — only ADS-B telemetry

### adsbdb — Route & Airline Lookup

- **Endpoint:** `GET https://api.adsbdb.com/v0/callsign/{callsign}`
- **Auth:** None
- **Rate limits:** Unspecified
- **Timeout:** 5 seconds per request
- **Fields used:** `origin.iata_code`, `destination.iata_code` from the `flightroute` response
- **Note:** Returns full airport details, airline info; we only need IATA codes

## Architecture

### Module: `src/aviation.rs`

All aviation-related logic lives in a single new module. The command handler in `main.rs` dispatches to it.

### Components

#### 1. PLZ Lookup (Embedded)

- **Data file:** `data/plz.csv` — columns: `plz,lat,lon`
- **Size:** ~8,400 rows, ~200 KB
- **Embedding:** `const PLZ_DATA: &str = include_str!("../data/plz.csv")`
- **Lookup:** `fn plz_to_coords(plz: &str) -> Option<(f64, f64)>`
- **Storage:** `OnceLock<HashMap<String, (f64, f64)>>` — parsed once on first call
- **Source:** Public German postal code centroid dataset

#### 2. adsb.lol Client

```rust
async fn get_aircraft_nearby(
    http: &reqwest::Client,
    lat: f64,
    lon: f64,
    radius_nm: u16,
) -> Result<Vec<NearbyAircraft>>
```

**`NearbyAircraft` struct (deserialized):**
- `hex: String`
- `flight: Option<String>` — callsign, needs trimming
- `t: Option<String>` — aircraft type code (e.g. "A321", "B38M")
- `alt_baro: Option<AltBaro>` — altitude in feet or "ground"

Uses `#[serde(deny_unknown_fields)]` is NOT set — extra fields are silently ignored via `#[serde(flatten)]` or just not declared.

#### 3. adsbdb Client

```rust
async fn get_route(
    http: &reqwest::Client,
    callsign: &str,
) -> Result<Option<FlightRoute>>
```

**`FlightRoute` struct (deserialized, nested):**
- `origin.iata_code: String`
- `destination.iata_code: String`

Returns `Ok(None)` when adsbdb has no route data for the callsign (not an error).

#### 4. Command Function

```rust
pub async fn up_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    http: &reqwest::Client,
    plz: Option<&str>,
    cooldowns: &Arc<Mutex<HashMap<String, Instant>>>,
) -> Result<()>
```

**Flow:**
1. Check per-user cooldown (30s). Return silently if on cooldown.
2. Validate PLZ argument exists and is 5 digits.
3. Look up coordinates from embedded table.
4. Fetch aircraft via adsb.lol (15 NM radius).
5. Filter to aircraft with non-empty callsigns.
6. Fetch routes concurrently via adsbdb — up to 10 candidates in parallel using `tokio::JoinSet`.
7. Filter to aircraft with known routes. Cap at 5 results.
8. Format output message. Send via `client.say_in_reply_to()`.
9. Update cooldown timestamp.

### Data Flow

```
"!up 60313"
  │
  ▼
Validate PLZ ──invalid──▶ "Das ist keine gültige PLZ FDM"
  │
  ▼
PLZ → (lat, lon) ──not found──▶ "Kenne ich nicht die PLZ FDM"
  │
  ▼
adsb.lol /v2/point/50.11/8.68/15 ──error──▶ "Da ist was schiefgelaufen FDM"
  │
  ▼
Filter aircraft with callsigns (take up to 10 candidates)
  │
  ▼
adsbdb /v0/callsign/{cs} × N (concurrent) ──individual failures silently skipped──
  │
  ▼
Filter to known routes, cap at 5
  │
  ▼
0 results ──▶ "Nix los über 60313"
  │
  ▼
Format: "✈ 3 aircraft near 60313: RJA33 AMM→MST FL320 | ..."
  │
  ▼
say_in_reply_to()
```

### Output Format

```
✈ {count} aircraft near {plz}: {entry1} | {entry2} | ...
```

Each entry: `{callsign} {origin_iata}→{dest_iata} FL{alt/100}`

- Altitude displayed as flight level (alt_baro / 100, rounded). Aircraft on the ground or below FL010 show alt in feet.
- If only 1 aircraft: `✈ 1 aircraft near {plz}: {entry}`

## Integration with main.rs

### Command Dispatch

In `handle_generic_commands()`, add a branch:

```rust
} else if first_word == "!up" {
    aviation::up_command(privmsg, client, &http, words.next(), &up_cooldowns).await?;
}
```

### Shared reqwest Client

Reuse a plain `reqwest::Client` (no auth headers) created in the generic command handler init. Both adsb.lol and adsbdb are keyless.

### Cooldown State

`Arc<Mutex<HashMap<String, Instant>>>` created in `run_generic_command_handler`, same pattern as `ai_cooldowns`.

## Files Changed/Created

| File | Action |
|------|--------|
| `src/aviation.rs` | **New** — PLZ lookup, API clients, command logic |
| `src/main.rs` | **Modified** — add `mod aviation`, dispatch `!up`, create shared HTTP client and cooldowns |
| `data/plz.csv` | **New** — embedded PLZ→coordinate mapping |

## Dependencies

No new Cargo.toml dependencies. Uses existing:
- `reqwest` (HTTP + JSON)
- `serde` (deserialization)
- `eyre` (error handling)
- `tokio` (async, JoinSet)
- `tracing` (logging)

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| adsb.lol goes down or rate-limits | 10s timeout, error response to user, handler continues |
| adsbdb has no route for a callsign | Treated as `None`, aircraft skipped silently |
| adsbdb slow for many lookups | Concurrent requests (JoinSet), cap candidates at 10 |
| PLZ data becomes outdated | German PLZs change very rarely; update CSV as needed |
| adsb.lol requires API key in future | Log warning, feature degrades gracefully |
| Response exceeds Twitch 500-char limit | Cap at 5 aircraft; format is ~40 chars per entry, well within limit |
