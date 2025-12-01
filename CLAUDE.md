# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Rust-based Twitch IRC bot with multiple features:
1. **1337 Tracker**: Monitors for "1337"/"DANKIES" messages at 13:37 Berlin time, posts stats at 13:38
2. **Minecraft Responder**: Answers "WannMinecraft" queries with server status and countdown
3. **Ping Toggle**: Allows users to toggle their @mention in StreamElements ping commands
4. **Scheduled Messages**: Dynamic message scheduling via Google Sheets with 5-minute polling

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

# Run with environment variables
docker run -d \
  --name twitch-1337 \
  -e TWITCH_USERNAME=your_bot \
  -e TWITCH_ACCESS_TOKEN=your_access_token \
  -e TWITCH_REFRESH_TOKEN=your_refresh_token \
  -e TWITCH_CLIENT_ID=your_client_id \
  -e TWITCH_CLIENT_SECRET=your_client_secret \
  -e TWITCH_CHANNEL=REDACTED_CHANNEL \
  chronophylos/twitch-1337:latest

# Run with .env file
docker run -d --name twitch-1337 --env-file .env chronophylos/twitch-1337:latest

# View logs
docker logs -f twitch-1337
```

## Environment Variables

**Required:**
- `TWITCH_USERNAME` - Bot username for posting messages
- `TWITCH_ACCESS_TOKEN` - OAuth access token (with chat:read and chat:edit scopes)
- `TWITCH_REFRESH_TOKEN` - OAuth refresh token
- `TWITCH_CLIENT_ID` - Twitch application client ID
- `TWITCH_CLIENT_SECRET` - Twitch application client secret
- `STREAMELEMENTS_API_TOKEN` - StreamElements API token for command management

**Optional:**
- `TWITCH_CHANNEL` - Channel to monitor (default: "REDACTED_CHANNEL")
- `RUST_LOG` - Logging level (default: "info", options: trace, debug, info, warn, error)

**Google Sheets Configuration (Optional - for scheduled messages):**
- `GOOGLE_SHEETS_SPREADSHEET_ID` - Spreadsheet ID from the Google Sheets URL
- `GOOGLE_SHEETS_SHEET_NAME` - Sheet name within spreadsheet (default: "ScheduledMessages")
- `GOOGLE_SERVICE_ACCOUNT_PATH` - Path to service account JSON key file

If Google Sheets variables are not configured, scheduled messages feature will be disabled with a warning.

## Token Storage

The bot persists refreshed OAuth tokens to `./token.ron` (Rust Object Notation format):
- Automatically saves updated tokens when they're refreshed by twitch-irc
- Falls back to environment variables on first run if file doesn't exist
- Eliminates need to manually update tokens when they expire
- Uses `FileBasedTokenStorage` implementing the `TokenStorage` trait

## Schedule Cache

Scheduled messages are cached to `./schedule_cache.ron` for offline fallback:
- Automatically saved when schedules are fetched from Google Sheets
- Used as fallback if Google Sheets is unavailable
- Contains schedule data, last update timestamp, and version number
- Persists across bot restarts
- RON format for human-readable storage

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
   - **Schedule Loader Service**: Polls Google Sheets every 5 minutes (if configured)
   - **Scheduled Message Handler**: Posts messages based on Google Sheets schedules (if configured)
   - **1337 Handler**: Daily scheduled monitoring (13:36-13:38)
   - **Minecraft Handler**: Responds to "WannMinecraft" queries 24/7
   - **Generic Command Handler**: Processes `!toggle-ping` commands 24/7
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
- `reqwest` - HTTP client for StreamElements API (features: json, rustls-tls-webpki-roots)
- `google-sheets4` - Google Sheets API client (version 6.0+)
- `hyper` - HTTP client library (version 1.5)
- `hyper-util` - HTTP utilities (features: client, client-legacy, http1, tokio)
- `yup-oauth2` - OAuth2 authentication (version 12.1)

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
- `serde_json` - JSON serialization for StreamElements API
- `ron` - Rust Object Notation for token storage

**Other:**
- `regex` - For username matching in ping command
- `rand` - For randomizing bot response messages
- `async-trait` - For TokenStorage trait implementation

### State Management

**1337 Handler State:**
- `total_users`: `Arc<Mutex<HashSet<String>>>` - Tracks unique usernames who said "1337"/"DANKIES" at 13:37
- Created fresh each day at 13:36, discarded after stats posted at 13:38
- Shared between handler task and monitoring subtask via tokio::sync::Mutex
- Maximum capacity: 10,000 users (prevents unbounded memory growth)

**Scheduled Messages State:**
- `schedule_cache`: `Arc<RwLock<ScheduleCache>>` - Shared cache of schedules loaded from Google Sheets
- Contains vector of schedules, last update timestamp, and version number
- Updated every 5 minutes by loader service
- Version increments trigger task manager to spawn/stop message tasks
- Persisted to disk (`schedule_cache.ron`) for offline fallback

**Persistent State:**
- OAuth tokens in `token.ron`
- Schedule cache in `schedule_cache.ron`

**No Persistent State:**
- Minecraft schedule hardcoded in `get_session_times()` (first session: 2025-11-30)
- StreamElements commands fetched/updated via API on demand

### Configuration Files

- `.cargo/config.toml` - Sets `CHRONO_TZ_TIMEZONE_FILTER = "Europe/Berlin"` to reduce binary size
- `Cargo.toml` - Minimal feature flags for smaller binaries and faster compilation
- `Dockerfile` - Multi-stage build with cargo-chef optimization, FROM scratch final image
- `Justfile` - Task runner for common development and Docker operations
- `.dockerignore` - Excludes unnecessary files from Docker build context
- `.env.example` - Template for environment variables

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
    env_file: .env
```

