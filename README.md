# twitch-1337

A minimal, resource-efficient Rust bot that monitors a Twitch channel for messages containing "1337" sent at exactly 13:37 Berlin time. The bot tracks unique users and posts daily statistics at 13:39.

## Features

- **Ultra Resource Efficient**: IRC connection only active 3 minutes per day (13:36-13:39)
- **Minimal Footprint**: 6MB statically-linked binary, runs on Alpine/busybox or FROM scratch
- **Secure**: OAuth tokens and client secret protected with `secrecy` crate, never logged
- **Timezone Aware**: All operations use Europe/Berlin timezone
- **Deduplication**: Tracks unique users per day via HashSet

## Quick Start

### Using Docker (Recommended)

```bash
# Build the image
just build

# Create environment file
cp .env.example .env
# Edit .env with your Twitch credentials

# Run the bot
just run

# View logs
just logs
```

### Using Cargo

```bash
# Set environment variables
export TWITCH_USERNAME=your_bot_username
export TWITCH_ACCESS_TOKEN=your_access_token_here
export TWITCH_REFRESH_TOKEN=your_refresh_token_here
export TWITCH_CLIENT_ID=your_client_id_here
export TWITCH_CLIENT_SECRET=your_client_secret_here
export TWITCH_CHANNEL=REDACTED_CHANNEL  # optional

# Run the bot
cargo run --release
```

## Environment Variables

**Required:**
- `TWITCH_USERNAME` - Bot username for posting stats
- `TWITCH_ACCESS_TOKEN` - OAuth access token
- `TWITCH_REFRESH_TOKEN` - OAuth refresh token
- `TWITCH_CLIENT_ID` - Twitch application client ID
- `TWITCH_CLIENT_SECRET` - Twitch application client secret
  - Get OAuth credentials from: https://dev.twitch.tv/console

**Optional:**
- `TWITCH_CHANNEL` - Channel to monitor (default: `REDACTED_CHANNEL`)
- `RUST_LOG` - Logging level (default: `info`, options: `trace`, `debug`, `info`, `warn`, `error`)

## Building

### Local Builds

```bash
# Standard build (dynamically linked, requires glibc)
cargo build --release

# Static build with musl (no dependencies, works everywhere)
cargo build --release --target x86_64-unknown-linux-musl

# Quick builds
just build-local      # Standard build
just build-musl       # Static musl build
```

### Docker Builds

