# E2E Test Harness Design

**Date:** 2026-04-18
**Status:** Design approved, awaiting implementation plan

## Goal

Add end-to-end integration tests that drive the full bot (IRC client, router, all handlers) against a fake Twitch IRC transport and assert correct outgoing messages. Cover the eight user-visible features with sufficient seams to exercise cooldowns, template rendering, persistence, LLM calls, ADS-B polling, and time-dependent logic.

Non-goals:
- Unit-testing individual helpers (already served by existing `#[cfg(test)]` modules).
- Mocking internals of the `twitch-irc` crate (we trust its parser).
- Testing token refresh, latency monitor, or config watcher in v1.

## Design Decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Test scope | Full bot including background handlers (1337 tracker, scheduled messages, flight tracker) |
| 2 | Mock IRC strategy | Transport-level fake via custom `Transport` impl over `tokio::io::DuplexStream` |
| 3 | Clock abstraction | Surgical `Clock` trait injected only into handlers that read wall-clock (`Utc::now`) |
| 4 | LLM stubbing | Fake `LlmClient` impl (trait already exists) |
| 5 | HTTP stubbing (ADS-B, rustlog, Nominatim) | `wiremock` with injected `base_url` |
| 6 | Transport generic propagation | Extract `lib.rs` with `pub fn run_bot<T: Transport>(...)` entry point |

## Architecture

### Source layout changes

```
src/
├── main.rs          binary: read config, build TwitchIRCClient<SecureTCPTransport>, call lib::run_bot
├── lib.rs           pub fn run_bot<T>(client, cfg, services, shutdown) -> Result<()>
├── clock.rs         pub trait Clock { ... }, impl SystemClock; FakeClock lives in tests/common
├── aviation.rs      AviationClient::new_with_base_url(base_url: String, client: reqwest::Client)
├── prefill.rs       already accepts base_url; keep
├── handlers/        new module: 1337, latency, router, generic_commands, schedules, config_watcher
└── commands/        unchanged

lib::Services bundles test-overridable deps:

pub struct Services {
    pub clock: Arc<dyn Clock>,
    pub llm: Option<Arc<dyn LlmClient>>,   // None if [ai] not configured
    pub aviation: Option<AviationClient>,  // None if aviation init failed
    pub data_dir: PathBuf,
}
```

**Key moves:**

- `main.rs` becomes a thin configure-and-launch shell: load config, build real client, assemble `Services` with production impls, call `lib::run_bot`, wait on Ctrl+C.
- Handler functions currently defined in `main.rs` (`run_message_router`, `run_1337_handler`, `run_latency_handler`, `run_generic_command_handler`, `run_scheduled_message_handler`, `run_config_watcher_service`) move into `src/handlers/` sub-modules inside the library crate. Each handler file owns one handler.
- Handler signatures that touch the IRC client become generic over `T: Transport + Send + Sync + 'static`. The `AuthenticatedTwitchClient` type alias becomes generic with a production default:
  ```rust
  pub type AuthenticatedTwitchClient<T = SecureTCPTransport> =
      TwitchIRCClient<T, RefreshingLoginCredentials<FileBasedTokenStorage>>;
  ```
  Tests use `TwitchIRCClient<FakeTransport, StaticLoginCredentials>` (test-only login, no OAuth refresh path exercised).

### Clock injection surface

Only these handlers receive `Arc<dyn Clock>`:

| Handler | Why |
|---|---|
| `run_1337_handler` | Sleeps until 13:36/13:37/13:38 Berlin |
| `run_schedule_task` | Checks `is_active(now)` against date range + time window |
| `run_flight_tracker` | Lost-threshold (300s) and removal (1800s) comparisons |

All other handlers keep direct `Utc::now()` calls (latency monitor, feedback timestamp) — they are not in the v1 test suite.

The `Clock` trait:

```rust
#[async_trait]
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    async fn sleep_until(&self, target: DateTime<Utc>);
}

pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> { Utc::now() }
    async fn sleep_until(&self, target: DateTime<Utc>) {
        let delta = (target - Utc::now()).to_std().unwrap_or_default();
        tokio::time::sleep(delta).await;
    }
}
```