**Systemd service (musl binary):**
```bash
# Copy musl binary
sudo cp target/x86_64-unknown-linux-musl/release/twitch-1337 /usr/local/bin/
# Create systemd service with environment variables
```

**Alpine/busybox:**
- Use musl binary or Docker image
- Both work without any additional packages

## Code Structure

### Main Entry Point

**`main() -> Result<()>` (src/main.rs:711-804)**
- Initializes error handling (color-eyre) and logging (tracing-subscriber)
- Validates all environment variables at startup (panics if missing)
- Calls `setup_and_verify_twitch_client()` to establish authenticated connection
- Creates broadcast channel with 100-message capacity
- Spawns 4 concurrent tasks: message router, 1337 handler, Minecraft handler, generic command handler
- Uses `tokio::select!` to wait for Ctrl+C or any task exit

### Connection Management

**`setup_and_verify_twitch_client() -> Result<(UnboundedReceiver, Client)>` (src/main.rs:829-893)**
- Creates IRC client with `RefreshingLoginCredentials<FileBasedTokenStorage>`
- Connects to Twitch IRC and joins configured channel
- Waits for `GlobalUserState` message to verify authentication (30s timeout)
- Returns verified client and incoming message receiver
- Detects and reports authentication failures with helpful error messages

**`setup_twitch_client() -> (UnboundedReceiver, Client)` (src/main.rs:806-818)**
- Creates `RefreshingLoginCredentials` with client ID, secret, and token storage
- Builds `ClientConfig` and `TwitchIRCClient`
- Returns receiver and client for connection setup

### Message Distribution

**`run_message_router(incoming_messages, broadcast_tx)` (src/main.rs:899-913)**
- Reads from twitch-irc's UnboundedReceiver
- Broadcasts all ServerMessages to subscribed handlers
- Exits when connection closes

### Handler: 1337 Tracker

**`run_1337_handler(broadcast_tx, client)` (src/main.rs:919-983)**
- Infinite loop: waits until 13:36 Berlin time each day
- Creates fresh `HashSet` for today's users
- Spawns `monitor_1337_messages()` subtask
- At 13:36:30: posts "PausersHype" reminder
- At 13:38: generates and posts stats message
- Aborts monitor subtask and repeats next day

**`monitor_1337_messages(broadcast_rx, total_users)` (src/main.rs:380-430)**
- Filters for PRIVMSG messages at exactly 13:37:xx Berlin time
- Checks if message contains "1337" or "DANKIES" via `is_valid_1337_message()`
- Ignores known bots: "supibot", "potatbotat"
- Inserts unique usernames into shared HashSet (max 10,000)

**`generate_stats_message(count, user_list) -> String` (src/main.rs:325-360)**
- Returns contextual German message based on count:
  - 0: "Erm" or "fuh"
  - 1: "@user zumindest einer..."
  - 2-3: Special handling for specific users
  - 4: "3.6, nicht gut, nicht dramatisch"
  - 5-7: "gute Auslese"
  - 8+: "insane quota Pag"
