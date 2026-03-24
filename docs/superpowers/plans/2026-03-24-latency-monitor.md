# Latency Monitor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Auto-measure IRC latency via PING/PONG every 5 minutes, compute an EMA, and use it as the live `expected_latency` for timing-sensitive handlers.

**Architecture:** A new independent handler task (`run_latency_handler`) sends PING with a unique nonce, listens for the matching PONG on the broadcast channel, computes one-way latency (RTT/2), and updates an EMA stored in `Arc<AtomicU32>`. The 1337 handler reads this shared atomic instead of the static config value.

**Tech Stack:** Rust, twitch-irc (irc! macro, ServerMessage::Pong), tokio, std::sync::atomic

**Spec:** `docs/superpowers/specs/2026-03-24-latency-monitor-design.md`

---

### Task 1: Make `expected_latency` optional with serde default

**Files:**
- Modify: `src/main.rs:370-381` (TwitchConfiguration struct)
- Modify: `src/main.rs:458-459` (validation)
- Modify: `config.toml.example:20-22`

- [ ] **Step 1: Add the default function above `TwitchConfiguration`**

Add right before the `TwitchConfiguration` struct definition (before line 370):

```rust
fn default_expected_latency() -> u32 {
    100
}
```

- [ ] **Step 2: Add serde default attribute to `expected_latency` field**

Change line 380 from:
```rust
    expected_latency: u32,
```
to:
```rust
    #[serde(default = "default_expected_latency")]
    expected_latency: u32,
```

- [ ] **Step 3: Update `config.toml.example` comment**

Change lines 20-22 from:
```toml
# Expected IRC message latency in milliseconds (used for precise 13:37 timing)
# Typical values: 50-150ms. Adjust based on your connection.
expected_latency = 89
```
to:
```toml
# Initial seed for IRC latency estimate in milliseconds (optional, default: 100)
# The bot measures actual latency via PING/PONG every 5 minutes and auto-adjusts.
# expected_latency = 89
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 5: Commit**

```bash
git add src/main.rs config.toml.example
git commit -m "feat: make expected_latency optional with default 100"
```

---

### Task 2: Add constants and imports for latency handler

**Files:**
- Modify: `src/main.rs:1-25` (imports)
- Modify: `src/main.rs:360-366` (constants area)

- [ ] **Step 1: Add imports**

Add to the `std` use block (line 1-5):
```rust
use std::sync::atomic::{AtomicU32, Ordering};
```

Add `PongMessage` to the `twitch_irc` message imports (line 21-25):
```rust
use twitch_irc::{
    ClientConfig, SecureTCPTransport, TwitchIRCClient,
    login::{RefreshingLoginCredentials, TokenStorage, UserAccessToken},
    message::{NoticeMessage, PongMessage, PrivmsgMessage, ServerMessage},
};
```

The `irc!` macro is exported via `#[macro_export]` from the twitch-irc crate, so it's available at the call site as `twitch_irc::irc!` without any `use` statement needed.

- [ ] **Step 2: Add latency constants**

Add after `MAX_USERS` constant (after line 364):

```rust
/// Interval between PING measurements
const LATENCY_PING_INTERVAL: Duration = Duration::from_secs(300);

/// Timeout waiting for PONG response
const LATENCY_PING_TIMEOUT: Duration = Duration::from_secs(10);

/// EMA smoothing factor (0.2 = moderate responsiveness)
const LATENCY_EMA_ALPHA: f64 = 0.2;

/// Only log EMA changes at info level when delta exceeds this threshold
const LATENCY_LOG_THRESHOLD: u32 = 10;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles (warnings about unused imports are fine for now)

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add latency monitor constants and imports"
```

---

### Task 3: Implement `run_latency_handler`

**Files:**
- Modify: `src/main.rs` — add new function after `run_1337_handler` (after the 1337 handler section, before the generic command handler)

- [ ] **Step 1: Add the latency handler function**

Add the following function. Place it after `run_1337_handler` and its helper functions, before `run_generic_command_handler`:

```rust
/// Periodically measures IRC latency via PING/PONG and updates a shared EMA estimate.
///
/// Sends a PING with a unique nonce every 5 minutes, measures the round-trip time
/// from the matching PONG response, and updates an exponential moving average (EMA)
/// of the one-way latency. The EMA is stored in a shared `AtomicU32` that other
/// handlers (e.g., the 1337 handler) read for timing adjustments.
///
/// The handler is fully independent — PING failures or PONG timeouts are logged
/// but never crash the handler or affect the EMA.
///
/// Note: The spec lists `initial_latency: u32` as an input, but we pass the
/// `Arc<AtomicU32>` directly since it's created in main() and shared with the
/// 1337 handler — the initial value is already loaded into the atomic.
#[instrument(skip(client, broadcast_tx, latency))]
async fn run_latency_handler(
    client: Arc<AuthenticatedTwitchClient>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
    latency: Arc<AtomicU32>,
) {
    let initial = latency.load(Ordering::Relaxed);
    info!(initial_latency_ms = initial, "Latency handler started");

    let mut ema: f64 = initial as f64;
    let mut last_logged_ema: u32 = initial;

    loop {
        sleep(LATENCY_PING_INTERVAL).await;

        let nonce = format!("{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
        let send_time = tokio::time::Instant::now();

        // Send PING with unique nonce
        if let Err(e) = client.send_message(twitch_irc::irc!["PING", nonce.clone()]).await {
            warn!(error = ?e, "Failed to send PING");
            continue;
        }

        // Listen for matching PONG
        let mut broadcast_rx = broadcast_tx.subscribe();
        let pong_result = tokio::time::timeout(LATENCY_PING_TIMEOUT, async {
            loop {
                match broadcast_rx.recv().await {
                    Ok(ServerMessage::Pong(PongMessage { source, .. })) => {
                        if source.params.get(1).map(String::as_str) == Some(nonce.as_str()) {
                            return send_time.elapsed();
                        }
                        // Nonce mismatch — likely library keepalive, ignore
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("Broadcast channel closed during PONG wait");
                        return send_time.elapsed(); // will be discarded by caller
                    }
                    _ => continue,
                }
            }
        })
        .await;

        let rtt = match pong_result {
            Ok(elapsed) => elapsed,
            Err(_) => {
                warn!("PONG timeout after {:?}", LATENCY_PING_TIMEOUT);
                continue;
            }
        };

        let one_way_ms = rtt.as_millis() as f64 / 2.0;
        ema = LATENCY_EMA_ALPHA * one_way_ms + (1.0 - LATENCY_EMA_ALPHA) * ema;
        let ema_rounded = ema.round() as u32;

        latency.store(ema_rounded, Ordering::Relaxed);

        debug!(
            rtt_ms = rtt.as_millis() as u64,
            one_way_ms = one_way_ms as u64,
            ema_ms = ema_rounded,
            "Latency measurement"
        );

        // Log at info level only when EMA shifts significantly
        if ema_rounded.abs_diff(last_logged_ema) >= LATENCY_LOG_THRESHOLD {
            info!(
                previous_ms = last_logged_ema,
                current_ms = ema_rounded,
                "Latency EMA changed"
            );
            last_logged_ema = ema_rounded;
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles (warning about unused function is fine)

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: implement run_latency_handler with PING/PONG EMA"
```

---

### Task 4: Update `run_1337_handler` to use shared latency

> **Note:** Line numbers below are from the original file. Tasks 1-3 insert lines earlier in the file, so search for the function/code by content rather than trusting exact line numbers.

**Files:**
- Modify: `src/main.rs` — `run_1337_handler` function signature and its two `sleep_until_hms` calls

- [ ] **Step 1: Change the function signature**

Change:
```rust
#[instrument(skip(broadcast_tx, client, channel))]
async fn run_1337_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    channel: String,
    expected_latency: u32,
) {
```
to:
```rust
#[instrument(skip(broadcast_tx, client, channel, latency))]
async fn run_1337_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    channel: String,
    latency: Arc<AtomicU32>,
) {
```

- [ ] **Step 2: Update the two `sleep_until_hms` call sites**

Change line 1545 from:
```rust
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30, expected_latency).await;
```
to:
```rust
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE - 1, 30, latency.load(Ordering::Relaxed)).await;
```

Change line 1556 from:
```rust
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0, expected_latency).await;
```
to:
```rust
        sleep_until_hms(TARGET_HOUR, TARGET_MINUTE + 1, 0, latency.load(Ordering::Relaxed)).await;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: update 1337 handler to read live latency from shared atomic"
```

