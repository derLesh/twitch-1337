# Automatic Latency Monitor

## Problem

The bot uses a static `expected_latency` config value (in milliseconds) to offset sleep timers so that messages (e.g., the 13:36:30 reminder, 13:38:00 stats) arrive at the intended time. This value must be manually tuned and doesn't adapt to changing network conditions.

## Solution

A new handler task that periodically measures IRC round-trip latency via PING/PONG, computes an exponential moving average (EMA), and exposes the live estimate to timing-sensitive code. The static config value becomes an optional initial seed.

## Design

### Latency Handler Task

New `run_latency_handler()` async function following the existing handler pattern:

- **Inputs:** `client: Arc<AuthenticatedTwitchClient>`, `broadcast_tx: broadcast::Sender<ServerMessage>`, `initial_latency: u32` (from config, default 100)
- **Shared state:** `Arc<AtomicU32>` initialized with the config seed, created before spawning and passed to consumers
- **Loop (every 5 minutes):**
  1. Generate a unique nonce (timestamp-based string)
  2. Record `Instant::now()`
  3. Send `PING <nonce>` via `client.send_message(irc!["PING", nonce])`
  4. Listen on broadcast for `ServerMessage::Pong` with a 10s timeout. Extract nonce from `pong.source.params.get(1)` — the PONG for `PING test` arrives as `:tmi.twitch.tv PONG tmi.twitch.tv :test`, so the nonce is in `params[1]`. Match against our nonce; ignore mismatches (e.g., from the library's internal keepalive which uses `tmi.twitch.tv` as its argument).
  5. Compute one-way latency as RTT / 2
  6. Update EMA and store in `AtomicU32`

### EMA Calculation

- **Formula:** `ema = alpha * sample + (1.0 - alpha) * ema`
- **Alpha:** 0.2 (hardcoded constant)
- **Initial value:** `config.twitch.expected_latency` or 100 if not set
- **Input:** One-way latency (RTT / 2)
- **Storage:** `Arc<AtomicU32>` with `Relaxed` ordering — stores EMA in milliseconds as a whole number

### Integration with `sleep_until_hms`

- `sleep_until_hms` signature stays `expected_latency: u32` — no change
- `run_1337_handler` receives `Arc<AtomicU32>` instead of `u32`, reads the current EMA at each call site via `latency.load(Relaxed)`, and passes the `u32` to `sleep_until_hms`
- This keeps `sleep_until_hms` simple, testable, and avoids `#[instrument]` issues with `Arc<AtomicU32>` (which prints as `UnsafeCell { .. }` in traces)

### Logging & Observability

- Each measurement logged at `debug`: raw RTT, one-way estimate, current EMA
- EMA update logged at `info` only when value changes by >= 10ms from last info-logged value
- PING timeout (no PONG within 10s) logged at `warn` — EMA not updated
- Handler startup logged at `info` with initial seed value
- Nonce mismatch silently ignored — the library's internal keepalive sends `PING tmi.twitch.tv` which produces PONGs with `tmi.twitch.tv` in params[1], always distinguishable from our timestamp-based nonces

### Config Changes

- `expected_latency` field in `TwitchConfiguration` gets `#[serde(default = "default_expected_latency")]` where `fn default_expected_latency() -> u32 { 100 }`. Type stays `u32` — serde fills in the default when the field is absent from TOML.
- Validation updates: only check `> 1000` if the value differs from default (or just always validate — the default of 100 passes anyway)
- `config.toml.example` updated to note the value is the initial seed

### Error Handling

- PING send failure: log at `warn`, skip this cycle, retry next interval
- PONG timeout (10s): log at `warn`, EMA unchanged, retry next interval
- No crash or panic paths — handler is fully independent
- Added to `tokio::select!` in `main()` for graceful shutdown

### Constants

- `LATENCY_PING_INTERVAL`: 5 minutes
- `LATENCY_PING_TIMEOUT`: 10 seconds
- `LATENCY_EMA_ALPHA`: 0.2
- `LATENCY_LOG_THRESHOLD`: 10ms

## Files Changed

- **`src/main.rs`:**
  - New `run_latency_handler()` function
  - New constants: `LATENCY_PING_INTERVAL`, `LATENCY_PING_TIMEOUT`, `LATENCY_EMA_ALPHA`, `LATENCY_LOG_THRESHOLD`
  - `sleep_until_hms()` signature unchanged (still takes `u32`)
  - `run_1337_handler()` takes `Arc<AtomicU32>` instead of `u32`, reads EMA at each call site
  - `TwitchConfiguration.expected_latency` gets `#[serde(default = "default_expected_latency")]` (type stays `u32`, default 100)
  - `main()` spawns the new handler, passes shared `Arc<AtomicU32>` to both latency and 1337 handlers
  - Add `use std::sync::atomic::{AtomicU32, Ordering}` and `irc!` macro import
- **`config.toml.example`:** Update comment to note it's the initial seed value

## No New Dependencies

Uses only existing crate capabilities: `twitch-irc`'s `irc!` macro for PING, `ServerMessage::Pong` for responses, `std::sync::atomic::AtomicU32` for shared state.