- Uses `one_of()` to randomly select from message variants

### Handler: Minecraft Responder

**`run_minecraft_handler(broadcast_tx, client)` (src/main.rs:989-1017)**
- Subscribes to broadcast channel
- Filters for PRIVMSG messages
- Calls `process_minecraft_message()` for each message

**`process_minecraft_message(privmsg, client) -> bool` (src/main.rs:593-642)**
- Checks if message contains "wannminecraft" or "wann minecraft"
- Ignores known bots
- Gets current Berlin time and checks `is_server_online()`
- If online: responds with server address
- If offline: calculates countdown via `get_next_session_start()` and `format_countdown()`
- Special handling for user "REDACTED_USER": hex-encoded responses only

**`get_session_times(date) -> Option<(DateTime, DateTime)>` (src/main.rs:441-478)**
- Returns Minecraft server online hours for a given date
- Schedule varies by weekday:
  - Mon-Thu: 18:00-23:00
  - Fri: 18:00-01:00 (next day)
  - Sat: 15:00-01:00 (next day)
  - Sun: 15:00-23:00
- First session: 2025-11-30

**`is_server_online(now) -> bool` (src/main.rs:483-513)**
- Checks if current Berlin time falls within session window
- Handles late-night sessions spanning midnight

**`get_next_session_start(now) -> DateTime` (src/main.rs:518-545)**
- Scans up to 30 days ahead for next session start
- Used for countdown calculations

**`format_countdown(duration) -> String` (src/main.rs:550-588)**
- Formats chrono::Duration as German text: "X Tage, Y Stunden, Z Minuten, W Sekunden"

### Handler: Generic Commands

**`run_generic_command_handler(broadcast_tx, client)` (src/main.rs:1026-1072)**
- Initializes `SEClient` (StreamElements API client)
- Subscribes to broadcast channel
- Dispatches commands to `handle_generic_commands()`

**`handle_generic_commands(privmsg, client, se_client) -> Result<()>` (src/main.rs:1082-1097)**
- Parses first word of message
- Routes to specialized handlers (currently only `toggle_ping_command`)

**`toggle_ping_command(privmsg, client, se_client, command_name) -> Result<()>` (src/main.rs:1126-1214)**
- Fetches all StreamElements commands with "pinger" keyword
- Finds command matching `command_name`
- Toggles user's @mention in command reply using regex
- Updates command via StreamElements API
- Responds with "Hab ich gemacht Okayge" on success
- Error responses: "Das kann ich nicht FDM" (no name), "Das finde ich nicht FDM" (not found)

### Handler: Scheduled Messages (Dynamic Google Sheets)

**Only runs if Google Sheets is configured** (`GOOGLE_SHEETS_SPREADSHEET_ID` and `GOOGLE_SERVICE_ACCOUNT_PATH` set).

**`run_schedule_loader_service(cache)` (src/main.rs:~1960)**
- Polls Google Sheets every 5 minutes for schedule updates
- Initial load on startup: Google Sheets → disk cache → empty cache (fallback chain)
- Saves fetched schedules to disk cache (`schedule_cache.ron`)
- Increments cache version on each update
- Logs consecutive failures (warns after 10 failures)
- Runs continuously in background

**`run_scheduled_message_handler(client, cache)` (src/main.rs:~1433)**
- Monitors cache for version changes every 30 seconds
- Spawns new tasks for added schedules
- Stops tasks for removed schedules
- Each schedule runs in independent task
- Dynamic task management without bot restart

**`run_schedule_task(schedule, client, cache)` (src/main.rs:~1361)**
- Runs single schedule in loop
- Sleeps for configured interval between posts
- Checks if schedule still exists in cache before each post
- Validates schedule is active (respects date range and time window)
- Posts message to Twitch chat
- Exits gracefully if schedule removed from cache

**`build_column_map(header_row) -> Result<HashMap<String, usize>>` (src/main.rs:~1914)**
- Builds mapping of column names to indices from header row
- Normalizes column names (lowercase, trimmed) for case-insensitive matching
- Validates that required columns exist
- Returns HashMap for flexible column ordering

**`fetch_schedules_from_sheets() -> Result<Vec<Schedule>>` (src/main.rs:~1945)**
- Authenticates with Google Sheets API using service account
- Fetches header row first to build column mapping
- Fetches data from configured spreadsheet and sheet
- Parses rows into `Schedule` objects using column names (not positions)
- Validates each schedule (interval >= 1 minute, valid date ranges, etc.)
- Skips invalid rows with error logging (includes row numbers)
- Returns vector of validated schedules