The Dockerfile uses a multi-stage build with [cargo-chef](https://github.com/LukeMathWalker/cargo-chef) for optimal layer caching and a `FROM scratch` final image for minimal size.

```bash
# Build Docker image
docker build -t chronophylos/twitch-1337:latest .

# Or use just
just build
just build-no-cache  # Force full rebuild
```

**Image Details:**
- Base: `FROM scratch` (final image is just the ~6.3MB static binary)
- Build base: `rust:1.89-bookworm` with `musl-tools` for cross-compilation
- Build stages: planner → cacher → builder → runtime
- Dependencies cached separately for fast rebuilds (~2-3 seconds after initial build)

## Running

### Docker

```bash
# Run with environment variables
docker run -d \
  --name twitch-1337 \
  -e TWITCH_USERNAME=your_bot \
  -e TWITCH_ACCESS_TOKEN=your_access_token \
  -e TWITCH_REFRESH_TOKEN=your_refresh_token \
  -e TWITCH_CLIENT_ID=your_client_id \
  -e TWITCH_CLIENT_SECRET=your_client_secret \
  -e TWITCH_CHANNEL=REDACTED_CHANNEL \
  -e RUST_LOG=info \
  chronophylos/twitch-1337:latest

# Or use .env file
docker run -d --name twitch-1337 --env-file .env chronophylos/twitch-1337:latest

# Or use just
just run         # Run in background
just run-test    # Run interactively for testing
```

### Local

```bash
# With cargo
RUST_LOG=debug cargo run --release

# With just
just run-local
```

## Justfile Commands

The project includes a [Justfile](https://github.com/casey/just) for common tasks:

### Docker Operations
- `just build` - Build the Docker image
- `just build-no-cache` - Force full rebuild
- `just run` - Run container in background (requires `.env`)
- `just run-test` - Run interactively for testing
- `just up` - Build and run
- `just reload` - Stop, remove, rebuild, and run
- `just logs` - Follow container logs
- `just logs-tail` - Show last 50 log lines
- `just stop` - Stop the container
- `just clean` - Stop and remove container
- `just restart` - Restart the container
- `just push` - Push to Docker Hub
- `just pull` - Pull from Docker Hub

### Local Development
- `just build-local` - Build Rust binary (glibc)
- `just build-musl` - Build static musl binary
- `just run-local` - Run with cargo
- `just lint` - Run clippy
- `just fmt` - Format code
- `just fmt-check` - Check formatting

## System Requirements

### Docker
- Docker Engine 20.10+
- No other dependencies (uses FROM scratch)

### Local Execution

**Option 1: Standard build (dynamically linked)**
- Linux with glibc 2.31+ (most modern distros)
- Runtime dependencies: `libc.so.6`, `libm.so.6`, `libgcc_s.so.1`

**Option 2: Static musl build (recommended for minimal systems)**
- Any Linux kernel 4.4.0+
- **No runtime dependencies** - fully static binary
- Works on Alpine, busybox, or any minimal system

Build musl binary with:
```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## How It Works

### Daily Timeline

```
13:36:00 → Monitor connects to IRC (anonymous)
13:37:00-13:37:59 → Messages containing "1337" counted (unique users tracked)
13:39:00 → Stats posted to channel (authenticated)
13:39:01 → Monitor disconnects, state cleared
```

### Architecture

**Two scheduled jobs run daily:**

1. **Monitor Job** (13:36-13:39)
   - Anonymous IRC connection
   - Filters messages at exactly 13:37:xx containing "1337"
   - Tracks unique usernames in `Arc<Mutex<HashSet<String>>>`

2. **Stats Job** (13:39)
   - Authenticated IRC connection
   - Posts message: "X unique users said 1337 at 13:37 today!"
   - Clears HashSet for next day

**Key Technologies:**
- `tokio` - Async runtime (minimal features: macros, rt-multi-thread, time)
- `tokio-cron-scheduler` - Daily job scheduling
- `twitch-irc` - IRC client with rustls (no OpenSSL)
- `chrono-tz` - Berlin timezone (filtered to single timezone for size)
- `tracing` - Structured logging
- `color-eyre` - Rich error messages

## Development

### Prerequisites

- Rust 1.75+ (tested with 1.89)
- Docker (optional, for container builds)
- Just (optional, for convenience commands)

### Setup

```bash
# Clone repository
git clone https://github.com/chronophylos/twitch-1337.git
cd twitch-1337

# Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install just (optional)
cargo install just

# Build
cargo build --release
```

### Code Quality

```bash
# Check code
cargo check

# Lint with clippy
cargo clippy

# Format code
cargo fmt

# Or use just
just lint
just fmt
```

### Logging

Set `RUST_LOG` to control verbosity:

```bash
RUST_LOG=trace cargo run    # Extremely verbose
RUST_LOG=debug cargo run    # See all IRC messages
RUST_LOG=info cargo run     # Default, shows important events
RUST_LOG=warn cargo run     # Warnings only
RUST_LOG=error cargo run    # Errors only
```

## Binary Size Optimization

The project is optimized for minimal binary size:

- **Standard build**: ~6.2MB
- **Musl static build**: ~6.0MB
- **Docker image**: ~6MB (FROM scratch)

**Optimizations:**
- Minimal tokio features (no IO drivers, no process, etc.)
- Single timezone compiled via `CHRONO_TZ_TIMEZONE_FILTER`
- `rustls` instead of OpenSSL (pure Rust TLS)
- No default features on `twitch-irc`
- Static linking with musl

## Deployment

### Docker Compose

```yaml
version: '3.8'
services:
  twitch-1337:
    image: chronophylos/twitch-1337:latest
    container_name: twitch-1337
    restart: unless-stopped
    environment:
      TWITCH_USERNAME: your_bot_username
      TWITCH_ACCESS_TOKEN: your_access_token_here
      TWITCH_REFRESH_TOKEN: your_refresh_token_here
      TWITCH_CLIENT_ID: your_client_id_here
      TWITCH_CLIENT_SECRET: your_client_secret_here
      TWITCH_CHANNEL: REDACTED_CHANNEL
      RUST_LOG: info
```

### Systemd Service

```ini
[Unit]
Description=Twitch 1337 Bot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=twitch-bot
Environment="TWITCH_USERNAME=your_bot"
Environment="TWITCH_ACCESS_TOKEN=your_access_token"
Environment="TWITCH_REFRESH_TOKEN=your_refresh_token"
Environment="TWITCH_CLIENT_ID=your_client_id"
Environment="TWITCH_CLIENT_SECRET=your_client_secret"
Environment="TWITCH_CHANNEL=REDACTED_CHANNEL"
Environment="RUST_LOG=info"
ExecStart=/usr/local/bin/twitch-1337
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

## Troubleshooting

### Bot doesn't post stats

1. Check logs: `docker logs twitch-1337` or `just logs`
2. Verify `TWITCH_ACCESS_TOKEN` is correct and has chat permissions
3. Ensure `TWITCH_USERNAME` matches the token's account
4. Check if bot is joined to channel at 13:39 Berlin time

### No messages counted

1. Set `RUST_LOG=debug` to see all IRC messages
2. Verify messages contain "1337" exactly
3. Check system timezone is correct (bot uses Europe/Berlin internally)
4. Ensure messages are sent between 13:37:00-13:37:59 Berlin time

### Binary won't run on minimal system

1. Use musl build: `cargo build --release --target x86_64-unknown-linux-musl`
2. Or use Docker: `just build && just run`
3. Verify with: `ldd binary` (should show "statically linked")

## License

[Add your license here]

## Credits

Created for the Twitch channel [REDACTED_CHANNEL](https://twitch.tv/REDACTED_CHANNEL).
