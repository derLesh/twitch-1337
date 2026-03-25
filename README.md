# twitch-1337

A Rust-based Twitch IRC bot for the channel [REDACTED_CHANNEL](https://twitch.tv/REDACTED_CHANNEL). Maintains a persistent IRC connection with broadcast-based message routing to multiple independent handlers.

## Features

### 1337 Tracker

Monitors for messages containing "1337" or "DANKIES" sent at exactly 13:37 Berlin time. Tracks unique users with sub-second timing and maintains an all-time leaderboard of fastest times.

```
13:36:00  Handler wakes, creates fresh state
13:36:30  Posts "PausersHype" reminder
13:37:00  Monitoring begins (tracks unique users + millisecond timing)
13:37:59  Monitoring ends
13:38:00  Posts stats with contextual German response
```

Known bots ("supibot", "potatbotat") are filtered out. The leaderboard is persisted to `data/leaderboard.ron`.

### !toggle-ping / !list-pings

Manages user @mentions in StreamElements ping commands.

- `!toggle-ping <command>` - Adds or removes your @mention from a ping command
- `!list-pings [enabled|disabled|all]` - Lists ping commands you're subscribed to

Supported commands: ackern, amra, arbeitszeitbetrug, dayz, dbd, deadlock, eft, euv, fetentiere, front, hoi, kluft, kreuzzug, ron, ttt, vicky.

### !up \<location\>

Shows aircraft currently flying overhead using live ADS-B data from [adsb.lol](https://www.adsb.lol/) with route information from [adsbdb](https://www.adsbdb.com/).

**Location input** (resolved in order):
1. German postal code (5 digits) - embedded CSV lookup
2. ICAO airport code (4 letters) - embedded CSV lookup
3. IATA airport code (3 letters) - embedded CSV lookup
4. Free text place name - OpenStreetMap Nominatim geocoding

**Example output:**
```
✈ 3 Flieger über Frankfurt am Main Airport: DLH456 (A321) TXL→CDG FL350 3.2nm ↘ | ...
```

#### Cone Visibility Filter

Instead of a fixed search radius, the `!up` command uses a cone-shaped visibility filter. Aircraft at higher altitudes are visible from further away, while low-altitude aircraft must be nearby. Ground-level aircraft are excluded entirely.

```
(Altitude (×1000 ft)) ^
        35 |
        30 | ⡇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣀⣠⠤⠴⠒⠒⠉⠉⠁
        25 | ⡇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣀⡤⠤⠖⠒⠊⠉⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀
        20 | ⡇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣀⣠⠤⠴⠒⠒⠉⠉⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
        15 | ⡇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣀⣀⠤⠤⠖⠒⠋⠉⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
        10 | ⡇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣀⡠⠤⠔⠒⠚⠉⠉⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
         5 | ⡇⠀⠀⠀⠀⠀⠀⢀⣀⣀⠤⠤⠒⠒⠋⠉⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
         0 | ⣇⣤⣤⣔⣒⣚⣉⣉⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀
-----------|-|---------|---------|---------|---------|---------|---------|-> (Distance (NM))
           | 0         2.5       5         7.5       10        12.5     15
```

Aircraft below the line are considered "overhead." Aircraft above the line are too far away for their altitude. Regenerate with `uv run scripts/cone_chart.py`.

The maximum visible distance scales linearly with altitude:

```
max_distance_nm = altitude_ft * 15 / 35,000
```

| Altitude | Max Distance |
|----------|-------------|
| FL350 (35,000 ft) | 15.0 NM |
| FL200 (20,000 ft) | 8.6 NM |
| FL100 (10,000 ft) | 4.3 NM |
| 5,900 ft | 2.5 NM |
| 1,000 ft | 0.4 NM |
| Ground | excluded |

The initial search fetches all aircraft within 15 NM from adsb.lol, then the cone filter narrows results based on each aircraft's actual altitude and distance.

### !fl \<aircraft\> \<duration\>

Generates random flight plans using SimBrief.

- Aircraft: ICAO type code (e.g., A20N, B738, C172)
- Duration: compact format (e.g., 1h, 30m, 2h30m)

**Example output:**
```
EDDF -> EGLL | 280 nm | 1h12m | FL360 | https://dispatch.simbrief.com/...
```

### !ai \<instruction\>

AI-powered responses via OpenRouter API. 30-second cooldown per user. Responses are kept brief (2-3 sentences) and match the language of the instruction. Requires optional OpenRouter configuration in config.toml.

### Scheduled Messages

Posts messages at configured intervals with optional date ranges and daily time windows. Schedules are defined in `config.toml` and hot-reload when the file changes (2-second debounce).

```toml
[[schedules]]
name = "hydration"
message = "Stay hydrated! DinkDonk"
interval = "00:30"
active_time_start = "18:00"
active_time_end = "23:00"
enabled = true
```

### Latency Monitor

Measures IRC round-trip latency via PING/PONG every 5 minutes. Maintains an exponential moving average (alpha=0.2) used by timing-sensitive handlers like the 1337 tracker to offset sleep timers.

## Quick Start

```bash
cp config.toml.example config.toml
# Edit config.toml with your credentials
cargo run
```

Configuration is entirely via `config.toml` - see `config.toml.example` for all available options.

### Docker

```bash
just build     # Build podman image
just deploy    # Build, push to remote host, and restart
just logs      # Tail logs on remote host
```

## Building

```bash
cargo build                                              # Debug build
cargo build --release                                    # Release build (glibc)
cargo build --release --target x86_64-unknown-linux-musl # Static build (no deps)
```

The Docker image uses a multi-stage build with [cargo-chef](https://github.com/LukeMathWalker/cargo-chef) for layer caching and a `FROM scratch` final image (~6 MB).

## Development

```bash
cargo check    # Type-check without building
cargo clippy   # Lint
cargo test     # Run tests
cargo fmt      # Format code
```

Set `RUST_LOG` to control log verbosity: `trace`, `debug`, `info` (default), `warn`, `error`.

## Architecture

Single persistent IRC connection with a broadcast channel (capacity: 100) distributing messages to independent handler tasks:

- **Message Router** - reads from twitch-irc, broadcasts to all handlers
- **1337 Handler** - daily 13:36-13:38 monitoring cycle
- **Generic Command Handler** - dispatches !toggle-ping, !list-pings, !up, !fl, !ai
- **Latency Monitor** - PING/PONG every 5 minutes
- **Config Watcher** - watches config.toml for schedule changes
- **Scheduled Message Handler** - spawns/stops dynamic message tasks

All handlers run independently in `tokio::select!` for coordinated shutdown. Errors in one handler don't affect others.

## Credits

Created for the Twitch channel [REDACTED_CHANNEL](https://twitch.tv/REDACTED_CHANNEL).