**`parse_schedule_row(row, row_num, column_map) -> Result<Schedule>` (src/main.rs:~2081)**
- Parses Google Sheets row into Schedule struct using column mapping
- Required columns (by name): Schedule Name, Message, Interval
- Optional columns: Start Date, End Date, Active Time Start, Active Time End, Enabled
- Supports flexible column ordering (uses header row to determine positions)
- Validates formats:
  - Dates: ISO 8601 (YYYY-MM-DDTHH:MM:SS), DD/MM/YYYY HH:MM, or DD/MM/YYYY
  - Times: HH:MM format
  - Intervals: "hh:mm" format (e.g., "01:30" for 1 hour 30 minutes) or legacy "30m"/"1h"/"2h30m"
- Skips disabled schedules (Enabled column = FALSE, NO, or 0)

**Google Sheets Format:**
The bot reads the header row to determine column positions, so columns can be in any order. Column names are case-insensitive.

Required columns:
- **Schedule Name** - Unique identifier for the schedule
- **Message** - Text to post in chat
- **Interval** - Posting frequency in "hh:mm" format (e.g., "01:00" for 1 hour, "00:30" for 30 minutes)

Optional columns:
- **Start Date** - When to start posting (formats: "DD/MM/YYYY", "DD/MM/YYYY HH:MM", or "YYYY-MM-DDTHH:MM:SS")
- **End Date** - When to stop posting (same formats as Start Date)
- **Active Time Start** - Daily start time in "HH:MM" format (e.g., "18:00")
- **Active Time End** - Daily end time in "HH:MM" format (e.g., "23:00")
- **Enabled** - Set to FALSE, NO, or 0 to disable a schedule without deleting it

Example header row:
```
Schedule Name | Start Date | End Date | Active Time Start | Active Time End | Interval | Enabled | Message
```

Example data row:
```
wichtel-reminder | 27/12/2025 | | 08:00 | 01:00 | 01:00 | TRUE | DinkDonk Don't forget your address!
```

**Time Windows:**
- If Active Time Start/End are empty: Posts 24/7 at interval
- If set: Only posts during daily time window (Europe/Berlin)
- Handles midnight-spanning windows (e.g., "22:00" to "02:00")

**Cache Files:**
- `save_cache_to_disk(cache) -> Result<()>`: Serializes cache to RON format
- `load_cache_from_disk() -> Result<ScheduleCache>`: Deserializes from disk
- Path: `./schedule_cache.ron`
- Contains schedules, last_updated, version

### Database Module

**`database::Schedule` (src/main.rs:~1537)**
- Stores schedule configuration from Google Sheets
- Fields: name, start_date, end_date, active_time_start, active_time_end, interval, message
- Methods:
  - `is_active(now)`: Checks if schedule active based on date range and time window
  - `parse_interval(s)`: Parses "30m"/"1h"/"2h30m" format to TimeDelta
  - `validate()`: Validates required fields and logical consistency
- Derives: Debug, Clone, Deserialize, Serialize

**`database::ScheduleCache` (src/main.rs:~1696)**
- Container for loaded schedules with metadata
- Fields: schedules (Vec<Schedule>), last_updated, version
- Methods:
  - `new()`: Creates empty cache
  - `update(schedules)`: Updates schedules, increments version
- Version number enables change detection for task manager

### StreamElements Module

**`streamelements::SEClient` (src/main.rs:94-168)**
- HTTP client for StreamElements API with Bearer auth
- `new(token) -> Result<Self>`: Creates client with auth headers
- `get_all_commands(channel_id) -> Result<Vec<Command>>`: Fetches all bot commands
- `update_command(channel_id, command) -> Result<()>`: Updates a command

**`streamelements::Command` (src/main.rs:42-64)**
- Represents a StreamElements bot command
- Fields: cooldown, aliases, keywords, enabled flags, reply text, etc.

### Token Storage

**`FileBasedTokenStorage` (src/main.rs:647-699)**
- Implements `TokenStorage` trait for twitch-irc
- Stores tokens in `./token.ron` file
- `load_token()`: Reads from file, falls back to environment variables on first run
- `update_token()`: Writes updated tokens to file (called automatically by twitch-irc)

