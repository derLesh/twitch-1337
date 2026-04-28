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
cargo audit                # optional locally; CI job fails on advisories

# Deploy (Justfile, podman local → docker.homelab SSH)
just build | just push | just restart | just deploy
```

## CI & branch policy

`main` is branch-protected (admin enforced). Direct pushes rejected — use a PR.
Linear history required, force-push + delete blocked, conversations must resolve.

**Required status checks (7, must all pass to merge):**

| Check | Workflow | Purpose |
|---|---|---|
| `fmt + clippy + test` | ci.yml | cargo fmt --check, clippy -D warnings, cargo test |
| `cargo audit` | ci.yml | RustSec CVE scan of Cargo.lock via rustsec/audit-check |
| `hadolint (Dockerfile)` | sast.yml | Dockerfile lint, SARIF → Security tab |
| `trivy config (IaC)` | sast.yml | IaC misconfig scan, HIGH/CRITICAL only |
| `actionlint (workflows)` | sast.yml | Workflow YAML + shell lint |
| `zizmor (workflows)` | sast.yml | Workflow security (injection, perms, pinning) |
| `gitleaks (secrets)` | sast.yml | Full-history secret scan |

`Docker` (build + push ghcr.io/chronophylos/twitch-1337:latest + sha-N) runs only on
push to main (post-merge), not on PRs — not a required check.
`Data refresh` runs Sundays 03:00 UTC; opens a `chore/data-refresh` PR.

**Native GitHub security (repo settings):** secret_scanning, push_protection,
dependabot_security_updates — all enabled. Push-protection blocks commits
containing known provider tokens at the server.

**Dependabot** (`.github/dependabot.yml`): weekly PRs for `cargo`, `github-actions`,
`docker`. Cargo minor+patch grouped as `rust-minor-patch`. Docker ecosystem bumps
both tag AND sha256 digest in Dockerfile.

**Action pinning:** security-critical actions pinned to **commit SHA** with version
comment: `rustsec/audit-check`, `gitleaks/gitleaks-action`, `zizmorcore/zizmor-action`,
`aquasecurity/trivy-action` (Mar 2026 supply-chain incident — always SHA-pin trivy).
Others pinned to major tags; Dependabot keeps them current.

**Typical PR flow:**
1. branch → commit → push → `gh pr create`
2. wait for 7 checks green; rebase on main if `strict` blocks merge
3. `gh pr merge --squash`

**When `cargo audit` fails:**
- Check open Dependabot PRs first (weekly); a bump may already be queued.
- Transitive vulns: `cargo tree -i <crate>` to find the parent; bump the parent.
- If two majors of one crate coexist in Cargo.lock (e.g. rustls-webpki 0.101 + 0.103),
  both must be resolved — usually by bumping the dep pulling in the old major.
- Last resort: ignore in `.cargo/audit.toml` with a written-down reason.

**When Dependabot cargo PRs break `fmt + clippy + test`:**
- Breaking-API bump; adapt code on the dependabot branch and force-push
  (`git push origin dependabot/cargo/<name>-<ver>`). CI reruns, then merge.
- Example: rand 0.10 moved `.random::<T>()` from `Rng` to `RngExt` — generic bounds
  become `impl rand::RngExt`.

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

Handlers live in `src/twitch/handlers/` and are spawned from `src/lib.rs::run_bot`. Each handler is generic over `<T: Transport, L: LoginCredentials>` so integration tests can swap in a fake transport. Shared deps (clock, data_dir, llm, aviation) come from `Services` in `src/lib.rs`.

```rust
// src/twitch/handlers/my_handler.rs
pub async fn run_my_handler<T, L>(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<TwitchIRCClient<T, L>>,
    /* other deps from Services as needed */
) where T: Transport, L: LoginCredentials { /* subscribe, filter, act */ }

// src/lib.rs::run_bot: spawn + add to the final tokio::select! exit arm
```

Integration-testable via `TestBotBuilder` in `tests/common/`.

## Key constants

`src/twitch/handlers/tracker_1337.rs`: `TARGET_HOUR=13`, `TARGET_MINUTE=37`, `MAX_USERS=10_000`.

`src/twitch/handlers/latency.rs`: `LATENCY_PING_INTERVAL=300s`, `LATENCY_EMA_ALPHA=0.2`.

`src/aviation/tracker.rs`: `MAX_TRACKED_FLIGHTS=12`, `MAX_FLIGHTS_PER_USER=3`, `TRACKING_LOST_THRESHOLD=300s`, `TRACKING_LOST_REMOVAL=1800s`, `POLL_FAST/NORMAL/SLOW=30/60/120s`.

## Binary / Docker

Musl static build: `cargo build --release --target x86_64-unknown-linux-musl` (~6MB, works on Alpine/busybox, FROM scratch image). rustls not OpenSSL. Multi-stage Dockerfile with cargo-chef.

Verify static: `ldd target/x86_64-unknown-linux-musl/release/twitch-1337` → "statically linked".
