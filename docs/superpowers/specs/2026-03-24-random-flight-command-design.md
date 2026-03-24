# Random Flight Command (`!fl`) Design

## Summary

Add a `!fl <ICAO_TYPE> <DURATION>` command to the Twitch bot that generates a random flight plan using the `random-flight` crate and replies with a compact one-liner.

## Command Format

```
!fl A20N 1h
!fl B738 2h30m
```

## Response Format

```
EDDF → EGLL | 280 nm | 1h12m | FL360 | https://dispatch.simbrief.com/...
```

Fields: departure ICAO → arrival ICAO | distance in nm | block time | cruise flight level | SimBrief dispatch URL.

## Error Responses

| Condition | Response |
|-----------|----------|
| Missing arguments | `Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM` |
| Unknown aircraft | `Das Flugzeug kenn ich nich FDM` |
| No route found | `Hab keine Route gefunden, versuch mal ne andere Zeit FDM` |

## Integration

### Dependency

Add `random-flight` as a path dependency and `humantime` for duration parsing:

```toml
random-flight = { path = "../random-flight" }
humantime = "2"
```

### Command Dispatch

Add `!fl` branch in `handle_generic_commands()` alongside existing commands.

### Handler Function

New `flight_command()` async fn:

1. Parse aircraft ICAO from first argument via `aircraft_by_icao_type()`
2. Parse duration from second argument via `humantime::parse_duration()`
3. Call `generate_flight_plan()` wrapped in `tokio::task::spawn_blocking` (sync + potentially slow with retries)
4. Format response: `"{dep} → {arr} | {dist:.0} nm | {time} | FL{alt} | {simbrief_url}"`
5. Reply in chat

### No Config Changes

The command is self-contained with no external API calls or configuration needed.