---

### Task 5: Wire up latency handler in `main()`

> **Note:** Line numbers below are from the original file. Earlier tasks shift them — search by content.

**Files:**
- Modify: `src/main.rs` — `main()` function: 1337 handler spawn block, info log messages, tokio::select! blocks

- [ ] **Step 1: Create the shared atomic and spawn latency handler**

Before the 1337 handler spawn block (before line 1297), add:

```rust
    // Create shared latency estimate, seeded from config
    let latency = Arc::new(AtomicU32::new(config.twitch.expected_latency));

    // Spawn latency monitor handler
    let handler_latency = tokio::spawn({
        let client = client.clone();
        let broadcast_tx = broadcast_tx.clone();
        let latency = latency.clone();
        async move {
            run_latency_handler(client, broadcast_tx, latency).await;
        }
    });
```

- [ ] **Step 2: Update the 1337 handler spawn to pass the shared atomic**

Change the 1337 handler spawn block from:
```rust
    let handler_1337 = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let channel = config.twitch.channel.clone();
        let expected_latency = config.twitch.expected_latency;
        async move {
            run_1337_handler(broadcast_tx, client, channel, expected_latency).await;
        }
    });
```
to:
```rust
    let handler_1337 = tokio::spawn({
        let broadcast_tx = broadcast_tx.clone();
        let client = client.clone();
        let channel = config.twitch.channel.clone();
        let latency = latency.clone();
        async move {
            run_1337_handler(broadcast_tx, client, channel, latency).await;
        }
    });
```

- [ ] **Step 3: Add latency handler to both `tokio::select!` blocks**

In the first select block (with schedules), add before the closing `}`:
```rust
                result = handler_latency => {
                    error!("Latency handler exited unexpectedly: {result:?}");
                }
```

In the second select block (without schedules), add before the closing `}`:
```rust
                result = handler_latency => {
                    error!("Latency handler exited unexpectedly: {result:?}");
                }
```

Note: Since `handler_latency` is a `JoinHandle` and can only be awaited once, it needs to be in only one select branch. Since both select blocks are in a match arm (only one runs), this is fine — but the variable must be accessible in both arms. Ensure `handler_latency` is defined before the `match` statement (it is, per step 1).

- [ ] **Step 4: Update the info log messages to mention the latency handler**

Update the handler list logs to include "Latency monitor":
```rust
    if schedules_enabled {
        info!(
            "Bot running with continuous connection. Handlers: Config watcher, 1337 tracker, Generic commands, Scheduled messages, Latency monitor"
        );
```
and:
```rust
    } else {
        info!(
            "Bot running with continuous connection. Handlers: 1337 tracker, Generic commands, Latency monitor"
        );
    }
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 6: Run clippy**

Run: `cargo clippy`
Expected: no warnings from our changes

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire up latency handler in main, connect to 1337 handler"
```

---

### Task 6: Manual integration test

This feature relies on a live Twitch IRC connection, so it can't be unit tested in isolation. Verify manually.

- [ ] **Step 1: Build release**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 2: Test with config that has `expected_latency`**

Ensure `config.toml` has `expected_latency = 89` set. Run the bot with debug logging:

Run: `RUST_LOG=debug cargo run`

Expected in logs:
- `Latency handler started` with `initial_latency_ms=89`
- After ~5 minutes: `Latency measurement` debug log with `rtt_ms`, `one_way_ms`, `ema_ms` values
- If EMA shifts by >= 10ms from 89: `Latency EMA changed` info log

- [ ] **Step 3: Test with config that omits `expected_latency`**

Comment out `expected_latency` in `config.toml`. Run:

Run: `RUST_LOG=debug cargo run`

Expected: `Latency handler started` with `initial_latency_ms=100` (the default)

- [ ] **Step 4: Verify PING/PONG works at all**

If the bot connects but you never see a `Latency measurement` log after 5+ minutes, the PONG may not be reaching the broadcast channel. Check for `PONG timeout` warnings. This would indicate the twitch-irc library consumes the PONG internally without forwarding it — in which case we need to investigate alternative approaches.

- [ ] **Step 5: Commit any fixes if needed**