Tests pair `FakeClock` with `#[tokio::test(start_paused = true)]` so tokio-native `sleep` and `Instant` advance only via `tokio::time::advance`.

### External HTTP surfaces

- **LLM** — `run_bot` takes `Arc<dyn LlmClient>`. Prod wires `openai::Client` or `ollama::Client` via `[ai].backend`. Tests pass `Arc::new(FakeLlm::new(canned))`.
- **ADS-B (adsb.lol)** — `AviationClient::new_with_base_url(base_url, http_client)` added. Default constructor keeps current behavior (`https://api.adsb.lol`). Tests pass `wiremock.uri()`.
- **Rustlog history prefill** — already parameterized via `HistoryPrefillConfig::base_url`. No change.
- **Nominatim (aviation geocoding)** — same pattern as ADS-B: add `base_url` param to the relevant call site.

## Test harness components

Location: `tests/common/`.

### `FakeTransport` (`tests/common/fake_transport.rs`)

Implements twitch-irc's `Transport` trait using `tokio::io::duplex(8192)`. One half is given to the `TwitchIRCClient`, the other is exposed as `TransportHandle` with `inject: mpsc::Sender<String>` and `capture: mpsc::Receiver<String>`.

The transport includes a minimal handshake reply script so the client's connection state machine completes:

```
CAP REQ → CAP * ACK :twitch.tv/commands twitch.tv/tags twitch.tv/membership
PASS oauth:... → (no reply; implicit success)
NICK bot → :tmi.twitch.tv 001 bot :... (plus 002-004, 375-376)
JOIN #chan → GLOBALUSERSTATE + ROOMSTATE for chan
```

Injected lines use full IRCv3 tag format: `@badges=...;user-id=123;tmi-sent-ts=... :user!user@host PRIVMSG #chan :!foo\r\n`.

Helper `irc_line::privmsg(user, text)` and `irc_line::privmsg_with(user, text, tags: &[(&str, &str)])` build tagged lines.

### `FakeClock` (`tests/common/fake_clock.rs`)

```rust
pub struct FakeClock {
    now: Arc<Mutex<DateTime<Utc>>>,
    waiters: Arc<Mutex<Vec<(DateTime<Utc>, oneshot::Sender<()>)>>>,
}

impl FakeClock {
    pub fn new(t: DateTime<Utc>) -> Arc<Self>;
    pub fn advance(&self, dur: chrono::Duration);  // also wakes matching waiters
    pub fn set(&self, t: DateTime<Utc>);
}

#[async_trait]
impl Clock for FakeClock { ... }
```

`sleep_until` registers a waiter and awaits a oneshot; `advance`/`set` drain any waiters whose target is now reached. This avoids a busy-poll loop and keeps clock-dependent handlers deterministic.

### `FakeLlm` (`tests/common/fake_llm.rs`)

```rust
pub struct FakeLlm {
    chat_responses: Mutex<VecDeque<String>>,
    tool_responses: Mutex<VecDeque<ToolChatCompletionResponse>>,
    calls: Mutex<Vec<ChatCompletionRequest>>,  // for assertions
}

#[async_trait]
impl LlmClient for FakeLlm {
    async fn chat_completion(&self, req: ChatCompletionRequest) -> Result<String>;
    async fn tool_chat_completion(&self, req: ToolChatCompletionRequest) -> Result<ToolChatCompletionResponse>;
}
```

Tests pre-load responses and inspect `calls` after the fact (e.g., assert system prompt contains expected memory block).

### `TestBot` (`tests/common/mod.rs`)

```rust
pub struct TestBot {
    pub transport: TransportHandle,
    pub clock: Arc<FakeClock>,
    pub data_dir: TempDir,
    pub adsb_mock: wiremock::MockServer,
    pub rustlog_mock: wiremock::MockServer,
    pub llm: Arc<FakeLlm>,
    shutdown: oneshot::Sender<()>,
    bot_task: JoinHandle<Result<()>>,
}

impl TestBot {
    pub async fn spawn(cfg: Configuration) -> Self;
    pub async fn send(&self, user: &str, msg: &str);
    pub async fn send_with_tags(&self, user: &str, msg: &str, tags: &[(&str, &str)]);
    pub async fn expect_say(&self, timeout: Duration) -> String;
    pub async fn expect_silent(&self, dur: Duration);
    pub async fn shutdown(self);  // surfaces JoinHandle errors
}
```

