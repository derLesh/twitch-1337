# Flight Tracker Design

**Date:** 2026-04-06
**Status:** Draft

## Overview

A real-time flight tracking feature for the Twitch bot. Users can track flights by callsign or ICAO24 hex code. The bot polls adsb.lol, detects flight phase changes and notable events, and posts updates to chat proactively. Users can also query flight status on demand.

## Data Source

**Primary:** adsb.lol v2 API (already integrated via `AviationClient`).

- `/v2/hex/{hex}` — query by ICAO24 hex code
- `/v2/callsign/{callsign}` — query by callsign

**Enrichment:** adsbdb for route info (origin/destination airports) — already used by `!up` command.

**Key data fields from adsb.lol v2 response:**
- `hex`, `flight` (callsign), `t` (ICAO aircraft type), `r` (registration)
- `lat`, `lon`, `alt_baro` (feet or "ground" string), `alt_geom`
- `gs` (ground speed, knots)
- `baro_rate`, `geom_rate` (vertical rate, ft/min)
- `squawk`
- `nav_modes` (array: autopilot, vnav, althold, approach, lnav, tcas)
- `seen`, `seen_pos` (seconds since last message/position)

## Architecture

**Approach:** Single handler task with command channel (mpsc).

One dedicated `run_flight_tracker()` handler task owns all polling, state, and persistence. Chat commands send instructions via an `mpsc::channel<TrackerCommand>`. The handler processes commands between poll cycles. State changes trigger proactive chat messages. State is persisted to a RON file after each change.

```
┌─────────────────┐     mpsc::channel      ┌──────────────────────┐
│ Command Handler  │ ──── TrackerCommand ──► │ Flight Tracker Task  │
│ (!track, !flight)│                         │                      │
└─────────────────┘                         │  - polls adsb.lol    │
                                            │  - detects phases    │
                                            │  - posts to chat     │
                                            │  - persists to RON   │
                                            └──────────────────────┘
```

**Why this approach:**
- Single owner of all state — no shared mutexes
- Clean message-passing pattern
- File writes serialized naturally (one writer)
- Adaptive polling straightforward since one loop controls timing

## Data Model

### FlightIdentifier

Either a callsign (e.g., "DLH123") or ICAO24 hex code (e.g., "3C6752").

**Detection heuristic:** 6-character string where all characters are hex digits = hex code. Otherwise = callsign.

### FlightPhase

States: `Ground`, `Takeoff`, `Climb`, `Cruise`, `Descent`, `Approach`, `Landing`, `Unknown`.

`Landing` is transient — posted as an event, then transitions to `Ground`.

### TrackedFlight

Per-flight state:

| Field | Type | Purpose |
|-------|------|---------|
| `identifier` | `FlightIdentifier` | Original tracking identifier |
| `callsign` | `Option<String>` | Resolved callsign (if tracked by hex) |
| `hex` | `Option<String>` | Resolved hex (if tracked by callsign) |
| `phase` | `FlightPhase` | Current detected phase |
| `route` | `Option<(String, String)>` | (origin IATA, dest IATA) from adsbdb |
| `aircraft_type` | `Option<String>` | ICAO type code (e.g., "A320") |
| `altitude_ft` | `Option<i64>` | Last known barometric altitude |
| `vertical_rate_fpm` | `Option<i64>` | Last known vertical rate |
| `ground_speed_kts` | `Option<f64>` | Last known ground speed |
| `lat` | `Option<f64>` | Last known latitude |
| `lon` | `Option<f64>` | Last known longitude |
| `squawk` | `Option<String>` | Last known squawk code |
| `tracked_by` | `String` | Twitch username who requested tracking |
| `tracked_at` | `DateTime<Utc>` | When tracking started |
| `last_seen` | `Option<DateTime<Utc>>` | Last successful poll with data |
| `last_phase_change` | `Option<DateTime<Utc>>` | When phase last changed |
| `polls_since_change` | `u32` | Polls since last phase change (for adaptive timing) |
| `scheduled_departure_at` | `Option<DateTime<Utc>>` | Scheduled departure from one-time Aviationstack metadata |
| `last_adsb_poll_at` | `Option<DateTime<Utc>>` | Last ADS-B lookup attempt, including empty/error responses |

### FlightTrackerState

Persisted to `flights.ron` in the data directory. Contains `Vec<TrackedFlight>`.

