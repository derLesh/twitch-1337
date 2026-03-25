# Design: Cone Visibility Filter for !up Command

**Date**: 2026-03-25
**Status**: Draft

## Overview

Replace the fixed-radius aircraft filter in `!up` with a cone-shaped visibility filter. Aircraft at higher altitudes are visible from further away; low-altitude aircraft must be much closer to be "overhead." This filters out aircraft that are technically within 15 NM but flying low at a distant airport approach, for example.

## Problem

The current 15 NM fixed search radius treats all aircraft equally. A plane at FL350 (35,000 ft) 14 NM away is clearly visible overhead, but a plane at 975 ft 14 NM away is near the horizon — not "over" you. The fixed radius produces results like low-altitude aircraft on distant approach paths that aren't meaningfully overhead.

## Approach

Keep the 15 NM search radius to adsb.lol unchanged (we still need to fetch candidates). Post-filter the results using a linear cone where the maximum visible distance scales proportionally with altitude.

**Formula**: `max_distance_nm = altitude_ft * UP_SEARCH_RADIUS_NM / UP_CONE_REFERENCE_ALT_FT`

Calibrated so FL350 (35,000 ft) gets the full 15 NM radius:

| Altitude | Max visible distance |
|----------|---------------------|
| FL350 (35,000 ft) | 15.0 NM |
| FL200 (20,000 ft) | 8.6 NM |
| FL100 (10,000 ft) | 4.3 NM |
| FL59 (5,900 ft) | 2.5 NM |
| 1,000 ft | 0.4 NM |
| GND | excluded |

## Data Changes

Add `lat` and `lon` to `NearbyAircraft` deserialization:

```rust
#[derive(Debug, Deserialize)]
struct NearbyAircraft {
    flight: Option<String>,
    t: Option<String>,
    alt_baro: Option<AltBaro>,
    lat: Option<f64>,   // new
    lon: Option<f64>,   // new
}
```

Both fields are `Option<f64>` since adsb.lol may omit them for some aircraft.

New constant:

```rust
const UP_CONE_REFERENCE_ALT_FT: f64 = 35_000.0;
```

## Filtering Logic

### Haversine distance

A small pure function computing great-circle distance in nautical miles:

```rust
fn haversine_distance_nm(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64
```

Uses the standard haversine formula with Earth radius in NM (3440.065). No external crate needed (~10 lines).

### Cone filter

```rust
fn is_within_cone(
    aircraft_lat: f64,
    aircraft_lon: f64,
    aircraft_alt: &AltBaro,
    center_lat: f64,
    center_lon: f64,
) -> bool
```

Logic:
1. `AltBaro::Ground` → return false (ground aircraft excluded)
2. `AltBaro::Feet(ft)` where `ft <= 0` → return false
3. Compute distance via `haversine_distance_nm`
4. Compute `max_distance = ft as f64 * UP_SEARCH_RADIUS_NM as f64 / UP_CONE_REFERENCE_ALT_FT`
5. Return `distance <= max_distance`

### Exclusion rules

Aircraft are excluded if any of:
- `alt_baro` is `None` (unknown altitude — can't compute cone)
- `alt_baro` is `Ground`
- `lat` or `lon` is `None` (unknown position — can't compute distance)
- Distance exceeds cone limit for their altitude

### Pipeline integration

The cone filter is inserted into the existing candidates pipeline in `up_command`, before the callsign filter:

```rust
let candidates: Vec<_> = aircraft
    .iter()
    .filter(|ac| {
        let (Some(ac_lat), Some(ac_lon), Some(alt)) = (&ac.lat, &ac.lon, &ac.alt_baro) else {
            return false;
        };
        is_within_cone(*ac_lat, *ac_lon, alt, *lat, *lon)
    })
    .filter_map(|ac| {
        let callsign = ac.flight.as_ref()?.trim();
        if callsign.is_empty() { return None; }
        Some((callsign.to_string(), ac))
    })
    .take(UP_MAX_CANDIDATES)
    .collect();
```

## Scope of Changes

### Modified: `src/main.rs` (aviation module)
- Add `lat: Option<f64>`, `lon: Option<f64>` to `NearbyAircraft`
- Add `UP_CONE_REFERENCE_ALT_FT` constant
- Add `haversine_distance_nm()` function
- Add `is_within_cone()` function
- Insert cone filter into candidates pipeline in `up_command`

### Unchanged
- adsb.lol API call (same endpoint, same 15 NM search radius)
- adsbdb calls, Nominatim, resolver chain
- Output format, cooldowns, error handling
- All other handlers and modules
