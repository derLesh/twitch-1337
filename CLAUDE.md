# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Rust-based Twitch IRC bot with multiple features:
1. **1337 Tracker**: Monitors for "1337"/"DANKIES" messages at 13:37 Berlin time, posts stats at 13:38
2. **Ping System**: Local file-based ping commands with admin management, user self-service, and dynamic triggering
3. **Scheduled Messages**: Dynamic message scheduling via config.toml with file watching
4. **Latency Monitor**: Measures IRC latency via PING/PONG every 5 minutes, auto-adjusts timing offsets

Uses a persistent IRC connection with broadcast-based message routing to multiple handlers.

## Build and Run Commands

### Using Just (Recommended)

The project includes a Justfile for deployment tasks:

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

# Lint with clippy
cargo clippy

# Run tests (when added)
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

The bot is configured via a `config.toml` file. To get started:

```bash
cp config.toml.example config.toml
# Edit config.toml with your credentials
cargo run
```

### Configuration File Structure

The config.toml file has the following sections:

**[twitch]** - Twitch IRC connection and authentication
- `channel` - Channel to monitor (without # prefix)
- `username` - Bot's Twitch username
- `refresh_token` - OAuth refresh token (automatically refreshed and persisted to `token.ron`)
- `client_id` - Twitch application client ID
- `client_secret` - Twitch application client secret
- `expected_latency` - Initial seed for IRC latency estimate in milliseconds (optional, default: 100). Auto-measured via PING/PONG.
- `hidden_admins` - (optional) Vec of Twitch user IDs granted admin access to ping commands

**[pings]** - Ping system configuration
- `default_cooldown` - Default cooldown between ping triggers in seconds (optional, default: 300)

**[[schedules]]** (optional, repeatable) - Scheduled messages
- `name` - Unique identifier for the schedule
- `message` - Text to post in chat
- `interval` - Posting frequency in "hh:mm" format (e.g., "01:00" for 1 hour)
- `start_date` - (optional) When to start posting (ISO 8601: YYYY-MM-DDTHH:MM:SS)
- `end_date` - (optional) When to stop posting
- `active_time_start` - (optional) Daily start time in HH:MM format
- `active_time_end` - (optional) Daily end time in HH:MM format
- `enabled` - (optional, default: true) Set to false to disable without deleting

See `config.toml.example` for a complete annotated example.

### Docker Deployment

For Docker deployments, mount config.toml as a volume:

```bash
docker run -d \
  --name twitch-1337 \
  -v ./config.toml:/app/config.toml:ro \
  chronophylos/twitch-1337:latest
```

Or with Docker Compose:

```yaml
services:
  twitch-1337:
    image: chronophylos/twitch-1337:latest
    restart: unless-stopped
    volumes:
      - ./config.toml:/app/config.toml:ro
```

## Token Storage

The bot persists refreshed OAuth tokens to `./token.ron` (Rust Object Notation format):
- Automatically saves updated tokens when they're refreshed by twitch-irc
- Falls back to refresh token from `config.toml` on first run if file doesn't exist
- Eliminates need to manually update tokens when they expire
- Uses `FileBasedTokenStorage` implementing the `TokenStorage` trait

## Architecture

### Persistent Connection with Broadcast-Based Message Routing

The bot maintains a **single persistent IRC connection** and uses a **broadcast channel** to distribute messages to multiple independent handlers:

1. **Setup and Connection** (startup):
   - Creates authenticated IRC client with refreshing token credentials
   - Connects to Twitch IRC and joins configured channel
   - Verifies authentication by waiting for GlobalUserState message (30s timeout)
   - Creates broadcast channel (capacity: 100 messages) for message distribution

2. **Message Router Task** (continuous):
   - Reads from twitch-irc's UnboundedReceiver
   - Broadcasts all ServerMessages to subscribed handlers
   - Runs until connection closes

3. **Handler Tasks** (parallel, continuous):
   - **Config Watcher Service**: Watches config.toml for changes (2s debounce)
   - **Scheduled Message Handler**: Posts messages based on config.toml schedules (if configured)
   - **1337 Handler**: Daily scheduled monitoring (13:36-13:38)
   - **Generic Command Handler**: Processes `!ping` admin/user commands and dynamic `!<name>` ping triggers 24/7
   - **Latency Monitor**: Measures IRC latency via PING/PONG every 5 minutes
   - Each handler runs independently
   - Handlers filter for relevant messages and act accordingly

4. **Graceful Shutdown**:
   - Main task waits for Ctrl+C or any handler exit
   - All tasks run in tokio::select! for coordinated shutdown

**Benefits of persistent connection + broadcast:**
- Single connection overhead (no repeated auth)
- Handlers are independent and can be added/removed easily
- No shared state between handlers (loose coupling)
- Broadcast channel handles backpressure (lagging handlers warned)
- Clean separation of concerns

### Key Dependencies

**Runtime & Async:**
- `tokio` - Async runtime (features: macros, rt-multi-thread, time, signal, fs)

**IRC & Networking:**
- `twitch-irc` - Twitch IRC client (features: refreshing-token-rustls-webpki-roots, transport-tcp-rustls-webpki-roots)
- `reqwest` - HTTP client for OpenRouter and aviation APIs (features: json, rustls-tls-webpki-roots)

**File Watching:**
- `notify-debouncer-mini` - File system watcher with debouncing for config reload

**Time Handling:**
- `chrono` - Date/time operations
- `chrono-tz` - Timezone handling for Europe/Berlin (filter-by-regex feature)
  - Only compiles Berlin timezone data via `.cargo/config.toml`

**Error Handling & Logging:**
- `color-eyre` / `eyre` - Rich error messages with context
- `tracing` - Structured logging
- `tracing-subscriber` - Log formatting and filtering (env-filter feature)

**Security:**
- `secrecy` - Protects OAuth tokens and secrets in memory (prevents accidental logging)

**Serialization:**
- `serde` - Serialization framework (derive feature)
- `serde_json` - JSON serialization for API interactions
- `ron` - Rust Object Notation for token and ping storage
- `toml` - Configuration file parsing

**Other:**
- `rand` - For randomizing bot response messages
- `async-trait` - For TokenStorage and Command trait implementations

### State Management

**1337 Handler State:**
- `total_users`: `Arc<Mutex<HashSet<String>>>` - Tracks unique usernames who said "1337"/"DANKIES" at 13:37
- Created fresh each day at 13:36, discarded after stats posted at 13:38
- Shared between handler task and monitoring subtask via tokio::sync::Mutex
- Maximum capacity: 10,000 users (prevents unbounded memory growth)

**Scheduled Messages State:**
- `schedule_cache`: `Arc<RwLock<ScheduleCache>>` - Shared cache of schedules loaded from config.toml
- Contains vector of schedules, last update timestamp, and version number
- Updated when config.toml changes (file watcher with 2s debounce)
- Version increments trigger task manager to spawn/stop message tasks

**Ping System State:**
- `ping_manager`: `Arc<RwLock<PingManager>>` - Manages ping definitions, membership, and cooldowns
- Persisted to `pings.ron` via atomic write+rename
- Cooldown tracking via in-memory `HashMap<String, Instant>` (not persisted, resets on restart)
- Shared across `PingAdminCommand` and `PingTriggerCommand`

**Persistent State:**
- OAuth tokens in `token.ron`
- Ping definitions and membership in `pings.ron`

**No Persistent State:**
- Schedules loaded from config.toml on startup and file changes

### Configuration Files

- `.cargo/config.toml` - Sets `CHRONO_TZ_TIMEZONE_FILTER = "Europe/Berlin"` to reduce binary size
- `Cargo.toml` - Minimal feature flags for smaller binaries and faster compilation
- `Dockerfile` - Multi-stage build with cargo-chef optimization, FROM scratch final image
- `Justfile` - Task runner for common development and Docker operations
- `.dockerignore` - Excludes unnecessary files from Docker build context
- `config.toml.example` - Template configuration file with all available options

## System Dependencies & Deployment

### Binary Types

**Standard build (default target: x86_64-unknown-linux-gnu):**
- Dynamically linked against glibc
- Requires: `libc.so.6`, `libm.so.6`, `libgcc_s.so.1`, `ld-linux-x86-64.so.2`
- Size: ~6.2MB
- Works on most modern Linux distributions (glibc 2.31+)

**Static musl build (target: x86_64-unknown-linux-musl):**
- Fully statically linked, zero runtime dependencies
- Size: ~6.0MB
- Works on Alpine, busybox, or any Linux kernel 4.4.0+
- **Recommended for minimal/container deployments**
- Build with: `cargo build --release --target x86_64-unknown-linux-musl`

### Docker Deployment

The project uses a multi-stage Dockerfile with cargo-chef for optimal caching:

**Build stages:**
1. **Planner** - Analyzes Cargo.toml and generates dependency recipe
2. **Cacher** - Builds dependencies only (cached until Cargo.toml changes)
3. **Builder** - Builds application (only rebuilds when source changes)
4. **Runtime** - `FROM scratch` with just the static musl binary

**Build base image:** `rust:1.89-bookworm` (Debian-based)
- Installs `musl-tools` package for cross-compilation
- Adds `x86_64-unknown-linux-musl` target via rustup
- Necessary because `ring` crate (used by rustls) requires C compiler for musl

**Image characteristics:**
- Runtime base: `FROM scratch` (no OS, no shell, no utilities)
- Size: ~6.3MB (just the static binary)
- Security: Minimal attack surface
- Dependencies: None - static binary runs directly on kernel
- Build caching: ~2-3 seconds for rebuilds when only source changes

**Why this works:**
- `rustls` instead of OpenSSL (pure Rust TLS, minimal C dependencies)
- musl-tools provides `musl-gcc` for compiling C components statically
- All dependencies are statically linked into the final binary
- Minimal tokio features reduce binary bloat

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
- Both work without any additional packages

## Code Structure

### Main Entry Point

**`main() -> Result<()>` (src/main.rs)**
- Initializes error handling (color-eyre) and logging (tracing-subscriber)
- Loads and validates configuration from `config.toml`
- Calls `setup_and_verify_twitch_client()` to establish authenticated connection
- Creates broadcast channel with 100-message capacity
- Spawns handler tasks based on configuration
- Uses `tokio::select!` to wait for Ctrl+C or any task exit

### Connection Management

**`setup_and_verify_twitch_client() -> Result<(UnboundedReceiver, Client)>`**
- Creates IRC client with `RefreshingLoginCredentials<FileBasedTokenStorage>`
- Connects to Twitch IRC and joins configured channel
- Waits for `GlobalUserState` message to verify authentication (30s timeout)
- Returns verified client and incoming message receiver
- Detects and reports authentication failures with helpful error messages

**`setup_twitch_client() -> (UnboundedReceiver, Client)`**
- Creates `RefreshingLoginCredentials` with client ID, secret, and token storage
- Builds `ClientConfig` and `TwitchIRCClient`
- Returns receiver and client for connection setup

### Message Distribution

**`run_message_router(incoming_messages, broadcast_tx)`**
- Reads from twitch-irc's UnboundedReceiver
- Broadcasts all ServerMessages to subscribed handlers
- Exits when connection closes

### Handler: 1337 Tracker

**`run_1337_handler(broadcast_tx, client)`**
- Infinite loop: waits until 13:36 Berlin time each day
- Creates fresh `HashSet` for today's users
- Spawns `monitor_1337_messages()` subtask
- At 13:36:30: posts "PausersHype" reminder
- At 13:38: generates and posts stats message
- Aborts monitor subtask and repeats next day

**`monitor_1337_messages(broadcast_rx, total_users)`**
- Filters for PRIVMSG messages at exactly 13:37:xx Berlin time
- Checks if message contains "1337" or "DANKIES" via `is_valid_1337_message()`
- Ignores known bots: "supibot", "potatbotat"
- Inserts unique usernames into shared HashSet (max 10,000)

**`generate_stats_message(count, user_list) -> String`**
- Returns contextual German message based on count:
  - 0: "Erm" or "fuh"
  - 1: "@user zumindest einer..."
  - 2-3: Special handling for specific users
  - 4: "3.6, nicht gut, nicht dramatisch"
  - 5-7: "gute Auslese"
  - 8+: "insane quota Pag"
- Uses `one_of()` to randomly select from message variants

### Handler: Latency Monitor

**`run_latency_handler(client, broadcast_tx, latency)`**
- Runs in infinite loop, sleeps `LATENCY_PING_INTERVAL` (5 min) between cycles
- Sends `PING` with timestamp nonce via `irc!["PING", nonce]`
- Subscribes to broadcast channel before sending (avoids race)
- Matches `ServerMessage::Pong` where `source.params[1]` equals nonce
- Computes one-way latency (RTT/2), updates EMA with `LATENCY_EMA_ALPHA` (0.2)
- Stores rounded EMA in shared `Arc<AtomicU32>` with `Relaxed` ordering
- Handles: PING failure (warn, skip), PONG timeout (warn, skip), channel lagged (warn, continue)

### Handler: Generic Commands

**`run_generic_command_handler(broadcast_tx, client, ping_manager, hidden_admin_ids, default_cooldown)`**
- Creates `CommandDispatcher` with registered commands (`PingAdminCommand`, `PingTriggerCommand`, etc.)
- Subscribes to broadcast channel
- Dispatches PRIVMSG messages to matching commands via `CommandDispatcher`

**`CommandDispatcher`**
- Holds `Vec<Box<dyn Command>>` of registered commands
- On each PRIVMSG, extracts first word and finds matching command via `Command::matches()`
- Builds `CommandContext` with privmsg, client, and args, then calls `command.execute(ctx)`

### Ping Admin Command (`!ping`)

**`PingAdminCommand`**
- Requires broadcaster/moderator badge or `hidden_admins` user ID for admin subcommands
- Admin subcommands:
  - `!ping create <name> <template>` - Create new ping with template text
  - `!ping delete <name>` - Delete a ping
  - `!ping add <name> <user>` - Add user to ping membership
  - `!ping remove <name> <user>` - Remove user from ping membership
- User subcommands (open to all):
  - `!ping join <name>` - Subscribe yourself to a ping
  - `!ping leave <name>` - Unsubscribe yourself from a ping
  - `!ping list` - List your active ping memberships

### Ping Trigger Command (`!<name>`)

**`PingTriggerCommand`**
- Overrides `Command::matches()` to dynamically match any `!<name>` where `<name>` is a registered ping
- Only members of the ping can trigger it
- Checks cooldown (per-ping `cooldown` field, or global `default_cooldown` from config)
- Renders template with `{mentions}` (space-separated @user list) and `{sender}` (triggering user)
- Silent on: non-member, cooldown active, empty mentions list

### Handler: Scheduled Messages (Config-Based)

**Only runs if schedules are configured** (the `[[schedules]]` sections in `config.toml`).

**`run_config_watcher_service(cache)`**
- Uses `notify-debouncer-mini` to watch config.toml for changes
- 2-second debounce to avoid rapid reloads
- Reloads and validates config on change
- Updates cache (increments version) on successful reload
- Keeps existing schedules if reload fails

**`run_scheduled_message_handler(client, cache)`**
- Monitors cache for version changes every 30 seconds
- Spawns new tasks for added schedules
- Stops tasks for removed schedules
- Each schedule runs in independent task
- Dynamic task management without bot restart

**`run_schedule_task(schedule, client, cache)`**
- Runs single schedule in loop
- Sleeps for configured interval between posts
- Checks if schedule still exists in cache before each post
- Validates schedule is active (respects date range and time window)
- Posts message to Twitch chat
- Exits gracefully if schedule removed from cache

**`load_schedules_from_config(config) -> Vec<Schedule>`**
- Iterates through `config.schedules` array
- Skips disabled schedules
- Converts `ScheduleConfig` to `database::Schedule`
- Validates each schedule
- Returns vector of validated schedules

**`reload_schedules_from_config() -> Option<Vec<Schedule>>`**
- Reads config.toml from disk (sync, called from file watcher)
- Parses and validates configuration
- Returns None on error (keeps existing schedules)

### Database Module

**`database::Schedule`**
- Stores schedule configuration
- Fields: name, start_date, end_date, active_time_start, active_time_end, interval, message
- Methods:
  - `is_active(now)`: Checks if schedule active based on date range and time window
  - `parse_interval(s)`: Parses "hh:mm" format or legacy "30m"/"1h"/"2h30m" to TimeDelta
  - `validate()`: Validates required fields and logical consistency
- Derives: Debug, Clone, Deserialize, Serialize

**`database::ScheduleCache`**
- Container for loaded schedules with metadata
- Fields: schedules (Vec<Schedule>), last_updated, version
- Methods:
  - `new()`: Creates empty cache
  - `update(schedules)`: Updates schedules, increments version
- Version number enables change detection for task manager

### Ping Module

**`ping::Ping`**
- A single ping definition
- Fields: name, template, members (Vec<String>), cooldown (Option<u64>), created_by

**`ping::PingStore`**
- Top-level container serialized to/from `pings.ron`
- Fields: pings (HashMap<String, Ping>)

**`ping::PingManager`**
- Manages ping state and persistence, shared as `Arc<RwLock<PingManager>>`
- Fields: store (PingStore), last_triggered (HashMap<String, Instant>), path (PathBuf)
- Methods:
  - `load() -> Result<Self>`: Loads from `pings.ron`, creates empty store if file missing
  - `save() -> Result<()>`: Atomic write via tmp file + rename
  - `create_ping(name, template, created_by, cooldown) -> Result<()>`: Creates new ping
  - `delete_ping(name) -> Result<()>`: Deletes a ping
  - `add_member(ping_name, username) -> Result<()>`: Adds user to ping
  - `remove_member(ping_name, username) -> Result<()>`: Removes user from ping
  - `ping_exists(name) -> bool`: Checks if a ping exists
  - `is_member(ping_name, username) -> bool`: Checks membership
  - `list_pings_for_user(username) -> Vec<&str>`: Lists pings user belongs to
  - `check_cooldown(ping_name, default_cooldown) -> bool`: Returns true if cooldown expired
  - `record_trigger(ping_name)`: Records trigger timestamp for cooldown
  - `render_template(ping_name, sender) -> Option<String>`: Renders template with `{mentions}` and `{sender}`

### Token Storage

**`FileBasedTokenStorage`**
- Implements `TokenStorage` trait for twitch-irc
- Stores tokens in `./token.ron` file
- `load_token()`: Reads from file, falls back to refresh token from config.toml on first run
- `update_token()`: Writes updated tokens to file (called automatically by twitch-irc)

### Configuration Types

**`Configuration`**
- Main configuration struct loaded from config.toml
- Contains: `twitch`, `pings`, `schedules` (Vec<ScheduleConfig>), optionally `openrouter`
- `validate()` method ensures all required fields are present and valid

**`TwitchConfiguration`**
- `channel`, `username` - Twitch channel and bot username
- `refresh_token`, `client_id`, `client_secret` - OAuth credentials (SecretString)
- `expected_latency` - Initial latency seed in milliseconds (optional, default: 100, auto-measured via PING/PONG)
- `hidden_admins` - Vec of Twitch user IDs with admin access to ping commands

**`PingsConfig`**
- `default_cooldown` - Default cooldown between ping triggers in seconds (default: 300)

**`ScheduleConfig`**
- Configuration for a scheduled message in config.toml
- `name` - Unique identifier
- `message` - Text to post
- `interval` - Posting frequency ("hh:mm" format)
- `start_date`, `end_date` - Optional date range (ISO 8601)
- `active_time_start`, `active_time_end` - Optional daily time window (HH:MM)
- `enabled` - Whether schedule is active (default: true)

### Constants

- `TARGET_HOUR: u32 = 13` - Hour for 1337 tracking
- `TARGET_MINUTE: u32 = 37` - Minute for 1337 tracking
- `MAX_USERS: usize = 10_000` - Maximum tracked users
- `CONFIG_PATH: &str = "./config.toml"` - Configuration file path
- `PINGS_PATH: &str = "./pings.ron"` - Ping storage file path
- `LATENCY_PING_INTERVAL: Duration = 300s` - Time between PING measurements
- `LATENCY_PING_TIMEOUT: Duration = 10s` - Max wait for PONG response
- `LATENCY_EMA_ALPHA: f64 = 0.2` - EMA smoothing factor
- `LATENCY_LOG_THRESHOLD: u32 = 10` - Minimum EMA change (ms) for info log

## Important Notes

- **Persistent Connection**: Single IRC connection runs 24/7, not ephemeral sessions
- **Secure**: OAuth tokens and secrets wrapped in `SecretString`, won't appear in debug output
- **Error Handling**: All failures logged with context, handlers continue running on error
- **Timezone**: All time-based operations use `Europe/Berlin` timezone
- **1337 Tracking**: Messages containing "1337" or "DANKIES" at exactly 13:37:xx Berlin time
- **Bot Filtering**: Ignores messages from "supibot" and "potatbotat"
- **Deduplication**: Same user counted only once per day via HashSet (1337 handler)
- **Binary Size**: Optimized with minimal tokio features and single timezone (6MB)
- **Logging**: Structured logs with tracing, configurable via `RUST_LOG`
- **Broadcast Architecture**: Message router distributes to independent handlers
- **Ping Persistence**: Ping definitions and membership stored in `pings.ron` (atomic write+rename)
- **Token Refresh**: Tokens automatically refreshed and saved to `./token.ron`
- **Hot Reload**: Schedules reload automatically when config.toml changes (2s debounce)
- **Latency Auto-Measurement**: IRC latency measured via PING/PONG every 5 min, EMA (alpha=0.2) updates shared `AtomicU32` read by timing-sensitive handlers

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
           -> Only members can trigger, checks cooldown
           -> Renders template with {mentions} and {sender}
           -> Silent on non-member, cooldown, or empty mentions
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

### General
- The `twitch_irc::irc!` macro requires a standalone `use twitch_irc::irc;` — it cannot be added to the braced `use twitch_irc::{...}` block
- When doing request-response on the broadcast channel (e.g., PING/PONG), subscribe BEFORE sending to avoid race conditions
- Use `RUST_LOG=debug` to see all IRC messages and handler activity
- Use `RUST_LOG=trace` to see every ServerMessage received (very verbose)
- Handlers are independent - errors in one handler don't crash others
- Broadcast channel capacity: 100 messages (handlers warned if lagging)
- All handlers run in infinite loops - only exit on channel close or panic
- Configuration is loaded from `config.toml` at startup and reloaded on file changes

### Adding New Handlers
1. Create handler function: `async fn run_my_handler(broadcast_tx, client)`
2. Subscribe to broadcast: `let mut broadcast_rx = broadcast_tx.subscribe()`
3. Loop on `broadcast_rx.recv().await`, filter for relevant messages
4. Spawn handler in main(): `tokio::spawn(run_my_handler(broadcast_tx.clone(), client.clone()))`
5. Add to `tokio::select!` for coordinated shutdown

### Deployment Workflow
- Use `just deploy` to build locally with podman, push to remote docker host via SSH, and restart
- The Justfile assumes podman locally and docker on remote host
- Deployment target: `docker.homelab` SSH host
- Remote project directory: `twitch` (docker compose location)

### Binary Size
- Standard build: ~6.2MB (glibc, dynamically linked)
- Musl build: ~6.0MB (static, no dependencies)
- Docker image: ~6MB (FROM scratch with musl binary)
- Verify static linking: `ldd target/x86_64-unknown-linux-musl/release/twitch-1337` (should show "statically linked")

### Configuration
- Copy `config.toml.example` to `config.toml` for local development
- Never commit `config.toml` to git with real credentials
- Get OAuth credentials from your Twitch application at https://dev.twitch.tv/console
- All configuration is in `config.toml` - no environment variables needed
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
- `name` (required) - Unique identifier for the schedule
- `message` (required) - Text to post in chat
- `interval` (required) - Posting frequency in "hh:mm" format
- `start_date` (optional) - ISO 8601 datetime (YYYY-MM-DDTHH:MM:SS)
- `end_date` (optional) - ISO 8601 datetime
- `active_time_start` (optional) - Daily start time in HH:MM format
- `active_time_end` (optional) - Daily end time in HH:MM format
- `enabled` (optional, default: true) - Set to false to disable

**Time Windows:**
- If active_time_start/end are empty: Posts 24/7 at interval
- If set: Only posts during daily time window (Europe/Berlin)
- Handles midnight-spanning windows (e.g., "22:00" to "02:00")

**Troubleshooting:**
- **"Invalid interval format"**: Use "hh:mm" format (e.g., "01:00" for 1 hour, "00:30" for 30 minutes)
- **"Invalid datetime format"**: Use ISO 8601 format (YYYY-MM-DDTHH:MM:SS)
- **Schedule not posting**: Check `enabled = true` and time window settings
- **Config not reloading**: Wait 2+ seconds after saving (debounce delay)
- Use `?error` instead of `%error` when logging errors to include the backtrace