### Utility Functions

**`calculate_next_occurrence(hour, minute) -> DateTime<Utc>` (src/main.rs:228-252)**
- Calculates next occurrence of daily Berlin time
- Handles DST transitions

**`wait_until_schedule(hour, minute)` (src/main.rs:255-273)**
- Sleeps until next occurrence of daily Berlin time
- Logs wait duration

**`sleep_until_hms(hour, minute, second, latency)` (src/main.rs:278-300)**
- Sleeps until specific time today (Berlin)
- Adjusts for expected latency (90ms)
- Used for precise timing within 1337 handler

**`is_ignored_bot(username) -> bool` (src/main.rs:305-307)**
- Returns true if username is "supibot" or "potatbotat"

**`is_valid_1337_message(message) -> bool` (src/main.rs:312-320)**
- Checks if PRIVMSG should count toward 1337 stats
- Filters bots and checks for "1337" or "DANKIES" keywords

**`encode_hex(input) -> String` (src/main.rs:365-367)**
- Converts string to hex representation
- Used for REDACTED_USER's special response encoding

**`one_of<T>(array) -> &T` (src/main.rs:372-374)**
- Returns random element from array using rand::rng()

### Types

**`AuthenticatedTwitchClient` (type alias, src/main.rs:174-175)**
- `TwitchIRCClient<SecureTCPTransport, RefreshingLoginCredentials<FileBasedTokenStorage>>`

**`streamelements::Command` (struct, src/main.rs:42-64)**
- Represents a StreamElements bot command
- Key fields: `id`, `command`, `reply`, `keywords`, `enabled`, `cooldown`

**`streamelements::CommandCooldown` (struct, src/main.rs:69-75)**
- `user: i64` - Per-user cooldown in seconds
- `global: i64` - Global cooldown in seconds

### Static Configuration (LazyLock)

- `APP_USER_AGENT: &str` - User agent for HTTP requests (src/main.rs:187)
- `CHANNEL_LOGIN: LazyLock<String>` - Channel name from env or "REDACTED_CHANNEL" (src/main.rs:189-191)
- `TWITCH_USERNAME: LazyLock<String>` - Required env var (src/main.rs:193-195)
- `TWITCH_ACCESS_TOKEN: LazyLock<SecretString>` - Required env var, wrapped for security (src/main.rs:197-201)
- `TWITCH_REFRESH_TOKEN: LazyLock<SecretString>` - Required env var, wrapped for security (src/main.rs:203-207)
- `TWITCH_CLIENT_ID: LazyLock<String>` - Required env var (src/main.rs:209-211)
- `TWITCH_CLIENT_SECRET: LazyLock<SecretString>` - Required env var, wrapped for security (src/main.rs:213-217)
- `STREAMELEMENTS_API_TOKEN: LazyLock<SecretString>` - Required env var, wrapped for security (src/main.rs:219-223)

### Constants

- `TARGET_HOUR: u32 = 13` - Hour for 1337 tracking (src/main.rs:177)
- `TARGET_MINUTE: u32 = 37` - Minute for 1337 tracking (src/main.rs:178)
- `MAX_USERS: usize = 10_000` - Maximum tracked users (src/main.rs:181)
- `EXPECTED_LATENCY: u32 = 90` - Twitch IRC latency in milliseconds (src/main.rs:185)

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
- **Token Refresh**: Tokens automatically refreshed and saved to `./token.ron`
- **Special Users**: REDACTED_USER gets hex-encoded responses for Minecraft queries

## Feature Timelines

### 1337 Tracker (Daily)
```
13:36:00 → Handler wakes, creates fresh HashSet, subscribes to broadcast
13:36:30 → Posts "PausersHype" reminder
13:37:00-13:37:59 → Monitoring subtask tracks "1337"/"DANKIES" messages (unique users)
13:38:00 → Posts stats message with contextual response
         → Aborts monitor subtask, waits for next day
```

### Minecraft Responder (24/7)
```
Continuous → Listens for "wannminecraft" / "wann minecraft"
           → Responds with server status or countdown to next session
           → Uses hardcoded schedule (Mon-Thu 18-23, Fri 18-01, Sat 15-01, Sun 15-23)
```

### Ping Toggle (24/7)
```
Continuous → Listens for "!toggle-ping <command>"
           → Fetches StreamElements commands, toggles user's @mention
           → Updates command via API, confirms success
```

