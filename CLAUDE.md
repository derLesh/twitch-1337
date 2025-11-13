# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Rust-based Twitch IRC bot that monitors a channel for messages containing "1337" sent at exactly 13:37 Berlin time. The bot tracks unique users and posts daily statistics at 13:38. Designed for resource efficiency - IRC connection only active 2.5 minutes per day (13:36-13:38:30).

## Build and Run Commands

### Using Just (Recommended)

The project includes a Justfile for common tasks:

```bash
# Docker operations
just build              # Build Docker image as chronophylos/twitch-1337:latest
just build-no-cache     # Force full rebuild without cache
just run                # Run container in background (requires .env file)
just run-test           # Run container interactively for testing
just up                 # Build and run in one command
just reload             # Stop, remove, rebuild, and run (fresh start)
just logs               # Follow container logs
just logs-tail          # Show last 50 log lines
just stop               # Stop the container
just clean              # Stop and remove container
just restart            # Restart the container
just push               # Push to Docker Hub
just pull               # Pull from Docker Hub

# Local development
just build-local        # Build Rust binary (glibc, dynamically linked)
just build-musl         # Build static musl binary (no dependencies)
just run-local          # Run with cargo
just lint               # Run clippy
just fmt                # Format code
just fmt-check          # Check formatting
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
- `TWITCH_USERNAME` - Bot username for posting stats
- `TWITCH_ACCESS_TOKEN` - OAuth access token
- `TWITCH_REFRESH_TOKEN` - OAuth refresh token
- `TWITCH_CLIENT_ID` - Twitch application client ID
- `TWITCH_CLIENT_SECRET` - Twitch application client secret

**Optional:**
- `TWITCH_CHANNEL` - Channel to monitor (default: "REDACTED_CHANNEL")
- `RUST_LOG` - Logging level (default: "info", options: trace, debug, info, warn, error)

## Data Persistence

The bot persists user participation history to `/data/history.jsonl` (JSONL format - one JSON object per line).

**File Format:**
```jsonl
{"date":"2025-10-29","users":["user1","user2","user3"]}
{"date":"2025-10-30","users":["user4"]}
```

**Each entry contains:**
- `date`: Date in YYYY-MM-DD format (Berlin timezone)
- `users`: Sorted array of usernames who said "1337" at 13:37

**Volume Mount (Required):**
- The `/data` directory must be mounted as a volume for persistence
- Example: `-v $(pwd)/data:/data` or `-v /path/to/data:/data`
- The `just run` and `just run-test` commands automatically create `./data` and mount it
- The bot will fail if it cannot write to the history file (ensures data integrity)

**File Operations:**
- Write-only: Bot appends new entries daily, never reads historical data
- Atomic writes with flush and sync to ensure data durability
- Entries are written after stats are posted (13:38)
- Directory and file are created automatically if they don't exist

## Architecture

### Main Application Flow

The application runs a single daily task that handles the complete flow:

1. **Daily Session** (13:36-13:38:30):
   - Connects to Twitch IRC at 13:36 (authenticated)
   - Spawns async subtask to monitor incoming messages
   - Filters for PRIVMSG at exactly 13:37 containing "1337"
   - Tracks unique usernames in session-scoped `HashSet`
   - At 13:38, posts stats message to channel
   - Waits 30 seconds (until 13:38:30)
   - Disconnects and prepares for next day

**Benefits of unified session:**
- Simpler code - single connection, single task
- No state synchronization between separate tasks
- Clearer control flow and error handling
- Single authentication instead of anonymous + authenticated

### Key Dependencies

**Runtime & Async:**
- `tokio` - Async runtime (features: macros, rt-multi-thread, time, signal, fs only)

**IRC & Networking:**
- `twitch-irc` - Twitch IRC client (feature: transport-tcp-rustls-webpki-roots only)

**Time Handling:**
- `chrono` - Date/time operations
- `chrono-tz` - Timezone handling for Europe/Berlin (filter-by-regex feature)
  - Only compiles Berlin timezone data via `.cargo/config.toml`

**Error Handling & Logging:**
- `color-eyre` - Rich error messages with context
- `tracing` - Structured logging
- `tracing-subscriber` - Log formatting and filtering

**Security:**
- `secrecy` - Protects OAuth token in memory (prevents accidental logging)

**Serialization:**
- `serde` - Serialization framework (derive feature)
- `serde_json` - JSON serialization for JSONL persistence

### State Management

- `total_users`: `Arc<Mutex<HashSet<String>>>` - Tracks unique usernames who said "1337" at 13:37
- Session-scoped: created fresh for each daily run, automatically cleared when session ends
- Shared between main task and monitoring subtask via tokio::sync::Mutex
- Maximum capacity: 10,000 users (prevents unbounded memory growth)

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

### Functions

**`run_daily_session() -> Result<()>`**
- Creates authenticated IRC connection
- Spawns monitoring subtask to process incoming messages
- Waits until 13:38 to post stats
- Posts stats message with user count
- Writes participation history to JSONL file
- Waits 30 seconds before disconnecting
- Returns errors via eyre for proper handling

**`append_to_history(entry: HistoryEntry) -> Result<()>`**
- Creates `/data` directory if it doesn't exist
- Opens history file in append mode (creates if missing)
- Serializes entry to JSON and writes line + newline
- Flushes and syncs to ensure data is written to disk
- Returns error if any file operation fails (fail-fast for data integrity)

**`sleep_until_daily_time(hour, minute) -> ()`**
- Calculates next occurrence of specified Berlin time
- Sleeps until that time using tokio::time::sleep
- Accounts for DST transitions
- Logs next scheduled run time

**`main() -> Result<()>`**
- Initializes error handling (color-eyre)
- Initializes logging (tracing-subscriber)
- Validates environment variables at startup
- Spawns single task that loops daily
- Handles Ctrl+C shutdown gracefully

### Types

**`HistoryEntry` (struct)**
- `date: String` - Date in YYYY-MM-DD format
- `users: Vec<String>` - Sorted list of participating usernames
- Implements `Serialize` for JSON encoding

### Static Configuration

- `CHANNEL_LOGIN: LazyLock<String>` - Channel name from env or default
- `TWITCH_USERNAME: LazyLock<String>` - Required env var
- `TWITCH_ACCESS_TOKEN: LazyLock<SecretString>` - Required env var, wrapped for security
- `TWITCH_REFRESH_TOKEN: LazyLock<SecretString>` - Required env var, wrapped for security
- `TWITCH_CLIENT_ID: LazyLock<String>` - Required env var
- `TWITCH_CLIENT_SECRET: LazyLock<SecretString>` - Required env var, wrapped for security
- `HISTORY_FILE: &str` - Path to history file (`/data/history.jsonl`)

## Important Notes

- **Resource Efficient**: IRC connection only active 2.5 minutes/day (13:36-13:38:30)
- **Secure**: OAuth tokens and client secret wrapped in `SecretString`, won't appear in debug output
- **Error Handling**: All failures logged with context, session retried next day on error
- **Timezone**: All operations use `Europe/Berlin` timezone
- **Message Filter**: Only messages containing "1337" at exactly 13:37:xx are counted
- **Deduplication**: Same user counted only once per day via HashSet
- **Binary Size**: Optimized with minimal tokio features and single timezone
- **Logging**: Structured logs with tracing, configurable via `RUST_LOG`
- **Simplified Architecture**: Single task, single connection, no state synchronization
- **Data Persistence**: User participation history stored in JSONL format at `/data/history.jsonl`
- **Data Integrity**: Bot fails if history file cannot be written (ensures no data loss)

## Daily Timeline

```
13:36:00 → Bot connects to IRC (authenticated), starts monitoring
13:37:00-13:37:59 → Messages containing "1337" counted (unique users)
13:38:00 → Stats posted to channel
13:38:30 → Bot disconnects, waits until next day
```

## Development Tips

### General
- Use `RUST_LOG=debug` to see all IRC messages
- Session catches errors and logs them - check logs if stats don't post
- HashSet is session-scoped - automatically cleared when session ends (~2.5 minutes)
- Single authenticated connection used for both monitoring and posting
- All `.unwrap()` calls replaced with proper error handling except lazy static initialization

### Docker Development
- Use `just build` for quick Docker builds with caching
- Use `just run-test` for interactive testing (see logs immediately)
- Use `just logs` to follow logs from running container
- cargo-chef caches dependencies - only rebuilds app when source changes
- Build verification step ensures binary is statically linked

### Binary Size
- Standard build: ~6.2MB (glibc, dynamically linked)
- Musl build: ~6.0MB (static, no dependencies)
- Docker image: ~6MB (FROM scratch with musl binary)
- Verify static linking: `ldd target/x86_64-unknown-linux-musl/release/twitch-1337` (should show "statically linked")

### Testing Minimal Deployments
- Docker scratch: Already configured in Dockerfile
- Alpine: `docker run -v $(pwd)/target/x86_64-unknown-linux-musl/release/twitch-1337:/bot alpine:latest /bot`
- Busybox: Same as Alpine but with busybox image

### Environment Configuration
- Copy `.env.example` to `.env` for local development
- Never commit `.env` to git (already in .gitignore)
- Get OAuth credentials from your Twitch application at https://dev.twitch.tv/console
