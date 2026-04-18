# CLAUDE.md

File guide Claude Code (claude.ai/code) working repo.

## Project Overview

Rust Twitch IRC bot, multiple features:
1. **1337 Tracker**: Monitor "1337"/"DANKIES" at 13:37 Berlin, post stats 13:38
2. **Leaderboard** (`!lb`): Persistent sub-1s personal-best board 1337 challenge, persist `leaderboard.ron`
3. **Ping System**: Local file-based ping commands. Admin mgmt, user self-service, dynamic trigger
4. **Scheduled Messages**: Dynamic scheduling via config.toml, file watch
5. **Latency Monitor**: IRC latency via PING/PONG every 5min, auto-adjust timing offsets
6. **AI Command** (`!ai`): OpenAI/Ollama chat, optional history + persistent memory
7. **Flight Tracker**: Long task, poll ADS-B APIs, post phase/squawk/divert updates (`!track`, `!untrack`, `!flight`, `!flights`)
8. **Aviation Lookups** (`!up`, `!fl`): Flights above PLZ/airport, random callsigns. Embedded CSVs `data/`
9. **Feedback** (`!fb`): Append user feedback `feedback.txt`

Persistent IRC connection, broadcast message routing to multiple handlers.

## Build and Run Commands

### Using Just (Recommended)

Justfile for deployment:

```bash
just build              # Build podman image as chronophylos/twitch-1337:latest
just build-no-cache     # Force full rebuild without cache
just push               # Push image to remote docker host via SSH
just restart            # Restart container on docker host via SSH
just deploy             # Build, push, and restart (full deployment)
```

### Using Cargo Directly

```bash
# Build the project
cargo build

# Run with default logging (info level)
cargo run

# Run with debug logging
RUST_LOG=debug cargo run

# Build optimized release version
cargo build --release

# Build static musl binary (no runtime dependencies)
cargo build --release --target x86_64-unknown-linux-musl

# Run release version
cargo run --release

# Check code without building
cargo check

# Format (CI checks this — run before every commit)
cargo fmt --all

# Verify formatting without writing (matches CI)
cargo fmt --all -- --check

# Lint with clippy — strict mode, matches CI
cargo clippy --all-targets -- -D warnings

# Run tests
cargo test
```

### Using Docker

```bash
# Build the image
docker build -t chronophylos/twitch-1337:latest .

# Run with config.toml mounted as a volume
docker run -d \
  --name twitch-1337 \
  -v ./config.toml:/app/config.toml:ro \
  chronophylos/twitch-1337:latest

# View logs
docker logs -f twitch-1337
```

## Configuration

Config via `config.toml`. Start:

```bash
cp config.toml.example config.toml
# Edit config.toml with your credentials
cargo run
```

### Configuration File Structure

config.toml sections:

**[twitch]** - Twitch IRC connection + auth
- `channel` - Channel monitor (no # prefix)
- `username` - Bot Twitch username
- `refresh_token` - OAuth refresh token (auto-refreshed, persisted `token.ron`)
- `client_id` - Twitch app client ID
- `client_secret` - Twitch app client secret
- `expected_latency` - Initial IRC latency seed ms (optional, default: 100). Auto-measured via PING/PONG.
- `hidden_admins` - (optional) Vec Twitch user IDs, admin access ping commands
- `admin_channel` - (optional) Separate channel bot joins for testing. Broadcaster only. Omit to disable.

**[pings]** - Ping system config
- `cooldown` - Default cooldown between triggers, seconds (optional, default: 300)
- `public` - Allow anyone trigger, not just members (optional, default: false)

**[ai]** (optional) - AI config `!ai` command
- `backend` - Required: `"openai"` (OpenAI-compat API) or `"ollama"` (Ollama native)
- `api_key` - Required openai backend, unused ollama
- `base_url` - Optional: API base URL (default OpenRouter openai, localhost:11434 ollama)
- `model` - Required: Model ID (e.g., `"google/gemini-2.0-flash-exp:free"` openai, `"gemma3:4b"` ollama)
- `system_prompt` - Optional: System prompt (sensible default)
- `instruction_template` - Optional: Template `{message}` + `{chat_history}` placeholders (default: `"{chat_history}\n{message}"`)
- `timeout` - Optional: AI request timeout seconds (default: 30)
- `history_length` - Recent chat messages as context (optional, default: 0 = disabled, max: 100). Main-channel buffered; admin channel excluded. Injected via `{chat_history}`.
- `history_prefill` - (optional) Sub-table prefill history from log API at startup. Needs `history_length > 0`.
  - `base_url` - Rustlog-compat API base URL (optional, default: `"https://logs.zonian.dev"`)
  - `threshold` - Float 0.0-1.0: today's messages below this fraction of `history_length` → also fetch yesterday (optional, default: 0.5)
- `memory_enabled` - Enable persistent AI memory (optional, default: false)
- `max_memories` - Max stored facts (optional, default: 50, max: 200)

**[[schedules]]** (optional, repeatable) - Scheduled messages
- `name` - Unique ID
- `message` - Text post chat
- `interval` - Frequency "hh:mm" (e.g., "01:00" = 1h)
- `start_date` - (optional) Start posting (ISO 8601: YYYY-MM-DDTHH:MM:SS)
- `end_date` - (optional) Stop posting
- `active_time_start` - (optional) Daily start HH:MM
- `active_time_end` - (optional) Daily end HH:MM
- `enabled` - (optional, default: true) false = disable, don't delete

See `config.toml.example` for annotated example.

### Docker Deployment

Docker: mount config.toml as volume:

```bash
docker run -d \
  --name twitch-1337 \
  -v ./config.toml:/app/config.toml:ro \
  chronophylos/twitch-1337:latest
```

Or Docker Compose:

```yaml
services:
  twitch-1337:
    image: chronophylos/twitch-1337:latest
    restart: unless-stopped
    volumes:
      - ./config.toml:/app/config.toml:ro
```

## Token Storage

Bot persists refreshed OAuth tokens `./token.ron` (Rust Object Notation):
- Auto-saves updated tokens on refresh by twitch-irc
- Falls back to refresh token from `config.toml` first run if file missing
- No manual token updates on expire
- Uses `FileBasedTokenStorage` implementing `TokenStorage` trait

## Architecture

### Persistent Connection with Broadcast-Based Message Routing

Bot maintains **single persistent IRC connection**, uses **broadcast channel** to distribute messages to multiple independent handlers:

1. **Setup and Connection** (startup):
   - Create authenticated IRC client with refreshing token credentials
   - Connect Twitch IRC, join configured channel
   - Verify auth via GlobalUserState message (30s timeout)
   - Create broadcast channel (capacity: 100) for message distribution

2. **Message Router Task** (continuous):
   - Reads twitch-irc UnboundedReceiver
   - Broadcasts all ServerMessages to subscribed handlers
   - Runs until connection closes

3. **Handler Tasks** (parallel, continuous):
   - **Config Watcher Service**: Watches config.toml (2s debounce)
   - **Scheduled Message Handler**: Posts per config.toml schedules (if configured)
   - **1337 Handler**: Daily scheduled monitor (13:36-13:38), updates leaderboard
   - **Generic Command Handler**: `CommandDispatcher` routes PRIVMSGs to registered `Command` impls 24/7 (`!p`, `!<ping>`, `!ai`, `!lb`, `!fb`, `!up`, `!fl`, `!track`, `!untrack`, `!flight`, `!flights`)
   - **Latency Monitor**: IRC latency via PING/PONG every 5min
   - **Flight Tracker**: Long task behind `mpsc::Sender<TrackerCommand>`, polls ADS-B APIs, detects phase changes, posts chat updates, persists `flights.ron`
   - Each handler independent
   - Handlers filter relevant messages, act

4. **Graceful Shutdown**:
   - Main task waits Ctrl+C or any handler exit
   - All tasks in tokio::select! for coordinated shutdown

**Benefits persistent connection + broadcast:**
- Single connection overhead (no repeated auth)
- Handlers independent, add/remove easy
- No shared state between handlers (loose coupling)
- Broadcast channel handles backpressure (lagging handlers warned)
- Clean separation of concerns

### Key Dependencies

**Runtime & Async:**
- `tokio` - Async runtime (features: macros, rt-multi-thread, time, signal, fs)

**IRC & Networking:**
- `twitch-irc` - Twitch IRC client (features: refreshing-token-rustls-webpki-roots, transport-tcp-rustls-webpki-roots)
- `reqwest` - HTTP client LLM backends + aviation APIs (features: json, rustls-tls-webpki-roots)

**File Watching:**
- `notify-debouncer-mini` - Filesystem watcher with debounce for config reload

**Time Handling:**
- `chrono` - Date/time ops
- `chrono-tz` - Timezone handling Europe/Berlin (filter-by-regex feature)
  - Only compiles Berlin timezone data via `.cargo/config.toml`

**Error Handling & Logging:**
- `color-eyre` / `eyre` - Rich error messages with context
- `tracing` - Structured logging
- `tracing-subscriber` - Log format + filter (env-filter feature)

**Security:**
- `secrecy` - Protects OAuth tokens + secrets in memory (prevents accidental logging)

**Serialization:**
- `serde` - Serialization framework (derive feature)
- `serde_json` - JSON for API
- `ron` - Rust Object Notation, token + ping storage
- `toml` - Config file parse

**Other:**
- `rand` - Randomize bot response messages
- `async-trait` - TokenStorage + Command trait impls

### State Management

**1337 Handler State:**
- `total_users`: `Arc<Mutex<HashSet<String>>>` - Tracks unique usernames said "1337"/"DANKIES" at 13:37
- Created fresh daily 13:36, discarded after stats at 13:38
- Shared handler task + monitoring subtask via tokio::sync::Mutex
- Max capacity: 10,000 users (prevents unbounded growth)

**Scheduled Messages State:**
- `schedule_cache`: `Arc<RwLock<ScheduleCache>>` - Shared cache schedules loaded config.toml
- Contains schedules vec, last update timestamp, version number
- Updated on config.toml change (file watcher, 2s debounce)
- Version increments trigger task manager to spawn/stop message tasks

**Ping System State:**
- `ping_manager`: `Arc<RwLock<PingManager>>` - Manages ping defs, membership, cooldowns
- Persist `pings.ron` via atomic write+rename
- Cooldown tracking in-memory `HashMap<String, Instant>` (not persisted, reset on restart)
- Shared across `PingAdminCommand` + `PingTriggerCommand`

**AI Memory State:**
- `memory_store`: `Arc<RwLock<MemoryStore>>` - Channel-wide fact store managed by LLM
- Persist `ai_memory.ron` via atomic write+rename
- Load at startup if `memory_enabled = true` in `[ai]`
- Read every `!ai` request (injected into system prompt)
- Written by fire-and-forget extraction task after each successful AI response
- Capped at `max_memories` (default 50, max 200)

**Leaderboard State:**
- `leaderboard`: `Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>` - All-time sub-1s PBs for 1337
- Load at startup from `leaderboard.ron`, written back after 13:38 stats via atomic write+rename
- Shared `run_1337_handler` (writer) + `LeaderboardCommand` (reader, `!lb`)

**Flight Tracker State:**
- Owned by `run_flight_tracker` task; external callers via `tokio::sync::mpsc::Sender<TrackerCommand>` (channel capacity 32)
- `FlightTrackerState { flights: Vec<TrackedFlight> }` persist `flights.ron` on every mutation (atomic write+rename)
- Poll cadence adaptive (`POLL_FAST`/`POLL_NORMAL`/`POLL_SLOW` = 30/60/120s), based on current phase mix
- Caps: `MAX_TRACKED_FLIGHTS = 12` global, `MAX_FLIGHTS_PER_USER = 3`
- Tracking transitions: lost at `TRACKING_LOST_THRESHOLD = 300s`, removed at `TRACKING_LOST_REMOVAL = 1800s`

**Aviation Data (read-only, embedded):**
- `data/plz.csv`, `data/airports.csv`, `data/airlines.csv` loaded via `include_str!` into `AviationClient` at compile time — no runtime file IO, no deployment dependency

**Persistent State (under `$DATA_DIR`, default `/var/lib/twitch-1337`):**
- OAuth tokens `token.ron`
- Ping defs + membership `pings.ron`
- AI memories `ai_memory.ron` (if `memory_enabled = true`)
- 1337 leaderboard `leaderboard.ron`
- Flight tracker state `flights.ron`
- User feedback log `feedback.txt` (append-only)

**No Persistent State:**
- Schedules loaded from config.toml at startup + file changes
- Per-command/per-user cooldown timestamps (in-memory, reset on restart)

### Configuration Files

- `.cargo/config.toml` - Sets `CHRONO_TZ_TIMEZONE_FILTER = "Europe/Berlin"` to shrink binary
- `Cargo.toml` - Minimal feature flags, smaller binaries + faster compile
- `Dockerfile` - Multi-stage build, cargo-chef optimization, FROM scratch final image
- `Justfile` - Task runner, dev + Docker ops
- `.dockerignore` - Excludes unnecessary files from Docker build context
- `config.toml.example` - Template with all options

## System Dependencies & Deployment

### Binary Types

**Standard build (default target: x86_64-unknown-linux-gnu):**
- Dynamically linked glibc
- Requires: `libc.so.6`, `libm.so.6`, `libgcc_s.so.1`, `ld-linux-x86-64.so.2`
- Size: ~6.2MB
- Works on most modern Linux (glibc 2.31+)

**Static musl build (target: x86_64-unknown-linux-musl):**
- Fully static, zero runtime deps
- Size: ~6.0MB
- Works Alpine, busybox, any Linux kernel 4.4.0+
- **Recommended for minimal/container deployments**
- Build: `cargo build --release --target x86_64-unknown-linux-musl`

### Docker Deployment

Multi-stage Dockerfile with cargo-chef for optimal caching:

**Build stages:**
1. **Planner** - Analyzes Cargo.toml, generates dep recipe
2. **Cacher** - Builds deps only (cached until Cargo.toml changes)
3. **Builder** - Builds app (only rebuilds on source change)
4. **Runtime** - `FROM scratch` with static musl binary

**Build base image:** `rust:1.89-bookworm` (Debian-based)
- Installs `musl-tools` for cross-compile
- Adds `x86_64-unknown-linux-musl` target via rustup
- Needed: `ring` crate (rustls) requires C compiler for musl

**Image characteristics:**
- Runtime base: `FROM scratch` (no OS, no shell, no utils)
- Size: ~6.3MB (just static binary)
- Security: Minimal attack surface
- Deps: None - static binary runs on kernel directly
- Build cache: ~2-3s rebuilds on source-only changes

**Why this works:**
- `rustls` instead of OpenSSL (pure Rust TLS, minimal C deps)
- musl-tools provides `musl-gcc` for static C compile
- All deps statically linked into final binary
- Minimal tokio features reduce bloat

### Deployment Options

**Docker (recommended):**
```bash
just build && just run
```

**Docker Compose:**
```yaml
services:
  twitch-1337:
    image: chronophylos/twitch-1337:latest
    restart: unless-stopped
    volumes:
      - ./config.toml:/app/config.toml:ro
```

**Systemd service (musl binary):**
```bash
# Copy musl binary
sudo cp target/x86_64-unknown-linux-musl/release/twitch-1337 /usr/local/bin/
# Create systemd service with config.toml in working directory
```

**Alpine/busybox:**
- Use musl binary or Docker image
- Both work, no extra packages

## Code Structure

### Main Entry Point

**`main() -> Result<()>` (src/main.rs)**
- Init error handling (color-eyre), logging (tracing-subscriber)
- Load + validate config `config.toml`
- Calls `setup_and_verify_twitch_client()` for authenticated connection
- Creates broadcast channel, 100-msg capacity
- Spawns handler tasks per config
- `tokio::select!` waits Ctrl+C or any task exit

### Connection Management

**`setup_and_verify_twitch_client() -> Result<(UnboundedReceiver, Client)>`**
- Creates IRC client with `RefreshingLoginCredentials<FileBasedTokenStorage>`
- Connects Twitch IRC, joins configured channel
- Waits `GlobalUserState` to verify auth (30s timeout)
- Returns verified client + incoming message receiver
- Detects + reports auth failures with helpful error messages

**`setup_twitch_client() -> (UnboundedReceiver, Client)`**
- Creates `RefreshingLoginCredentials` with client ID, secret, token storage
- Builds `ClientConfig` + `TwitchIRCClient`
- Returns receiver + client for connection setup

### Message Distribution

**`run_message_router(incoming_messages, broadcast_tx)`**
- Reads twitch-irc UnboundedReceiver
- Broadcasts all ServerMessages to subscribed handlers
- Exits on connection close

### Handler: 1337 Tracker

**`run_1337_handler(broadcast_tx, client)`**
- Infinite loop: waits until 13:36 Berlin daily
- Creates fresh `HashSet` for today's users
- Spawns `monitor_1337_messages()` subtask
- 13:36:30: posts "PausersHype" reminder
- 13:38: generates + posts stats message
- Aborts monitor subtask, repeats next day

**`monitor_1337_messages(broadcast_rx, total_users)`**
- Filters PRIVMSG at exactly 13:37:xx Berlin
- Checks message contains "1337" or "DANKIES" via `is_valid_1337_message()`
- Ignores known bots: "supibot", "potatbotat"
- Inserts unique usernames into shared HashSet (max 10,000)

**`generate_stats_message(count, user_list) -> String`**
- Returns contextual German message per count:
  - 0: "Erm" or "fuh"
  - 1: "@user zumindest einer..."
  - 2-3: Special handling specific users
  - 4: "3.6, nicht gut, nicht dramatisch"
  - 5-7: "gute Auslese"
  - 8+: "insane quota Pag"
- `one_of()` randomly picks from message variants

### Handler: Latency Monitor

**`run_latency_handler(client, broadcast_tx, latency)`**
- Infinite loop, sleeps `LATENCY_PING_INTERVAL` (5min) between cycles
- Sends `PING` with timestamp nonce via `irc!["PING", nonce]`
- Subscribes broadcast channel before sending (avoids race)
- Matches `ServerMessage::Pong` where `source.params[1]` equals nonce
- Computes one-way latency (RTT/2), updates EMA with `LATENCY_EMA_ALPHA` (0.2)
- Stores rounded EMA in shared `Arc<AtomicU32>` with `Relaxed` ordering
- Handles: PING fail (warn, skip), PONG timeout (warn, skip), channel lagged (warn, continue)

### Handler: Generic Commands

**`run_generic_command_handler(CommandHandlerConfig)`**
- Takes `CommandHandlerConfig` struct bundling: `broadcast_tx`, `client`, `ai_config`, `leaderboard`, `ping_manager`, `hidden_admin_ids`, `default_cooldown`, `pings_public`, `cooldowns`, `tracker_tx`, `aviation_client`, `admin_channel`, `bot_username`, `channel`
- Builds `CommandDispatcher` with every registered command
- Subscribes broadcast channel, dispatches PRIVMSG
- Also joins `admin_channel` (if configured) so broadcaster tests commands privately

**`CommandDispatcher` / `Command` trait** (`src/commands/mod.rs`)
- `Command` trait: `name() -> &str`, `enabled() -> bool` (default true), `matches(word) -> bool` (default exact match on `name()`), `async execute(ctx: CommandContext) -> Result<()>`
- `CommandContext<'a>`: `privmsg: &PrivmsgMessage`, `client: &Arc<AuthenticatedTwitchClient>`, `trigger: &str`, `args: Vec<&str>`
- Dispatcher: per PRIVMSG splits first whitespace word, finds first command whose `matches()` returns true, invokes `execute()`

### Registered Commands

All command structs under `src/commands/*.rs`, implement `Command`:

- **`PingAdminCommand` (`!p`)** — `src/commands/ping_admin.rs`
  - Admin subcommands (broadcaster/mod/hidden_admin): `!p create <name> <template>`, `!p delete <name>`, `!p add <name> <user>`, `!p remove <name> <user>`
  - User subcommands (all): `!p join <name>`, `!p leave <name>`, `!p list`
- **`PingTriggerCommand` (`!<ping>`)** — `src/commands/ping_trigger.rs`
  - Overrides `matches()` to dynamically match any `!<name>` in `PingManager`
  - Members-only unless `[pings].public = true`; per-ping or default cooldown; renders `{mentions}` (sender excluded) + `{sender}`
  - Replies with remaining cooldown via `cooldown::format_cooldown_remaining`; silent on non-member / empty mentions
- **`AiCommand` (`!ai`)** — `src/commands/ai.rs`
  - Calls configured `LlmClient` with optional chat history (`ChatContext`) + memory injection
  - Spawns `memory::spawn_memory_extraction` fire-and-forget after each response when `memory_enabled`
- **`LeaderboardCommand` (`!lb`)** — `src/commands/leaderboard.rs`
  - Reads shared leaderboard, returns fastest all-time `PersonalBest` (ms + date)
- **`FeedbackCommand` (`!fb <message>`)** — `src/commands/feedback.rs`
  - Appends timestamped line `$DATA_DIR/feedback.txt`; per-user cooldown
- **`FlightsAboveCommand` (`!up`)** — `src/commands/flights_above.rs`
  - Queries ADS-B via `AviationClient` for aircraft above PLZ, airport (ICAO/IATA), raw coords; disabled if aviation init failed
- **`RandomFlightCommand` (`!fl`)** — `src/commands/random_flight.rs`
  - Returns plausible random airline flight for aircraft ICAO type + duration, using embedded airline data
- **`TrackCommand` (`!track <callsign|hex>`)** — `src/commands/track.rs`
  - Sends `TrackerCommand::Track` over `tracker_tx` to flight tracker task
- **`UntrackCommand` (`!untrack <callsign|hex>`)** — `src/commands/untrack.rs`
  - Sends `TrackerCommand::Untrack` with `is_mod` from badges (mods force-remove anyone's flight)
- **`FlightsCommand` (`!flights`) / `FlightCommand` (`!flight <query>`)** — `src/commands/flights.rs`
  - Both send `TrackerCommand::Status` (optional query), await reply on oneshot channel

### Handler: Flight Tracker

**`flight_tracker::run_flight_tracker(rx, client, channel, aviation_client, data_dir)`** (`src/flight_tracker.rs`)
- Loads `FlightTrackerState` from `flights.ron` (empty if missing or corrupt)
- Main loop: `tokio::select!` between `rx.recv()` (commands from chat) + poll timer
- `process_command` handles `TrackerCommand::{Track, Untrack, Status}`, enforces per-user + global caps, persists after mutations
- `poll_all_flights` queries adsb.lol per tracked hex, updates `TrackedFlight`, runs `detect_phase`, emits chat messages via `msg_*` helpers on state changes (takeoff, cruise, descent, approach, landing, divert, tracking-lost, emergency squawk)
- `compute_poll_interval` picks `POLL_FAST`/`POLL_NORMAL`/`POLL_SLOW` based on fastest-changing phase currently tracked
- Emergency squawks (7500/7600/7700) annotated via `emergency_squawk_meaning`

**Key types:**
- `FlightIdentifier` enum: `Callsign(String)` | `Hex(String)` — parse input, match aircraft
- `FlightPhase` enum: `Preflight`, `Takeoff`, `Cruise`, `Descent`, `Approach`, `Landed`, `Lost`, ...
- `TrackedFlight`: persisted per-flight state (identifier, phase, last seen, requested_by, etc.)
- `TrackerCommand` enum: `Track`, `Untrack`, `Status` — each carries `oneshot` reply channel for chat reply

### Handler: Scheduled Messages (Config-Based)

**Only runs if schedules configured** (`[[schedules]]` sections in `config.toml`).

**`run_config_watcher_service(cache)`**
- Uses `notify-debouncer-mini` to watch config.toml
- 2-second debounce, avoids rapid reloads
- Reloads + validates config on change
- Updates cache (increments version) on successful reload
- Keeps existing schedules if reload fails

**`run_scheduled_message_handler(client, cache)`**
- Monitors cache for version changes every 30s
- Spawns new tasks for added schedules
- Stops tasks for removed schedules
- Each schedule runs in independent task
- Dynamic task mgmt, no bot restart

**`run_schedule_task(schedule, client, cache)`**
- Runs single schedule in loop
- Sleeps configured interval between posts
- Checks schedule still in cache before post
- Validates schedule active (respects date range + time window)
- Posts message to Twitch chat
- Exits gracefully if schedule removed from cache

**`load_schedules_from_config(config) -> Vec<Schedule>`**
- Iterates `config.schedules` array
- Skips disabled schedules
- Converts `ScheduleConfig` to `database::Schedule`
- Validates each schedule
- Returns vec of validated schedules

**`reload_schedules_from_config() -> Option<Vec<Schedule>>`**
- Reads config.toml from disk (sync, called from file watcher)
- Parses + validates config
- Returns None on error (keeps existing schedules)

### Database Module

**`database::Schedule`**
- Stores schedule config
- Fields: name, start_date, end_date, active_time_start, active_time_end, interval, message
- Methods:
  - `is_active(now)`: Checks schedule active per date range + time window
  - `parse_interval(s)`: Parses "hh:mm" or legacy "30m"/"1h"/"2h30m" to TimeDelta
  - `validate()`: Validates required fields + logical consistency
- Derives: Debug, Clone, Deserialize, Serialize

**`database::ScheduleCache`**
- Container for loaded schedules with metadata
- Fields: schedules (Vec<Schedule>), last_updated, version
- Methods:
  - `new()`: Creates empty cache
  - `update(schedules)`: Updates schedules, increments version
- Version number enables change detection for task manager

### Memory Module

**`memory::Memory`**
- Single remembered fact
- Fields: fact, created_at (ISO 8601 string), updated_at (ISO 8601 string)

**`memory::MemoryStore`**
- Persistent store AI memories, serialized to RON
- Fields: memories (`HashMap<String, Memory>`, keyed by LLM-generated slug)
- Methods:
  - `load(data_dir) -> Result<(Self, PathBuf)>`: Loads from `ai_memory.ron`, empty store if missing
  - `save(path) -> Result<()>`: Atomic write via `.ron.tmp` + rename
  - `format_for_prompt() -> Option<String>`: Sorted `## Known facts` block for system prompt injection
  - `format_for_extraction() -> String`: Sorted key:fact list for extraction prompt
  - `execute_tool_call(call, max_memories) -> String`: Handles `save_memory` + `delete_memory` tool calls

**`memory::spawn_memory_extraction(...)`**
- Fire-and-forget tokio task, post-response memory extraction
- Sends conversation to LLM with tool defs, executes tool calls against MemoryStore
- Up to 3 rounds of tool calling, persists after each round
- Errors logged at debug, never affects user-visible response

### Cooldown Module

**`cooldown::format_cooldown_remaining(remaining: Duration) -> String`**
- Formats `Duration` as human-friendly cooldown: `"30s"`, `"4m 3s"`, `"1h 5m"`
- Used by all cooldown-gated commands, consistent chat responses

### Prefill Module

**`prefill::HistoryPrefillConfig`**
- Config for startup history prefill
- Fields: base_url (String), threshold (f64)
- Deserialized from `[ai.history_prefill]` in config.toml

**`prefill::prefill_chat_history(channel, history_length, config) -> VecDeque<(String, String)>`**
- Fetches recent messages from rustlog-compat API at startup
- Fetches today's messages; if count < threshold * history_length, also fetches yesterday
- Merges chronologically, returns at most history_length messages
- On any failure, logs warning, returns what it has (or empty buffer)
- Uses Europe/Berlin timezone for date calc

### Ping Module

**`ping::Ping`**
- Single ping definition
- Fields: name, template, members (Vec<String>), cooldown (Option<u64>), created_by

**`ping::PingStore`**
- Top-level container serialized to/from `pings.ron`
- Fields: pings (HashMap<String, Ping>)

**`ping::PingManager`**
- Manages ping state + persistence, shared as `Arc<RwLock<PingManager>>`
- Fields: store (PingStore), last_triggered (HashMap<String, Instant>), path (PathBuf)
- Methods:
  - `load() -> Result<Self>`: Loads from `pings.ron`, creates empty if missing
  - `save() -> Result<()>`: Atomic write via tmp file + rename
  - `create_ping(name, template, created_by, cooldown) -> Result<()>`: Creates new ping
  - `delete_ping(name) -> Result<()>`: Deletes ping
  - `add_member(ping_name, username) -> Result<()>`: Adds user to ping
  - `remove_member(ping_name, username) -> Result<()>`: Removes user from ping
  - `ping_exists(name) -> bool`: Checks ping exists
  - `is_member(ping_name, username) -> bool`: Checks membership
  - `list_pings_for_user(username) -> Vec<&str>`: Lists pings user belongs to
  - `remaining_cooldown(ping_name, default_cooldown) -> Option<Duration>`: Returns `Some(remaining)` if on cooldown, `None` if ready
  - `record_trigger(ping_name)`: Records trigger timestamp for cooldown
  - `render_template(ping_name, sender) -> Option<String>`: Renders template with `{mentions}` (sender excluded) + `{sender}`

### Aviation Module (`src/aviation.rs`)

**`AviationClient`**
- Wraps `reqwest::Client` with custom user agent; cheap to `clone()` (internal `Arc`)
- `new() -> Result<Self>` — builds client, fails only on TLS init
- `get_aircraft_by_hex(hex)` / `get_aircraft_nearby(lat, lon, radius_nm)` — hit adsb.lol v2; see `reference_adsb_aggregators.md` memory for free drop-in fallbacks (airplanes.live, adsb.fi, ADSB.One)
- `up_command(...)` — implements `!up` behavior end-to-end (resolve location, query nearby, format reply)

**Embedded CSV lookups (`include_str!` at compile time):**
- `plz_to_coords(plz) -> Option<(f64, f64)>` — German postal code → lat/lon (from `data/plz.csv`)
- `iata_to_coords(code)` / airport helpers — IATA/ICAO → coords + name (from `data/airports.csv`)
- Airline IATA → ICAO mapping (from `data/airlines.csv`) — used by `RandomFlightCommand`
- Embedding = static musl binary has zero runtime data-file dependency

**`NearbyAircraft`** — deserialized ADS-B snapshot (hex, callsign, `AltBaro::{Feet, Ground}`, coords, vertical rate, squawk, heading, ground speed, etc.)

### LLM Module (`src/llm/`)

**`llm::LlmClient` trait** (`mod.rs`)
- `async fn chat_completion(req: ChatCompletionRequest) -> Result<String>` — single-shot
- `async fn tool_chat_completion(req: ToolChatCompletionRequest) -> Result<ToolChatCompletionResponse>` — tool-calling, used by memory extraction
- Shared request/response types: `Message { role, content }`, tool defs, prior-round replay

**`llm::openai` (`openai.rs`)** — OpenAI-compat JSON wire format (`/v1/chat/completions`); works with OpenAI, OpenRouter, LiteLLM, vLLM, etc.

**`llm::ollama` (`ollama.rs`)** — Ollama native API (`/api/chat`); tool calls use Ollama's `tool_name` field convention

Correct client built at startup from `[ai].backend`, handed to `AiCommand` + `memory::spawn_memory_extraction` as `Arc<dyn LlmClient>`.

### Token Storage

**`FileBasedTokenStorage`** (`src/main.rs`)
- Implements twitch-irc `TokenStorage` trait
- Stores tokens `$DATA_DIR/token.ron` (default `/var/lib/twitch-1337/token.ron`)
- `load_token()`: Reads from file, falls back to refresh token from config.toml first run
- `update_token()`: Writes updated tokens to file (auto-called by twitch-irc)

### Data Directory

**`get_data_dir() -> PathBuf`** / **`ensure_data_dir() -> Result<()>`** (`src/main.rs`)
- Resolves `$DATA_DIR` env var, default `/var/lib/twitch-1337`
- `ensure_data_dir` creates directory if missing at startup
- **All** runtime-persistent files must go here via `get_data_dir().join(...)` — never hardcode relative paths. See `ping.rs`, `memory.rs`, `flight_tracker.rs`, `feedback.rs` for pattern.
- Dockerfile sets `ENV DATA_DIR=/data`; mount volume there in prod.

### Configuration Types

**`Configuration`**
- Main config struct loaded from config.toml
- Contains: `twitch`, `pings`, `schedules` (Vec<ScheduleConfig>), optionally `ai`
- `validate()` ensures all required fields present + valid

**`TwitchConfiguration`**
- `channel`, `username` - Twitch channel + bot username
- `refresh_token`, `client_id`, `client_secret` - OAuth credentials (SecretString)
- `expected_latency` - Initial latency seed ms (optional, default: 100, auto-measured via PING/PONG)
- `hidden_admins` - Vec Twitch user IDs with admin access to ping commands

**`PingsConfig`**
- `cooldown` - Default cooldown between ping triggers seconds (default: 300)
- `public` - Allow anyone trigger pings, not just members (default: false)

**`AiConfig`**
- Config for AI/LLM integration (`[ai]` section in config.toml)
- `backend` - `AiBackend` enum: `Openai` or `Ollama`
- `api_key` - API key (SecretString, required for openai backend)
- `base_url` - Optional base URL override
- `model` - Model ID string
- `system_prompt` - Optional system prompt (has default)
- `instruction_template` - Optional template with `{message}` + `{chat_history}` placeholders (has default)
- `history_length` - Number chat messages to keep as context (default: 0)
- `history_prefill` - Optional `HistoryPrefillConfig` for startup history prefill
- `timeout` - AI request timeout seconds (default: 30)
- `memory_enabled` - Enable persistent AI memory (default: false)
- `max_memories` - Max stored facts (default: 50, max: 200)

**`AiBackend`**
- Enum: `Openai`, `Ollama`
- Deserialized from `"openai"` or `"ollama"` in config.toml

**`ScheduleConfig`**
- Config for scheduled message in config.toml
- `name` - Unique ID
- `message` - Text to post
- `interval` - Frequency ("hh:mm")
- `start_date`, `end_date` - Optional date range (ISO 8601)
- `active_time_start`, `active_time_end` - Optional daily time window (HH:MM)
- `enabled` - Schedule active (default: true)

### Constants

`src/main.rs`:
- `TARGET_HOUR: u32 = 13`, `TARGET_MINUTE: u32 = 37` - 1337 tracking window
- `MAX_USERS: usize = 10_000` - Max tracked users per day in 1337 HashSet
- `CONFIG_PATH: &str = "./config.toml"` - Config file path (working-dir relative)
- `LEADERBOARD_FILENAME: &str = "leaderboard.ron"` - joined with `$DATA_DIR`
- `LATENCY_PING_INTERVAL: Duration = 300s`, `LATENCY_PING_TIMEOUT: Duration = 10s`
- `LATENCY_EMA_ALPHA: f64 = 0.2`, `LATENCY_LOG_THRESHOLD: u32 = 10`

`src/ping.rs`: `PINGS_FILENAME: &str = "pings.ron"` (joined with `$DATA_DIR`)

`src/flight_tracker.rs`:
- `FLIGHTS_FILENAME: &str = "flights.ron"`
- `MAX_TRACKED_FLIGHTS: usize = 12`, `MAX_FLIGHTS_PER_USER: usize = 3`
- `TRACKING_LOST_THRESHOLD: 300s`, `TRACKING_LOST_REMOVAL: 1800s`
- `POLL_FAST: 30s`, `POLL_NORMAL: 60s`, `POLL_SLOW: 120s`, `POLL_TIMEOUT: 10s`

## Important Notes

- **Persistent Connection**: Single IRC connection 24/7, not ephemeral sessions
- **Secure**: OAuth tokens + secrets wrapped in `SecretString`, won't appear in debug output
- **Error Handling**: All failures logged with context, handlers continue on error
- **Timezone**: All time-based ops use `Europe/Berlin`
- **1337 Tracking**: Messages containing "1337" or "DANKIES" at exactly 13:37:xx Berlin
- **Bot Filtering**: Ignores "supibot" + "potatbotat"
- **Deduplication**: Same user counted once per day via HashSet (1337 handler)
- **Binary Size**: Optimized with minimal tokio features + single timezone (6MB)
- **Logging**: Structured logs with tracing, configurable via `RUST_LOG`
- **Broadcast Architecture**: Message router distributes to independent handlers
- **Ping Persistence**: Ping defs + membership stored `$DATA_DIR/pings.ron` (atomic write+rename)
- **Token Refresh**: Tokens auto-refreshed + saved `$DATA_DIR/token.ron`
- **Data Directory**: All persistent files under `$DATA_DIR` (default `/var/lib/twitch-1337`); Dockerfile sets it to `/data`. Use `get_data_dir()` — never hardcode relative paths.
- **Embedded Data**: `data/plz.csv`, `data/airports.csv`, `data/airlines.csv` compiled into binary via `include_str!` — no runtime file deps.
- **Hot Reload**: Schedules reload auto on config.toml change (2s debounce)
- **Latency Auto-Measurement**: IRC latency measured via PING/PONG every 5min, EMA (alpha=0.2) updates shared `AtomicU32` read by timing-sensitive handlers

## Feature Timelines

### 1337 Tracker (Daily)
```
13:36:00 -> Handler wakes, creates fresh HashSet, subscribes to broadcast
13:36:30 -> Posts "PausersHype" reminder
13:37:00-13:37:59 -> Monitoring subtask tracks "1337"/"DANKIES" messages (unique users)
13:38:00 -> Posts stats message with contextual response
         -> Aborts monitor subtask, waits for next day
```

### Ping System (24/7)
```
Admin commands (!ping create/delete/add/remove):
           -> Requires broadcaster/moderator badge or hidden_admin user ID
           -> Modifies PingManager state, persists to pings.ron

User commands (!ping join/leave/list):
           -> Open to all users
           -> Self-service subscription management

Ping triggers (!<name>):
           -> Dynamically matches any registered ping name
           -> Only members can trigger (unless public mode enabled), checks cooldown
           -> Renders template with {mentions} and {sender}
           -> Replies with remaining cooldown time when on cooldown
           -> Silent on non-member or empty mentions
```

### Scheduled Messages (Conditional - Only if schedules configured)
```
Startup    -> Load schedules from config.toml
           -> Spawn config watcher service and message handler
           -> Spawn tasks for each active schedule

On config.toml change:
           -> Debounce 2 seconds
           -> Reload and validate config
           -> Update cache (increment version)
           -> Keep old schedules if reload fails

Every 30s  -> Task manager checks cache version
           -> Spawn tasks for new schedules
           -> Stop tasks for removed schedules

Per Task   -> Sleep for schedule's interval
           -> Check if schedule still in cache
           -> Check if schedule is active (date range + time window)
           -> Post message to chat
           -> Repeat or exit if removed
```

### Flight Tracker (Continuous)
```
Startup    -> Load FlightTrackerState from $DATA_DIR/flights.ron
           -> Initialize AviationClient
           -> Enter select! loop over (mpsc rx, poll timer)

On command -> process_command handles Track/Untrack/Status
           -> Enforce MAX_TRACKED_FLIGHTS=12, MAX_FLIGHTS_PER_USER=3
           -> Persist state to flights.ron (atomic write+rename)
           -> Reply via oneshot channel from CommandContext

Every 30-120s (adaptive) -> poll_all_flights queries adsb.lol per tracked hex
           -> detect_phase compares altitude / vertical_rate / ground state
           -> Emit msg_* chat messages on phase change (takeoff/cruise/descent/approach/landing)
           -> Annotate emergency squawks (7500/7600/7700)
           -> Mark flight lost after 300s missing, remove after 1800s
```

### Latency Monitor (Continuous)
```
Startup    -> Seed EMA from config.twitch.expected_latency (default: 100ms)
Every 5min -> Send PING with timestamp nonce
           -> Wait up to 10s for matching PONG (nonce in source.params[1])
           -> Compute one-way latency (RTT/2), update EMA (alpha=0.2)
           -> Store in shared Arc<AtomicU32>, read by 1337 handler for sleep_until_hms
           -> Log measurement at debug, log EMA shift >= 10ms at info
```

**If no schedules configured:**
```
Startup    -> Log info: "No schedules configured, scheduled messages disabled"
           -> Do not spawn watcher service or message handler
           -> Bot continues with other handlers
```

## Development Tips

### Pre-commit Checks (required)

CI runs `cargo fmt --all -- --check` + `cargo clippy --all-targets -- -D warnings` — any drift or warning fails build. Before committing, always run:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

Notes:
- Run `cargo fmt --all` (not just `cargo fmt`) so every workspace crate covered.
- Clippy lints strict (`-D warnings`), additional lints enabled in `Cargo.toml` under `[lints.clippy]` — treat every warning as build failure.
- After any code change introducing new functions, expressions, or imports, re-run fmt + clippy before staging. Don't rely on editors — verify with CLI.
- If clippy lint feels wrong, fix underlying issue or add targeted `#[allow(...)]` with one-line comment explaining why. Do not relax CI gate.

### General
- `twitch_irc::irc!` macro requires standalone `use twitch_irc::irc;` — cannot be added to braced `use twitch_irc::{...}` block
- On request-response on broadcast channel (e.g., PING/PONG), subscribe BEFORE sending to avoid race
- Use `RUST_LOG=debug` for all IRC messages + handler activity
- Use `RUST_LOG=trace` for every ServerMessage received (very verbose)
- Handlers independent - errors in one don't crash others
- Broadcast channel capacity: 100 messages (handlers warned if lagging)
- All handlers run in infinite loops - only exit on channel close or panic
- Config loaded from `config.toml` at startup, reloaded on file change

### Adding New Handlers
1. Create handler: `async fn run_my_handler(broadcast_tx, client)`
2. Subscribe broadcast: `let mut broadcast_rx = broadcast_tx.subscribe()`
3. Loop `broadcast_rx.recv().await`, filter relevant messages
4. Spawn in main(): `tokio::spawn(run_my_handler(broadcast_tx.clone(), client.clone()))`
5. Add to `tokio::select!` for coordinated shutdown

### Deployment Workflow
- `just deploy` builds locally with podman, pushes to remote docker host via SSH, restarts
- Justfile assumes podman locally, docker on remote
- Deployment target: `docker.homelab` SSH host
- Remote project dir: `twitch` (docker compose location)

### Binary Size
- Standard build: ~6.2MB (glibc, dynamically linked)
- Musl build: ~6.0MB (static, no deps)
- Docker image: ~6MB (FROM scratch with musl binary)
- Verify static linking: `ldd target/x86_64-unknown-linux-musl/release/twitch-1337` (should show "statically linked")

### Configuration
- Copy `config.toml.example` to `config.toml` for local dev
- Never commit `config.toml` to git with real credentials
- Get OAuth credentials from Twitch app at https://dev.twitch.tv/console
- All config in `config.toml` - no env vars needed
- Edit config.toml while running to add/modify/remove schedules (auto-reloads)

### Schedule Configuration

Add `[[schedules]]` sections to config.toml:

```toml
[[schedules]]
name = "hydration"
message = "Stay hydrated! DinkDonk"
interval = "00:30"  # Every 30 minutes
enabled = true

[[schedules]]
name = "stream-reminder"
message = "Don't forget to follow!"
interval = "01:00"  # Every hour
start_date = "2025-01-01T00:00:00"  # Optional: start date
end_date = "2025-12-31T23:59:59"    # Optional: end date
active_time_start = "18:00"         # Optional: only during evening
active_time_end = "23:00"
enabled = true
```

**Schedule Fields:**
- `name` (required) - Unique ID
- `message` (required) - Text post chat
- `interval` (required) - Frequency "hh:mm"
- `start_date` (optional) - ISO 8601 datetime (YYYY-MM-DDTHH:MM:SS)
- `end_date` (optional) - ISO 8601 datetime
- `active_time_start` (optional) - Daily start HH:MM
- `active_time_end` (optional) - Daily end HH:MM
- `enabled` (optional, default: true) - false = disable

**Time Windows:**
- If active_time_start/end empty: Posts 24/7 at interval
- If set: Only posts during daily time window (Europe/Berlin)
- Handles midnight-spanning windows (e.g., "22:00" to "02:00")

**Troubleshooting:**
- **"Invalid interval format"**: Use "hh:mm" (e.g., "01:00" = 1h, "00:30" = 30min)
- **"Invalid datetime format"**: Use ISO 8601 (YYYY-MM-DDTHH:MM:SS)
- **Schedule not posting**: Check `enabled = true` + time window settings
- **Config not reloading**: Wait 2+ seconds after saving (debounce delay)
- Use `?error` not `%error` when logging errors to include backtrace