## Phase Detection

Thresholds applied in order each poll cycle:

| Phase | Condition |
|-------|-----------|
| **Ground** | `alt_baro == "ground"` OR (altitude < 200ft AND ground_speed < 30kts) |
| **Takeoff** | Was Ground, now altitude rising AND ground_speed > 60kts |
| **Climb** | Airborne, `baro_rate > 500 ft/min` |
| **Cruise** | Airborne, `\|baro_rate\| < 300 ft/min`, altitude > 10,000ft, stable for 2+ polls |
| **Descent** | Airborne, `baro_rate < -500 ft/min` |
| **Approach** | Descending, altitude < 10,000ft OR nav_modes contains "approach" |
| **Landing** | Was airborne, now Ground |

**Tracking lost:** No data received for 5+ minutes. After 30 minutes of "tracking lost", the flight is automatically removed.

## Event Detection

### Squawk Changes

Monitor for emergency squawk codes:
- **7500** — Hijack
- **7600** — Radio failure
- **7700** — General emergency

Trigger: squawk changes to one of these values (compared against previous poll).

### Divert Detection

On first successful poll, cache the route destination airport coordinates from adsbdb. In subsequent polls during Descent or Approach phase: if the aircraft's bearing relative to the destination diverges > 90 degrees for 3+ consecutive polls, flag as potential divert. Divert detection is limited to Descent/Approach since en-route heading changes are normal (airways, ATC vectors).

## Chat Commands

### `!track <callsign|hex>`

Start tracking a flight.

**Validation:**
- Per-user limit: 3 tracked flights
- Global limit: 12 tracked flights
- Reject duplicates

**On success:**
- Query adsb.lol to verify flight exists
- Fetch route from adsbdb for enrichment
- Persist to flights.ron
- Reply: `Tracke DLH123 (A320) FRA→MUC Okayge`

### `!untrack <callsign|hex>`

Stop tracking a flight.

**Permissions:** Only the user who started tracking OR moderators can untrack.

**On success:**
- Remove from state, persist
- Reply: `DLH123 wird nicht mehr getrackt Okayge`

### `!flights`

List all currently tracked flights with current phase.

**Response:** `Getrackte Flüge: DLH123 (Cruise FL350) | RYR456 (Climb 12000ft) | ...`

### `!flight <callsign|hex>`

Status of a specific tracked flight.

**Response:** `DLH123 (A320) FRA→MUC | Cruise FL350 | 452kts | Squawk 1000 | seit 1h23m getrackt`

Fields shown: callsign, type, route, phase, altitude (as FL or feet), ground speed, squawk, tracking duration. Missing fields omitted gracefully.

## Chat Messages (Mixed German/English)

| Event | Message Format |
|-------|---------------|
| Track started | `Tracke DLH123 (A320) FRA→MUC Okayge` |
| Takeoff | `DLH123 (A320) FRA→MUC ist gestartet! ✈` |
| Cruise reached | `DLH123 (A320) FRA→MUC cruist auf FL350` |
| Descent started | `DLH123 (A320) FRA→MUC hat Descent eingeleitet` |
| Approach | `DLH123 (A320) FRA→MUC ist im Approach` |
| Landing | `DLH123 (A320) FRA→MUC ist gelandet! Flugzeit: 1h23m` |
| Squawk emergency | `⚠ DLH123 (A320) FRA→MUC squawkt {code}! ({meaning})` |
| Possible divert | `⚠ DLH123 (A320) FRA→MUC scheint zu diverten!` |
| Tracking lost | `DLH123 Signal verloren, wird nicht mehr getrackt` |
| Already tracked | `DLH123 wird schon getrackt FDM` |
| User limit | `Du trackst schon 3 Flüge FDM` |
| Global limit | `Maximal 12 Flüge gleichzeitig FDM` |
| Not found | `Den Flieger finde ich nicht FDM` |
| Permission denied | `Das darfst du nicht FDM` |
| Untracked | `DLH123 wird nicht mehr getrackt Okayge` |

Messages degrade gracefully — if route or aircraft type is unknown, those parts are omitted.

## Adaptive Polling

The handler computes per-flight due times and sleeps until the next due poll.
Live flights keep phase-based intervals:

| Condition | Interval |
|-----------|----------|
| Flight has `polls_since_change < 5` OR phase is Takeoff/Approach/Landing | **30s** |
| Flight is in Climb or Descent | **60s** |
| Flight is in Cruise or Ground | **120s** |

Flights accepted from metadata before ADS-B visibility are pending (`last_seen == None`) and use a lower-call schedule:

| Pending condition | Interval |
|-------------------|----------|
| Scheduled departure > 6h away | **30m** |
| Scheduled departure 6h..90m away | **15m** |
| 90m before to 45m after scheduled departure | **2m** |
| 45m..3h after scheduled departure | **5m** |
| 3h..12h after scheduled departure | **15m** |
| No scheduled departure, tracked < 10m ago | **2m** |
| No scheduled departure, tracked 10m..6h ago | **10m** |
| No scheduled departure, tracked 6h..24h ago | **30m** |

Pending flights expire without another ADS-B call after 12h past scheduled
departure, or after 24h when no scheduled departure is known.

If the scheduled departure was already expired when tracking started, treat it as stale metadata and use the no-scheduled-departure pending schedule instead.

## Persistence

**File:** `flights.ron` in the data directory (alongside `leaderboard.ron`, `feedback.txt`).

**Format:** RON (consistent with existing non-admin persistence patterns).

**Write triggers:** After any state change (flight added/removed, phase changed, data updated).

**Startup:** Load from file if it exists, otherwise start empty. Graceful fallback on parse errors (log warning, start empty).

**Idle behavior:** When no flights are tracked, the handler blocks on the mpsc receiver (no polling, no busy-waiting). It resumes polling when a `Track` command arrives.

## Integration

### New Files

| File | Purpose |
|------|---------|
| `src/flight_tracker.rs` | Handler, polling loop, phase detection, persistence, state types |
| `src/commands/track.rs` | `!track` command |
| `src/commands/untrack.rs` | `!untrack` command |
| `src/commands/flights.rs` | `!flights` and `!flight` status commands |

### Modified Files

| File | Change |
|------|--------|
| `src/main.rs` | Create mpsc channel, spawn tracker handler, pass sender to command handler, add to select! |
| `src/commands/mod.rs` | Register new commands |
| `src/aviation.rs` | Add `get_aircraft_by_hex()` and `get_aircraft_by_callsign()` methods, make adsbdb route lookup reusable |

### Wiring

```
main():
    let (tracker_tx, tracker_rx) = mpsc::channel(32);
    
    spawn run_flight_tracker(tracker_rx, client, channel, aviation_client)
    
    // tracker_tx.clone() passed to Track/Untrack/Flights command constructors
    
    add tracker_handle to tokio::select! for shutdown
```

### Reuse from Existing Code

- `AviationClient` — HTTP client for adsb.lol (add hex/callsign query methods)
- `AltBaro` enum — already handles "ground" string parsing
- adsbdb route lookup — already implemented for `!up`
- `get_data_dir()` — data directory resolution
- Error handling patterns, tracing, reply patterns from existing commands

## Limits & Constants

| Constant | Value |
|----------|-------|
| `MAX_TRACKED_FLIGHTS` | 12 |
| `MAX_FLIGHTS_PER_USER` | 3 |
| `POLL_FAST` | 30s |
| `POLL_NORMAL` | 60s |
| `POLL_SLOW` | 120s |
| `POLL_TIMEOUT` | 10s |
| `TRACKING_LOST_THRESHOLD` | 5 min |
| `TRACKING_LOST_REMOVAL` | 30 min |
| `DIVERT_BEARING_THRESHOLD` | 90 degrees |
| `DIVERT_CONSECUTIVE_POLLS` | 3 |
| `CRUISE_STABLE_POLLS` | 2 |
| `CLIMB_RATE_THRESHOLD` | 500 ft/min |
| `DESCENT_RATE_THRESHOLD` | -500 ft/min |
| `CRUISE_RATE_THRESHOLD` | 300 ft/min |
| `CRUISE_MIN_ALTITUDE` | 10,000 ft |
| `APPROACH_MAX_ALTITUDE` | 10,000 ft |
| `GROUND_MAX_ALTITUDE` | 200 ft |
| `GROUND_MAX_SPEED` | 30 kts |
| `TAKEOFF_MIN_SPEED` | 60 kts |
| `MPSC_CHANNEL_CAPACITY` | 32 |