### Scheduled Messages (Conditional - Only if Google Sheets configured)
```
Startup    → Try Google Sheets → disk cache → empty cache (fallback chain)
           → Spawn loader service and message handler
           → Spawn tasks for each active schedule

Every 5min → Poll Google Sheets for schedule updates
           → Update cache if successful
           → Save to disk (schedule_cache.ron)

Every 30s  → Task manager checks cache version
           → Spawn tasks for new schedules
           → Stop tasks for removed schedules

Per Task   → Sleep for schedule's interval
           → Check if schedule still in cache
           → Check if schedule is active (date range + time window)
           → Post message to chat
           → Repeat or exit if removed
```

**If Google Sheets NOT configured:**
```
Startup    → Log warning: "Google Sheets not configured. Scheduled messages disabled."
           → Do not spawn loader service or message handler
           → Bot continues with other handlers
```

## Development Tips

### General
- Use `RUST_LOG=debug` to see all IRC messages and handler activity
- Use `RUST_LOG=trace` to see every ServerMessage received (very verbose)
- Handlers are independent - errors in one handler don't crash others
- Broadcast channel capacity: 100 messages (handlers warned if lagging)
- All handlers run in infinite loops - only exit on channel close or panic
- LazyLock panics if environment variables are missing at startup

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

### Environment Configuration
- Copy `.env.example` to `.env` for local development
- Never commit `.env` to git (already in .gitignore)
- Get OAuth credentials from your Twitch application at https://dev.twitch.tv/console
- StreamElements API token from StreamElements dashboard

### Google Sheets Setup (Optional - for Scheduled Messages)

**1. Create Google Cloud Project:**
- Visit https://console.cloud.google.com/
- Create new project or select existing
- Enable Google Sheets API

**2. Create Service Account:**
- Navigate to "IAM & Admin" → "Service Accounts"
- Click "Create Service Account"
- Name it (e.g., "twitch-bot-scheduler")
- Click "Create and Continue"
- Skip role assignment (not needed for Sheets access)
- Click "Done"

**3. Generate JSON Key:**
- Click on created service account
- Go to "Keys" tab
- Click "Add Key" → "Create new key"
- Select "JSON" format
- Download and save securely (e.g., `/path/to/service-account.json`)

**4. Create Google Sheet:**
- Create new Google Sheet
- Name first sheet "ScheduledMessages" (or custom name)
- Add header row (columns can be in any order, names are case-insensitive):
  ```
  Schedule Name | Message | Interval | Start Date | End Date | Active Time Start | Active Time End | Enabled
  ```
  Required columns: Schedule Name, Message, Interval
  Optional columns: Start Date, End Date, Active Time Start, Active Time End, Enabled
- Share sheet with service account email (found in JSON key as `client_email`)
- Grant "Editor" permissions

**5. Configure Environment Variables:**
```bash
GOOGLE_SHEETS_SPREADSHEET_ID=<from URL: docs.google.com/spreadsheets/d/SPREADSHEET_ID/edit>
GOOGLE_SHEETS_SHEET_NAME=ScheduledMessages  # Optional, default is "ScheduledMessages"
GOOGLE_SERVICE_ACCOUNT_PATH=/path/to/service-account.json
```

**6. Test:**
- Add test schedule row
- Start bot
- Check logs for "Google Sheets configured, starting scheduled message system"
- Schedule should post after configured interval

**Example Schedule Row:**
```
Schedule Name: wichtel-reminder
Start Date: 27/12/2025
End Date: (empty)
Active Time Start: 08:00
Active Time End: 01:00
Interval: 01:00
Enabled: TRUE
Message: DinkDonk Don't forget your address!
```

**Troubleshooting:**
- **"Failed to read service account key"**: Check file path and permissions
- **"Failed to fetch data from Google Sheets"**: Verify spreadsheet ID and sheet name
- **"No data found"**: Check sheet has data rows (not just headers)
- **"Required column not found"**: Ensure header row has "Schedule Name", "Message", and "Interval" columns
- **"Invalid interval format"**: Use "hh:mm" format (e.g., "01:00" for 1 hour, "00:30" for 30 minutes)
- **"Invalid date format"**: Use DD/MM/YYYY, DD/MM/YYYY HH:MM, or YYYY-MM-DDTHH:MM:SS format
- use ?error instead of %error when logging errors to include the backtrace as well