`spawn` does:
1. Create `TempDir`, set `DATA_DIR` env var.
2. Start wiremock servers, capture URIs.
3. Build `FakeLlm`, `FakeClock`.
4. Build `fake_transport_pair()` → `(FakeTransport, TransportHandle)`.
5. Build `TwitchIRCClient::<FakeTransport, StaticLoginCredentials>::new(client_cfg)` with test credentials.
6. Assemble `lib::Services { clock: fake_clock, llm: Some(fake_llm), aviation: Some(aviation_with_mock_base_url), data_dir: tempdir.path().into() }`.
7. `tokio::spawn(lib::run_bot(client, cfg, services, shutdown_rx))`.

All wiremock/FakeLlm/FakeClock handles are retained on `TestBot` so tests can `bot.llm.expect_called_with(...)`, `bot.adsb_mock.register(Mock::...)`, `bot.clock.advance(...)`.

## Data flow

### Request round trip

```
test code          TestBot                   lib::run_bot                    twitch-irc
─────────          ───────                   ────────────                    ──────────
bot.send("alice","!foo")
      │
      └──▶ transport.inject ──IRC line──▶ FakeTransport incoming
                                                │
                                                ▼
                                       TwitchIRCClient parses frame
                                                │
                                                ▼
                                       broadcast::Sender<ServerMessage>
                                                │
                                                ▼
                                       CommandDispatcher matches !foo
                                                │
                                                ▼
                                       PingTriggerCommand::execute
                                                │
                                                ▼
                                       client.say(channel, "hi @bob") ─▶ FakeTransport outgoing
                                                                              │
bot.expect_say(1s) ◀──captured line──── transport.capture ◀───────────────────┘
      │
      ▼
assert body == "hi @bob"
```

### Clock-dependent scenario (1337 handler)

```
#[tokio::test(start_paused = true)]
async fn tracker_1337_counts_users_and_posts_stats() {
    let cfg = test_config();
    let bot = TestBot::spawn_with_clock(cfg, FakeClock::new(berlin(13, 35, 0))).await;

    // 13:36:30 — reminder posted
    bot.clock.advance(Duration::minutes(1) + Duration::seconds(30));
    assert_eq!(bot.expect_say(1.s()).await, "PausersHype");

    // 13:37:xx — inject messages
    bot.clock.advance(Duration::seconds(30));
    bot.send("alice", "1337").await;
    bot.send("charlie", "DANKIES").await;

    // 13:38:30 — stats
    bot.clock.advance(Duration::minutes(1) + Duration::seconds(30));
    let stats = bot.expect_say(1.s()).await;
    assert!(stats.contains("@alice"));
    assert!(stats.contains("@charlie"));

    // leaderboard.ron persisted
    let lb = read_ron_leaderboard(&bot.data_dir);
    assert!(lb.contains_key("alice"));

    bot.shutdown().await;
}
```

### Shutdown

`bot.shutdown()` drops `shutdown_tx`; `run_bot` observes the closed channel and returns. Test awaits `bot_task` JoinHandle and surfaces any `Err` or panic — silent handler crashes cannot hide behind passing assertions.

## Error handling and flakiness controls

| Concern | Control |
|---|---|
| Assertion timeout | `expect_say(Duration)` panics if no message received within window (default 1s). |
| Spurious extra messages | `expect_silent(Duration)` panics if any message appears. |
| Channel overflow | `inject`/`capture` `mpsc` sized 64. Overflow panics the test. |
| Wall-clock flake | `#[tokio::test(start_paused = true)]` for clock-dependent tests; real time never elapses. |
| Task panics | `TestBot::shutdown` awaits `bot_task.await??` — `Err` or panic surfaced. |
| Port collisions | wiremock ephemeral ports; parallel-safe. |
| `$DATA_DIR` contamination | `TempDir` per test, cleaned on drop. |
| Wire format brittleness | `parse_privmsg_text(raw) -> String` strips `\r\n` and IRC framing; assertions work on message body only. |
| Flaky retries | None. Fix the test or the bot. |

