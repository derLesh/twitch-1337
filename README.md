# twitch-1337

[![CI](https://github.com/Chronophylos/twitch-1337/actions/workflows/ci.yml/badge.svg)](https://github.com/Chronophylos/twitch-1337/actions/workflows/ci.yml)
[![Docker](https://github.com/Chronophylos/twitch-1337/actions/workflows/docker.yml/badge.svg)](https://github.com/Chronophylos/twitch-1337/actions/workflows/docker.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust Edition 2024](https://img.shields.io/badge/rust-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)

A Rust-based Twitch IRC bot:

- **1337 tracker** -- logs users who say "1337" or "DANKIES" at exactly 13:37 Berlin time, with a sub-second leaderboard of fastest times
- **Community pings** -- template-based ping commands with membership, cooldowns, and self-service join/leave
- **Chat commands** -- AI responses (`!ai`), live overhead aircraft (`!up`), random flight plans (`!fl`), flight tracking (`!track`), user feedback (`!fb`), scheduled messages

Single persistent IRC connection with broadcast-based message routing to independent handler tasks.

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

- `!lb` -- shows the all-time fastest 1337

### Ping System

Community ping commands with admin management and user self-service.

- `!p create <name> <template>` / `!p edit <name> <template>` / `!p delete <name>` -- admin: manage pings
- `!p add <name> <user>` / `!p remove <name> <user>` -- admin: manage membership
- `!p join <name>` / `!p leave <name>` / `!p list` -- user: self-service
- `!<name>` -- trigger a ping (mentions all members except the sender)

Templates support `{mentions}` (space-separated @-mentions, sender excluded) and `{sender}`. Triggers are rate-limited via `cooldown` in `[pings]`; set `public = true` to let non-members trigger. State is persisted to `data/pings.ron`.

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

<details>
<summary><strong>Cone visibility filter</strong> -- how <code>!up</code> decides which aircraft count as "overhead"</summary>

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

</details>

### !fl \<aircraft\> \<duration\>

Generates random flight plans using SimBrief.

- Aircraft: ICAO type code (e.g., A20N, B738, C172)
- Duration: compact format (e.g., 1h, 30m, 2h30m)

**Example output:**
```
EDDF -> EGLL | 280 nm | 1h12m | FL360 | https://dispatch.simbrief.com/...
```

### Flight Tracker

Tracks specific aircraft over time and posts status updates (takeoff, cruise, descent, landing, divert, emergency squawks) as the flight progresses. Backed by adsb.lol polling with adaptive poll rates (30s / 60s / 120s depending on phase).

- `!track <callsign|hex>` -- start tracking a flight (6-char hex is treated as ICAO24, anything else as callsign)
- `!untrack <callsign|hex>` -- stop tracking (own flights, or any flight if mod/broadcaster)
- `!flights` -- list currently tracked flights
- `!flight <callsign|hex>` -- show status of one tracked flight

Limits: up to 12 flights total, 3 per user. Tracked flights are persisted to `data/flights.ron`.

### !fb \<message\>

User feedback. Appends the message to `data/feedback.txt` with a timestamp. Per-user cooldown configurable via `[cooldowns].feedback` (default 300s).

### !ai \<instruction\>

AI-powered responses via any OpenAI-compatible API (OpenRouter, OpenAI, etc.) or a local Ollama server. Responses are kept brief (2-3 sentences) and match the language of the instruction. Disabled unless `[ai]` is present in `config.toml`.

Optional behaviors:
- **Chat history access** (`history_length`) -- recent main-channel messages are kept locally and exposed to the model through the `get_recent_chat` tool only when needed.
- **Startup prefill** (`[ai.history_prefill]`) -- seeds the buffer from a rustlog-compatible log API so the bot has context right after restart.
- **Persistent memory** (`memory_enabled`) -- the model itself decides what to remember across conversations via tool calls; facts stored in `data/ai_memory.ron`.
- **Per-workflow reasoning effort** (`reasoning_effort`) -- optional hints for `!ai`, `[ai.extraction]`, and `[ai.consolidation]`; values are passed through verbatim because support differs by model/provider.
- **7TV emote grounding** (`[ai.emotes]`) -- loads the current channel + optional global 7TV catalog, intersects it with a manual glossary, and injects only known emotes into the prompt.

Per-user cooldown configurable via `[cooldowns].ai` (default 30s).

Example 7TV glossary (`data/7tv_emotes.toml` by default):

```toml
[[emotes]]
name = "KEKW"
meaning = "laughter; something is funny"
usage = "jokes, fail moments, or ironic chat reactions"
avoid = "serious topics"
```

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
- **Generic Command Handler** - dispatches `!p`, `!lb`, `!up`, `!fl`, `!ai`, `!fb`, `!track`, `!untrack`, `!flights`, `!flight`, and ping triggers
- **Flight Tracker** - polls adsb.lol for tracked aircraft and announces phase changes
- **Latency Monitor** - PING/PONG every 5 minutes
- **Config Watcher** - watches config.toml for schedule changes
- **Scheduled Message Handler** - spawns/stops dynamic message tasks

All handlers run independently in `tokio::select!` for coordinated shutdown. Errors in one handler don't affect others.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
