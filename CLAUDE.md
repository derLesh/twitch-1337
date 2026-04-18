# CLAUDE.md

Guide Claude Code work repo.

## Project

Rust Twitch IRC bot. Features: 1337 tracker (13:37 Berlin), leaderboard (`!lb`), ping system (`!p`, `!<ping>`), scheduled messages (config.toml), latency monitor, AI (`!ai`, OpenAI/Ollama), flight tracker (`!track`, `!untrack`, `!flight`, `!flights`), aviation lookups (`!up`, `!fl`), feedback (`!fb`).

Single persistent IRC connection, broadcast channel routes to independent handler tasks.

## Commands

```bash
# Dev
cargo build | cargo run | cargo check | cargo test
RUST_LOG=debug cargo run   # all IRC + handler activity
RUST_LOG=trace cargo run   # every ServerMessage

# Pre-commit (CI gate — required, in order)
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test

# Deploy (Justfile, podman local → docker.homelab SSH)
just build | just push | just restart | just deploy
```

## Config

`config.toml` (copy `config.toml.example`). Sections: `[twitch]`, `[pings]`, `[ai]` (optional), `[cooldowns]`, `[[schedules]]` (optional, repeatable). Schema + defaults in `config.toml.example` — treat as source of truth.

Schedules hot-reload on save (2s debounce via notify-debouncer-mini). No restart.

OAuth credentials + AI API key wrapped in `SecretString` (secrecy crate). Config structs are `Deserialize`-only; do NOT add `Serialize` derive (closes credential-leak via debug dump).

## Data dir

All runtime-persistent files live under `$DATA_DIR` (default `/var/lib/twitch-1337`, Docker sets `/data`). Use `get_data_dir().join(...)` — never hardcode relative paths. Files: `token.ron`, `pings.ron`, `ai_memory.ron`, `leaderboard.ron`, `flights.ron`, `feedback.txt`.

Atomic persistence pattern: write tmp + rename. See `ping.rs`, `memory.rs`, `flight_tracker.rs`.

## Embedded data

`data/plz.csv`, `data/airports.csv`, `data/airlines.csv` baked in via `include_str!`. Zero runtime data-file dep (important for `FROM scratch` musl image).

## Architecture invariants

- All time ops use `Europe/Berlin` (chrono-tz with `CHRONO_TZ_TIMEZONE_FILTER=Europe/Berlin` in `.cargo/config.toml` — only Berlin data compiled in).
- 1337 tracker: messages containing "1337" or "DANKIES" at exactly 13:37:xx. Ignore "supibot", "potatbotat". Dedupe via HashSet (max 10k). Leaderboard = fastest sub-1s PB.
- Broadcast channel capacity 100. Lagging handlers get `RecvError::Lagged`, continue.
- Handlers independent. Errors in one don't crash others (`bail!` on startup only).
- Ping templates: reject control chars on create/edit (CR/LF would split PRIVMSG).
- Aviation client init failure: log + disable `!up`/`!fl`/flight tracker + track commands. Don't abort.
- Latency monitor: PING/PONG every 5min, EMA alpha=0.2, shared `Arc<AtomicU32>`. Read by 1337 handler for precise wake-up.
- Flight tracker: `Arc<mpsc::Sender<TrackerCommand>>` from commands to long task. Adaptive poll 30/60/120s based on phase mix. adsb.lol v2; fallback aggregators in memory `reference_adsb_aggregators.md`.
- AI memory: LLM manages via `save_memory`/`delete_memory` tool calls. Fire-and-forget extraction after each response. Cap `max_memories` (default 50, max 200).
- Scheduled messages: on Ctrl+C, main notifies `Arc<Notify>`; children finish in-flight `say()` then exit; main awaits handler with 5s timeout.

## Gotchas

- `twitch_irc::irc!` macro needs standalone `use twitch_irc::irc;` — cannot be in braced `use twitch_irc::{...}`.
- Request/response on broadcast (e.g. PING/PONG): subscribe BEFORE send to avoid race.
- Clippy strict (`-D warnings`) + extra lints in `Cargo.toml [lints.clippy]`. Don't `#[allow]` without one-line reason.
- Log errors with `?error` not `%error` to include backtrace.

## Adding handler

```rust
async fn run_my_handler(broadcast_tx, client) { /* subscribe, filter, act */ }
// main(): tokio::spawn(run_my_handler(broadcast_tx.clone(), client.clone()))
// Add to tokio::select! for coordinated shutdown
```

## Key constants

`src/main.rs`: `TARGET_HOUR=13`, `TARGET_MINUTE=37`, `MAX_USERS=10_000`, `LATENCY_PING_INTERVAL=300s`, `LATENCY_EMA_ALPHA=0.2`.

`src/flight_tracker.rs`: `MAX_TRACKED_FLIGHTS=12`, `MAX_FLIGHTS_PER_USER=3`, `TRACKING_LOST_THRESHOLD=300s`, `TRACKING_LOST_REMOVAL=1800s`, `POLL_FAST/NORMAL/SLOW=30/60/120s`.

## Binary / Docker

Musl static build: `cargo build --release --target x86_64-unknown-linux-musl` (~6MB, works on Alpine/busybox, FROM scratch image). rustls not OpenSSL. Multi-stage Dockerfile with cargo-chef.

Verify static: `ldd target/x86_64-unknown-linux-musl/release/twitch-1337` → "statically linked".