## V1 Test Scenario List

### Tier 1 — no external deps

- `ping_trigger_member_fires` — member sends `!foo`, outgoing matches template with sender excluded
- `ping_trigger_cooldown` — second `!foo` within window returns cooldown reply; `expect_silent` on third within same window is not asserted (we verify the cooldown reply path)
- `ping_admin_create` — broadcaster `!p create foo "hi {mentions}"`, ping persisted to `pings.ron`
- `ping_admin_non_admin_rejected` — non-broadcaster `!p create` is silently ignored
- `leaderboard_read` — seed `leaderboard.ron`, `!lb` returns fastest PB
- `feedback_append` — `!fb hello`, assert line appended to `feedback.txt`

### Tier 2 — HTTP + LLM stubs

- `ai_command_reply` — `!ai ping`, `FakeLlm` returns "pong", outgoing == "pong"
- `ai_with_history` — inject 3 prior PRIVMSGs, `!ai` call; assert `FakeLlm` received system prompt with those messages in history block
- `ai_memory_extraction` — `!ai my name is alice`, `FakeLlm` tool call saves memory, assert `ai_memory.ron` contains fact
- `up_command_by_plz` — `!up 10115`, wiremock adsb.lol returns canned aircraft list, outgoing contains expected callsigns
- `track_command_phase_change` — `!track DLH1234`, wiremock adsb.lol returns takeoff then cruise snapshots across polls, assert phase-change chat messages in order

### Tier 3 — clock-dependent

- `tracker_1337_counts_users_and_posts_stats` — as above
- `tracker_1337_no_messages` — advance through 13:37 with zero injections, stats == "Erm" or "fuh"
- `tracker_1337_updates_leaderboard` — user says "1337" at 13:37:00.500, leaderboard.ron has entry with ms < 1000

### Deferred to v2

- Scheduled message fires on interval
- Flight tracker lost-threshold
- Latency monitor
- Config watcher reload

## Dependencies Added

New dev-dependencies in `Cargo.toml`:

- `wiremock` — HTTP mocking
- `tempfile` — `TempDir`
- Enable `test-util` feature for `tokio` in `[dev-dependencies.tokio]` — unlocks `tokio::time::pause` and `tokio::time::advance`. Production `tokio` feature set in `[dependencies]` stays minimal.

No new runtime dependencies. `Clock` trait is ~10 LOC, no crate needed.

Verification during implementation: confirm `twitch-irc` exposes `StaticLoginCredentials` and its `Transport` trait is `pub`. If not, the plan pivots to the trait-seam approach (Q2 option B) — re-open design.

## Build Sequence

Rough order (full plan produced separately via `writing-plans`):

1. Split `main.rs` → `main.rs` + `lib.rs`; move handlers into library crate.
2. Make `AuthenticatedTwitchClient` generic with default; propagate `<T: Transport>` through handler signatures.
3. Add `Clock` trait + `SystemClock`; thread into 1337, schedule, flight-tracker handlers.
4. Extract `AviationClient::new_with_base_url`.
5. Wire `HttpClients` / `Services` struct through `run_bot` signature (LLM, aviation client, data_dir, clock).
6. Add `tests/common/` harness: `FakeTransport`, `FakeClock`, `FakeLlm`, `TestBot`.
7. Tier 1 tests.
8. Tier 2 tests.
9. Tier 3 tests (1337 handler).
10. CI: `cargo test` runs all tiers.

## Success Criteria

- `cargo test` green on all v1 scenarios listed above.
- Tests run under 10 seconds total on dev machine (no real network, no real wall-clock waits).
- No flakes across 20 consecutive runs.
- `main.rs` compiles and deploys unchanged behaviorally (same handler set, same wiring, only source layout shifted).
- CI gate `cargo clippy --all-targets -- -D warnings` passes on test code.
