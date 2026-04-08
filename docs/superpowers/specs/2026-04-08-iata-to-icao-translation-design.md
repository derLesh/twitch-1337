# IATA-to-ICAO Flight Number Translation

**Date**: 2026-04-08
**Issue**: #1 — Failure to translate flight number
**Status**: Design approved

## Problem

When a user types `!track TP247`, the bot queries adsb.lol for callsign `TP247` verbatim. ADS-B uses ICAO airline codes, so the actual callsign is `TAP247` (TAP Air Portugal). The bot needs to translate IATA airline codes to ICAO before querying.

## Scope

- IATA-to-ICAO airline code translation (e.g., `TP` → `TAP`)
- Static embedded CSV as primary lookup, adsbdb API as fallback
- **Out of scope**: Codeshare resolution (deferred to future issue)

## Design

### Data: Embedded airline CSV

New file `data/airlines.csv` with IATA-to-ICAO mappings sourced from the OPTD or iata-utils dataset.

Format:
```
TP,TAP
LH,DLH
BA,BAW
```

Loaded via `include_str!("../data/airlines.csv")` + `OnceLock<HashMap<&'static str, &'static str>>` — same pattern as `airports.csv` and `plz.csv`. Keyed by IATA code, value is ICAO code.

### Detection: IATA flight number pattern

A function detects whether user input looks like an IATA flight number:

- **IATA pattern**: 2-letter prefix + 1-4 digits (e.g., `TP247`, `LH5765`, `BA12`)
- **ICAO pattern**: 3-letter prefix + 1-4 digits (e.g., `TAP247`, `DLH5765`, `BAW12`)

Heuristic: if input matches `^[A-Z]{2}\d{1,4}$`, treat as potential IATA flight number. Only translate when the 2-letter prefix exists in the airline mapping data. Otherwise pass through unchanged.

### Resolution: `AviationClient::resolve_callsign()`

New async method on `AviationClient`:

```
resolve_callsign(input: &str) -> String
```

Steps:
1. Check if input matches IATA pattern (`^[A-Z]{2}\d{1,4}$`)
2. If no match — return input unchanged (already ICAO or unknown format)
3. Split into airline code (`TP`) and flight number (`247`)
4. Look up airline code in embedded CSV HashMap
5. If found — return `"{icao_code}{flight_number}"` (e.g., `TAP247`)
6. If not found — call adsbdb API (`GET /v0/airline/{iata_code}`)
7. If API returns a result — return `"{icao_code}{flight_number}"`, log the miss for future CSV updates
8. If API also fails — return input unchanged, log a warning

### Integration point

In `flight_tracker.rs`, where `FlightIdentifier::Callsign(cs)` is matched for querying, add the translation call before the adsb.lol lookup:

```rust
FlightIdentifier::Callsign(cs) => {
    let resolved = aviation_client.resolve_callsign(&cs).await;
    tokio::time::timeout(POLL_TIMEOUT, aviation_client.get_aircraft_by_callsign(&resolved)).await
}
```

No changes to `FlightIdentifier::parse()`, `get_aircraft_by_callsign()`, or any other existing code.

## Files changed

- **New**: `data/airlines.csv` — IATA-to-ICAO airline code mappings
- **Modified**: `src/aviation.rs` — CSV loading, `resolve_callsign()` method, adsbdb airline API call
- **Modified**: `src/flight_tracker.rs` — call `resolve_callsign()` before the API query
