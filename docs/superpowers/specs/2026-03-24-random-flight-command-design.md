# Random Flight Command (`!fl`) Design

## Summary

Add a `!fl <ICAO_TYPE> <DURATION>` command to the Twitch bot that generates a random flight plan using the `random-flight` crate and replies with a compact one-liner.

## Command Format

```
!fl A20N 1h
!fl B738 2h30m
!fl C172 45m
```

Duration supports: `Nh`, `Nm`, `NhNm` (e.g. `1h`, `30m`, `2h30m`).

## Response Format

```
EDDF → EGLL | 280 nm | 1h12m | FL360 | https://dispatch.simbrief.com/...
```

Fields: departure ICAO → arrival ICAO | distance (rounded) | block time | cruise flight level (altitude / 100) | SimBrief dispatch URL.

Block time formatted as compact `NhNm` (e.g. `1h12m`, `45m`, `3h0m`).

Flight level: `cruise_altitude_ft / 100`, e.g. 36000 ft → `FL360`.

## Error Responses

| Condition | Response |
|-----------|----------|
| Missing arguments | `Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM` |
| Unknown aircraft | `Das Flugzeug kenn ich nich FDM` |
| Generation failure (all error variants) | `Hab keine Route gefunden, versuch mal ne andere Zeit FDM` |

All `random_flight::Error` variants (`NoValidAirports`, `NoCandidateArrivals`, `RetriesExhausted`, `RangeExceeded`, `RunwayTooShort`) map to the same user-facing message. `UnknownAirport` cannot occur since we don't pin departure/arrival.

## Integration

### Dependency

Add `random-flight` as a path dependency:

```toml
random-flight = { path = "../random-flight" }
```

No `humantime` dependency needed — use a small custom duration parser (same style as the existing `parse_interval` in the codebase) that handles `1h`, `30m`, `2h30m` formats and returns `std::time::Duration`.

### Command Dispatch

Add `!fl` branch in `handle_generic_commands()` alongside existing commands.

### Handler Function

New `flight_command()` async fn:

1. Parse aircraft ICAO from first argument via `aircraft_by_icao_type()` — returns `Option<&'static Aircraft>`
2. Parse duration from second argument via custom `parse_flight_duration()` helper
3. Clone the `Aircraft` and move into `tokio::task::spawn_blocking` closure (the `&'static` ref from `aircraft_by_icao_type` can be moved directly, but clone is clearer)
4. Call `generate_flight_plan(&aircraft, duration, None)` inside the blocking task
5. Format response: `"{dep} → {arr} | {dist:.0} nm | {time} | FL{alt} | {url}"`
   - `alt` = `fp.cruise_altitude_ft / 100`
   - `time` = custom compact format (`NhNm`)
6. Reply in chat

### No Config Changes

The command is self-contained with no external API calls or configuration needed.

### Docker Build Note

The `random-flight` crate's `build.rs` downloads airport data from OurAirports at compile time (falls back to bundled `data/airports.csv`). This should work fine with the existing multi-stage Docker build since the builder stage has network access.
