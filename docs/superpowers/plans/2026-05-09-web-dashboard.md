# Web Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an embedded Axum + Askama + HTMX web dashboard to the existing Rust Twitch bot, gated by Twitch OAuth + mod check, exposing v1 surfaces for ping CRUD and AI memory browse/edit.

**Architecture:** Same binary, server-rendered HTML, no SPA. Three-crate workspace split (`core` lib + new `web` lib + bin). Cloudflare Tunnel exposes the bind port. mtime-token guards memory edits against the dreamer ritual.

**Tech Stack:** Axum 0.8, Askama 0.14 (with `with-axum`), tower-http, tower-cookies, oauth2, rust-embed, humantime-serde, HTMX 2.x.

Spec: `docs/superpowers/specs/2026-05-09-web-dashboard-design.md`.

---

## File Structure

After this plan lands, the workspace looks like:

```
crates/
  core/                           # was crates/twitch-1337 (lib half)
    Cargo.toml                    # package "twitch-1337-core"
    src/
      lib.rs                      # run_bot, Services, re-exports
      config.rs                   # adds [web] WebConfig section
      ai/memory/store.rs          # adds write_with_guard + Mtime
      twitch/handlers/spawn.rs    # adds web task spawn
      twitch/handlers/latency.rs  # toggles irc_connected
      ...                         # all other modules unchanged
  llm/                            # unchanged
  web/                            # NEW
    Cargo.toml                    # package "twitch-1337-web"
    src/
      lib.rs                      # run_web, WebDeps
      state.rs                    # WebState
      auth/
        mod.rs                    # routes + middleware
        session.rs                # SessionTable + Session
        csrf.rs                   # token issue/verify
        mod_check.rs              # hidden_admins → broadcaster → helix
      helix.rs                    # HelixClient trait + reqwest impl
      error.rs                    # WebError
      routes/
        health.rs
        pings.rs
        memory.rs
        assets.rs
      flash.rs                    # 60s flash cookie
      templates/                  # Askama .html
        base.html
        sidebar.html
        auth/{login,denied}.html
        pings/{list,form,row}.html
        memory/{tree,editor,state_list,conflict}.html
      assets/
        htmx.min.js
        pico.min.css
        app.css
        app.js
  twitch-1337/                    # NEW: bin-only crate
    Cargo.toml                    # package "twitch-1337"
    src/main.rs                   # was crates/twitch-1337/src/main.rs; adds --healthcheck
```

Workspace root `Cargo.toml` lists all three; Justfile / Dockerfile retarget the bin path.

---

## Task 1: Crate split (no behavior change)

Mechanical refactor: move the current lib half of `crates/twitch-1337` into a new `crates/core/`, leave `main.rs` in a new bin-only `crates/twitch-1337/`. Branch: `build/crate-split`.

**Files:**
- Move: `crates/twitch-1337/{src/*,data,tests,build.rs,Cargo.toml}` → `crates/core/`
- Create: `crates/twitch-1337/Cargo.toml` (bin-only, depends on `twitch-1337-core`)
- Move: `crates/core/src/main.rs` → `crates/twitch-1337/src/main.rs`
- Modify: `Cargo.toml` (workspace members)
- Modify: `Justfile` (retarget `cargo run -p twitch-1337` paths if needed)
- Modify: `Dockerfile` (no path change since `cargo build -p twitch-1337` still works)
- Modify: `.cargo/config.toml` (no change expected)
- Modify: every `tests/**.rs` import to `use twitch_1337_core as twitch_1337;` shim or rename `use` paths

### Steps

- [ ] **Step 1: Create the new crate skeletons**

```bash
git checkout -b build/crate-split
mkdir -p crates/core/src
git mv crates/twitch-1337/build.rs crates/core/build.rs 2>/dev/null || true
git mv crates/twitch-1337/data crates/core/data
git mv crates/twitch-1337/tests crates/core/tests
# Move every src file EXCEPT main.rs into crates/core/src/
git mv crates/twitch-1337/src/lib.rs crates/core/src/
git mv crates/twitch-1337/src/ai crates/core/src/
git mv crates/twitch-1337/src/aviation crates/core/src/
git mv crates/twitch-1337/src/commands crates/core/src/
git mv crates/twitch-1337/src/config.rs crates/core/src/
git mv crates/twitch-1337/src/cooldown.rs crates/core/src/
git mv crates/twitch-1337/src/database.rs crates/core/src/
git mv crates/twitch-1337/src/llm_factory.rs crates/core/src/
git mv crates/twitch-1337/src/ping.rs crates/core/src/
git mv crates/twitch-1337/src/suspend.rs crates/core/src/
git mv crates/twitch-1337/src/twitch crates/core/src/
git mv crates/twitch-1337/src/util crates/core/src/
# main.rs stays under crates/twitch-1337/src/
```

- [ ] **Step 2: Write the new bin Cargo.toml**

Create `crates/twitch-1337/Cargo.toml`:

```toml
[package]
name = "twitch-1337"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
twitch-1337-core = { path = "../core" }
chrono = { workspace = true }
chrono-tz = { workspace = true }
color-eyre = { workspace = true }
secrecy = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 3: Rewrite the core Cargo.toml**

Move `crates/twitch-1337/Cargo.toml` → `crates/core/Cargo.toml` then edit:

```toml
[package]
name = "twitch-1337-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[features]
testing = []

[dependencies]
# (unchanged — every workspace dep the old crate had)
async-trait = { workspace = true }
# ...
llm = { path = "../llm" }
# ...

[dev-dependencies]
twitch-1337-core = { path = ".", features = ["testing"] }
# (every other dev-dep unchanged)
```

The old `dev-dependencies.twitch-1337` entry renames to `twitch-1337-core`.

- [ ] **Step 4: Update workspace members**

Edit `Cargo.toml` (root):

```toml
[workspace]
resolver = "3"
members = ["crates/core", "crates/llm", "crates/twitch-1337"]
```

- [ ] **Step 5: Update bin main.rs imports**

Edit `crates/twitch-1337/src/main.rs` line 8 — replace `use twitch_1337::{...}` with the same items but resolved through the renamed crate. Add to top:

```rust
use twitch_1337_core as twitch_1337;
```

This shim lets the rest of the file (including the `twitch_1337::Services` etc.) compile unchanged.

- [ ] **Step 6: Update integration test imports**

For each `crates/core/tests/**/*.rs` and `crates/core/tests/common/**/*.rs`, replace `use twitch_1337::...` with `use twitch_1337_core as twitch_1337;` at the top, OR globally rename the import path. The shim is the smaller diff:

```bash
for f in $(grep -rl "use twitch_1337::" crates/core/tests); do
    sed -i '1i use twitch_1337_core as twitch_1337;' "$f"
done
```

(Manually verify no double-imports after.)

- [ ] **Step 7: Build and run full test suite**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS, identical pre-split test count.

- [ ] **Step 8: Verify the bin still builds the same**

```bash
cargo build -p twitch-1337 --release
ls -lh target/release/twitch-1337
```

Expected: file exists, size comparable to pre-split.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
build: split twitch-1337 into core lib + bin crates

Moves the lib half (everything except main.rs) into a new
twitch-1337-core crate at crates/core/, leaves the bin as
twitch-1337 at crates/twitch-1337/. Mechanical move; no behavior
change. Tests retarget the renamed crate via a one-line shim.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 10: Open PR + verify CI**

```bash
git push -u origin build/crate-split
gh pr create --title "build: split twitch-1337 into core lib + bin crates" --body "Mechanical split per spec. Lib content moves to crates/core/ (package twitch-1337-core); bin stays at crates/twitch-1337/. No behavior change. Prep for [web] crate addition."
```

Wait for all 7 required checks green before merging.

---

## Task 2: Stub web crate + healthcheck plumbing

Adds `crates/web/` with only `/healthz`, the new `[web]` config section (parsed but optional), the `irc_connected: Arc<AtomicBool>` shared with the latency monitor, and the `--healthcheck` bin subcommand. No auth, no UI templates yet. Branch: `feature/web-stub`.

**Files:**
- Create: `crates/web/Cargo.toml`
- Create: `crates/web/src/lib.rs`
- Create: `crates/web/src/state.rs`
- Create: `crates/web/src/error.rs`
- Create: `crates/web/src/routes/health.rs`
- Modify: `Cargo.toml` (workspace members + workspace deps)
- Modify: `crates/core/Cargo.toml` (depend on `humantime-serde`)
- Modify: `crates/core/src/config.rs` (add `WebConfig`)
- Modify: `crates/core/src/lib.rs` (spawn web task; new `irc_connected` field on `Services`)
- Modify: `crates/core/src/twitch/handlers/spawn.rs` (thread `irc_connected` to latency)
- Modify: `crates/core/src/twitch/handlers/latency.rs` (set/clear `irc_connected`)
- Modify: `crates/twitch-1337/src/main.rs` (parse `--healthcheck`, build `irc_connected`)
- Modify: `Dockerfile` (HEALTHCHECK line)
- Modify: `config.toml.example` (commented `[web]` section)
- Test: `crates/web/tests/healthz.rs`
- Test: `crates/core/tests/web_smoke.rs`

### Steps

- [ ] **Step 1: Add workspace deps**

Edit root `Cargo.toml` `[workspace.dependencies]` (alphabetical):

```toml
askama = { version = "0.14", features = ["with-axum"] }
askama_axum = "0.14"
axum = { version = "0.8", default-features = false, features = ["http1", "json", "tokio", "tower-log", "tracing"] }
humantime-serde = "1"
oauth2 = { version = "5", default-features = false, features = ["reqwest", "rustls-tls"] }
rust-embed = { version = "8", features = ["axum-ex"] }
tower = { version = "0.5", features = ["util"] }
tower-cookies = { version = "0.11", features = ["signed", "private"] }
tower-http = { version = "0.6", features = ["fs", "trace"] }
url = "2"
```

Also add to root `[workspace] members`:

```toml
members = ["crates/core", "crates/llm", "crates/twitch-1337", "crates/web"]
```

- [ ] **Step 2: Create the web crate skeleton**

Create `crates/web/Cargo.toml`:

```toml
[package]
name = "twitch-1337-web"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
askama = { workspace = true }
askama_axum = { workspace = true }
async-trait = { workspace = true }
axum = { workspace = true }
chrono = { workspace = true }
eyre = { workspace = true }
serde = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tower = { workspace = true }
tower-http = { workspace = true }
tracing = { workspace = true }
twitch-1337-core = { path = "../core" }

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "test-util"] }

[lints]
workspace = true
```

- [ ] **Step 3: Write the failing healthz test**

Create `crates/web/tests/healthz.rs`:

```rust
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use twitch_1337_web::routes::health::router;

#[tokio::test]
async fn healthz_returns_200_when_irc_connected() {
    let flag = Arc::new(AtomicBool::new(true));
    let app = router(flag.clone());
    let res = app
        .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthz_returns_503_when_irc_disconnected() {
    let flag = Arc::new(AtomicBool::new(false));
    let app = router(flag.clone());
    let res = app
        .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}
```

- [ ] **Step 4: Run test — should fail to compile**

```bash
cargo test -p twitch-1337-web --test healthz 2>&1 | head -20
```

Expected: errors about missing `routes::health::router` / unresolved `twitch_1337_web`.

- [ ] **Step 5: Implement `routes::health`**

Create `crates/web/src/routes/health.rs`:

```rust
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

pub fn router(irc_connected: Arc<AtomicBool>) -> Router {
    Router::new().route("/healthz", get(move || {
        let flag = irc_connected.clone();
        async move {
            if flag.load(Ordering::Relaxed) {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }))
}
```

- [ ] **Step 6: Implement minimal `lib.rs`**

Create `crates/web/src/lib.rs`:

```rust
//! Embedded web dashboard for the twitch-1337 bot. v1 surfaces:
//! /healthz only; auth + ping + memory routes land in later tasks.

pub mod error;
pub mod routes;
pub mod state;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eyre::{Result, WrapErr as _};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tracing::{info, warn};

pub struct WebDeps {
    pub bind_addr: SocketAddr,
    pub irc_connected: Arc<AtomicBool>,
}

pub async fn run_web(deps: WebDeps, shutdown: Arc<Notify>) -> Result<()> {
    let app = routes::health::router(deps.irc_connected);
    let listener = TcpListener::bind(deps.bind_addr)
        .await
        .wrap_err_with(|| format!("bind {}", deps.bind_addr))?;
    info!(target: "twitch_1337_web", addr = %deps.bind_addr, "Web dashboard listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await
        .wrap_err("web serve")?;
    warn!(target: "twitch_1337_web", "Web dashboard stopped");
    Ok(())
}
```

Create `crates/web/src/routes/mod.rs`:

```rust
pub mod health;
```

Create `crates/web/src/state.rs` (placeholder for now):

```rust
//! WebState lands in the auth task; this file exists so the module path is
//! reserved before later tasks land.
```

Create `crates/web/src/error.rs`:

```rust
//! WebError lands in the auth task; placeholder module.
```

- [ ] **Step 7: Run healthz test — should pass**

```bash
cargo test -p twitch-1337-web --test healthz
```

Expected: 2 passed.

- [ ] **Step 8: Add `WebConfig` to core config**

Edit `crates/core/Cargo.toml` to add `humantime-serde = { workspace = true }` to `[dependencies]`.

Edit `crates/core/src/config.rs`. Add at module top-level (after the existing structs):

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_bind")]
    pub bind_addr: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default)]
    pub session_secret: SecretString,
    #[serde(default = "default_session_ttl", with = "humantime_serde")]
    pub session_ttl: std::time::Duration,
    #[serde(default = "default_mod_check_refresh", with = "humantime_serde")]
    pub mod_check_refresh: std::time::Duration,
}

fn default_web_bind() -> String { "127.0.0.1:8080".to_owned() }
fn default_session_ttl() -> std::time::Duration { std::time::Duration::from_secs(7 * 24 * 60 * 60) }
fn default_mod_check_refresh() -> std::time::Duration { std::time::Duration::from_secs(300) }

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: default_web_bind(),
            public_url: String::new(),
            session_secret: SecretString::new(String::new().into()),
            session_ttl: default_session_ttl(),
            mod_check_refresh: default_mod_check_refresh(),
        }
    }
}
```

Add `pub web: WebConfig,` to `Configuration`:

```rust
pub struct Configuration {
    pub twitch: TwitchConfiguration,
    // existing fields…
    #[serde(default)]
    pub web: WebConfig,
}
```

Update `Configuration::test_default` (around line 547) to include `web: WebConfig::default(),`.

- [ ] **Step 9: Add web validation**

In `validate_config` (`crates/core/src/config.rs`), append:

```rust
if config.web.enabled {
    let secret = config.web.session_secret.expose_secret();
    if hex::decode(secret).map(|b| b.len() < 32).unwrap_or(true) {
        bail!("web.session_secret must be ≥32 bytes hex when web.enabled = true");
    }
    if !config.web.public_url.starts_with("https://") {
        bail!("web.public_url must start with https:// when web.enabled = true (got {:?})", config.web.public_url);
    }
    let ttl = config.web.session_ttl.as_secs();
    if !(3600..=2_592_000).contains(&ttl) {
        bail!("web.session_ttl must be between 1h and 30d (got {ttl}s)");
    }
    let refresh = config.web.mod_check_refresh.as_secs();
    if !(30..=3600).contains(&refresh) {
        bail!("web.mod_check_refresh must be between 30s and 1h (got {refresh}s)");
    }
}
```

Add `hex = "0.4"` to root workspace deps and to `crates/core/Cargo.toml`.

- [ ] **Step 10: Write config tests for web validation**

Add to the `tests` module at the bottom of `crates/core/src/config.rs`:

```rust
#[test]
fn web_disabled_skips_validation() {
    let cfg = Configuration::test_default();
    assert!(!cfg.web.enabled);
    validate_config(&cfg).expect("disabled web validates trivially");
}

#[test]
fn web_enabled_requires_https_public_url() {
    let mut cfg = Configuration::test_default();
    cfg.web.enabled = true;
    cfg.web.session_secret = secrecy::SecretString::new("00".repeat(32).into());
    cfg.web.public_url = "http://insecure".into();
    let err = validate_config(&cfg).unwrap_err().to_string();
    assert!(err.contains("public_url"), "{err}");
}

#[test]
fn web_enabled_requires_32_byte_secret() {
    let mut cfg = Configuration::test_default();
    cfg.web.enabled = true;
    cfg.web.session_secret = secrecy::SecretString::new("ab".into());
    cfg.web.public_url = "https://bot.test".into();
    let err = validate_config(&cfg).unwrap_err().to_string();
    assert!(err.contains("session_secret"), "{err}");
}
```

Run:

```bash
cargo nextest run -p twitch-1337-core config::tests::web_
```

Expected: 3 passed.

- [ ] **Step 11: Add `irc_connected` to `Services` and plumb to latency**

Edit `crates/core/src/lib.rs`. In `Services`:

```rust
pub struct Services {
    pub clock: Arc<dyn Clock>,
    // existing…
    pub irc_connected: Arc<std::sync::atomic::AtomicBool>,
}
```

Threaded into `SpawnDeps` and `run_latency_handler`:

Edit `crates/core/src/twitch/handlers/spawn.rs`:
- Add `pub irc_connected: Arc<AtomicBool>` to `SpawnDeps`.
- Pass it to `run_latency_handler`.

Edit `crates/core/src/twitch/handlers/latency.rs`. Change signature:

```rust
pub async fn run_latency_handler<T, L>(
    client: Arc<TwitchIRCClient<T, L>>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
    latency: Arc<AtomicU32>,
    irc_connected: Arc<std::sync::atomic::AtomicBool>,
) where
    T: Transport,
    L: LoginCredentials,
{
    use std::sync::atomic::Ordering;

    irc_connected.store(true, Ordering::Relaxed);
    info!(initial_latency_ms = latency.load(Ordering::Relaxed), "Latency handler started");

    let mut consecutive_misses: u32 = 0;
    // existing init…
    loop {
        sleep(LATENCY_PING_INTERVAL).await;
        // existing PING + PONG wait…
        match pong_result {
            Ok(rtt) => {
                consecutive_misses = 0;
                irc_connected.store(true, Ordering::Relaxed);
                // existing EMA update…
            }
            Err(_timeout) => {
                consecutive_misses += 1;
                if consecutive_misses >= 3 {
                    irc_connected.store(false, Ordering::Relaxed);
                    warn!("3 consecutive PONG timeouts; marking IRC disconnected");
                }
            }
        }
    }
}
```

(Insert the `consecutive_misses` counter inside the existing loop; do not duplicate the existing PING/PONG block — read the surrounding code and adapt.)

- [ ] **Step 12: Spawn web task from `run_bot`**

Edit `crates/core/src/lib.rs::run_bot`. Capture `irc_connected` from `services`. After `spawn_handlers` returns and before the `tokio::select!` exit arm, add:

```rust
let web_handle = if config.web.enabled {
    let bind_addr: std::net::SocketAddr = config.web.bind_addr.parse()
        .wrap_err("parse web.bind_addr")?;
    let deps = twitch_1337_web::WebDeps {
        bind_addr,
        irc_connected: services_irc_connected.clone(),
    };
    let notify = handlers.shutdown_notify.clone();
    Some(tokio::spawn(async move {
        if let Err(e) = twitch_1337_web::run_web(deps, notify).await {
            tracing::error!(?error = e.as_ref() as &dyn std::error::Error, "Web task exited with error");
        }
    }))
} else {
    None
};
```

Where `services_irc_connected` is captured before destructuring `services`.

Add `twitch-1337-web = { path = "../web" }` to `crates/core/Cargo.toml` `[dependencies]`.

Update `await_shutdown` (in `spawn.rs`) to also await `web_handle` if present, with a 5s timeout.

- [ ] **Step 13: Wire `irc_connected` from main.rs**

Edit `crates/twitch-1337/src/main.rs`. Build the flag and pass it in:

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

let irc_connected = Arc::new(AtomicBool::new(false));

let services = Services {
    clock: Arc::new(SystemClock),
    // existing…
    irc_connected: irc_connected.clone(),
};
```

- [ ] **Step 14: Add `--healthcheck` to main.rs**

Replace the top of `main()` in `crates/twitch-1337/src/main.rs`:

```rust
#[tokio::main]
pub async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        return run_healthcheck().await;
    }

    color_eyre::install()?;
    install_tracing();
    install_crypto_provider();

    let config = load_configuration().await?;
    // existing…
}

async fn run_healthcheck() -> Result<()> {
    let config = load_configuration().await?;
    if !config.web.enabled {
        return Ok(());
    }
    let url = format!("http://127.0.0.1:{}/healthz",
        config.web.bind_addr.rsplit(':').next().unwrap_or("8080"));
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?
        .get(&url)
        .send()
        .await?;
    if res.status().is_success() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
```

Add `reqwest = { workspace = true }` to `crates/twitch-1337/Cargo.toml`.

- [ ] **Step 15: Update Dockerfile HEALTHCHECK**

Edit `Dockerfile`. After the `COPY --from=builder /app/target/.../twitch-1337 /twitch-1337` line, before `ENTRYPOINT`:

```dockerfile
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/twitch-1337", "--healthcheck"]
```

- [ ] **Step 16: Document [web] in config.toml.example**

Append to `crates/core/data/config.toml.example` (or wherever the example lives — `find . -name 'config.toml.example'`):

```toml
# [web]                            # uncomment to enable the dashboard
# enabled = true
# bind_addr = "0.0.0.0:8080"
# public_url = "https://bot.example.com"
# session_secret = "<32+ bytes of hex>"
# session_ttl = "7d"
# mod_check_refresh = "5m"
```

- [ ] **Step 17: Write integration smoke test**

Create `crates/core/tests/web_smoke.rs`:

```rust
use std::time::Duration;

mod common;

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn web_healthz_responds_when_enabled() {
    let mut builder = common::TestBotBuilder::new();
    builder = builder.with_web("127.0.0.1:18080");
    let bot = builder.spawn().await;

    // Force IRC connected so /healthz returns 200
    bot.set_irc_connected(true);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let res = reqwest::Client::new()
        .get("http://127.0.0.1:18080/healthz")
        .send()
        .await
        .expect("connect");
    assert_eq!(res.status(), 200);

    bot.shutdown().await;
}
```

Add `with_web()` and `set_irc_connected()` helpers to `crates/core/tests/common/test_bot.rs`:

```rust
pub fn with_web(mut self, bind: &str) -> Self {
    self.config.web.enabled = true;
    self.config.web.bind_addr = bind.into();
    self.config.web.session_secret = secrecy::SecretString::new("0".repeat(64).into());
    self.config.web.public_url = "https://test.invalid".into();
    self
}

pub fn set_irc_connected(&self, v: bool) {
    self.irc_connected.store(v, std::sync::atomic::Ordering::Relaxed);
}
```

(Add `irc_connected: Arc<AtomicBool>` field on `TestBot` and capture it during `spawn`.)

- [ ] **Step 18: Run full suite**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
```

Expected: all green including the new healthz tests.

- [ ] **Step 19: Manual smoke**

```bash
echo '[web]
enabled = true
bind_addr = "127.0.0.1:18080"
public_url = "https://localhost"
session_secret = "'$(openssl rand -hex 32)'"
session_ttl = "7d"
mod_check_refresh = "5m"' >> $DATA_DIR/config.toml  # or merge manually

cargo run -p twitch-1337
# in another shell:
curl -i http://127.0.0.1:18080/healthz
```

Expected: 503 immediately after start, 200 once IRC connects (latency log line).

- [ ] **Step 20: Commit + PR**

```bash
git checkout -b feature/web-stub
git add -A
git commit -m "$(cat <<'EOF'
feat(web): stub embedded web crate with /healthz + --healthcheck

Adds crates/web with a single /healthz endpoint backed by a new
irc_connected Arc<AtomicBool> driven by the latency monitor (3-miss
threshold). Bot binary gains a --healthcheck subcommand used by the
Dockerfile HEALTHCHECK so probes work in FROM scratch without curl.
[web] config section is parsed and validated; everything is opt-in.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push -u origin feature/web-stub
gh pr create --title "feat(web): stub embedded web crate with /healthz" --body "First slice of the web dashboard spec. Adds the crate, the /healthz route, the [web] config section, the irc_connected flag, and the --healthcheck subcommand. No auth or UI yet."
```

---

## Task 3: Auth (OAuth + sessions + CSRF + mod check + base layout)

Adds Twitch OAuth login, session table, CSRF token plumbing, mod-gate middleware, the `HelixClient` trait, base templates with sidebar shell, and login/denied pages. Branch: `feature/web-auth`.

**Files:**
- Create: `crates/web/src/helix.rs`
- Create: `crates/web/src/auth/mod.rs`
- Create: `crates/web/src/auth/session.rs`
- Create: `crates/web/src/auth/csrf.rs`
- Create: `crates/web/src/auth/mod_check.rs`
- Create: `crates/web/src/error.rs` (full `WebError` enum)
- Create: `crates/web/src/state.rs` (full `WebState`)
- Create: `crates/web/src/flash.rs`
- Create: `crates/web/src/routes/assets.rs`
- Create: `crates/web/src/templates/base.html`
- Create: `crates/web/src/templates/sidebar.html`
- Create: `crates/web/src/templates/auth/login.html`
- Create: `crates/web/src/templates/auth/denied.html`
- Create: `crates/web/src/assets/{htmx.min.js,pico.min.css,app.css,app.js}`
- Modify: `crates/web/src/lib.rs` (full `WebDeps`, mount auth routes)
- Modify: `crates/web/Cargo.toml` (oauth2, rust-embed, tower-cookies, url, reqwest, etc.)
- Modify: `crates/core/src/lib.rs` (build helix client + WebDeps)
- Test: `crates/web/tests/auth_session.rs`
- Test: `crates/web/tests/auth_csrf.rs`
- Test: `crates/web/tests/auth_mod_check.rs`
- Test: `crates/web/tests/auth_routes.rs`
- Test: `crates/web/tests/helix_pagination.rs`

### Steps

- [ ] **Step 1: Add web crate deps**

Edit `crates/web/Cargo.toml` `[dependencies]`:

```toml
oauth2 = { workspace = true }
reqwest = { workspace = true }
rust-embed = { workspace = true }
secrecy = { workspace = true }
serde_json = { workspace = true }
rand = { workspace = true }
tower-cookies = { workspace = true }
url = { workspace = true }
hex = "0.4"
```

- [ ] **Step 2: Write the failing session test**

Create `crates/web/tests/auth_session.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use twitch_1337_core::util::clock::Clock;
use twitch_1337_web::auth::session::{Session, SessionTable};

struct StubClock(std::sync::Mutex<chrono::DateTime<Utc>>);
impl Clock for StubClock {
    fn now(&self) -> chrono::DateTime<Utc> { *self.0.lock().unwrap() }
}
impl StubClock { fn advance(&self, secs: i64) { let mut g = self.0.lock().unwrap(); *g = *g + chrono::Duration::seconds(secs); } }

#[test]
fn session_round_trips() {
    let clock = Arc::new(StubClock(std::sync::Mutex::new(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())));
    let table = SessionTable::new(Duration::from_secs(7 * 24 * 3600), clock.clone());
    let id = table.insert("12345".into(), "alice".into()).expect("insert");
    let got = table.get_and_touch(&id).expect("present");
    assert_eq!(got.user_login, "alice");
    assert_eq!(got.user_id, "12345");
}

#[test]
fn session_expires_after_ttl() {
    let clock = Arc::new(StubClock(std::sync::Mutex::new(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())));
    let table = SessionTable::new(Duration::from_secs(60), clock.clone());
    let id = table.insert("12345".into(), "alice".into()).unwrap();
    clock.advance(61);
    assert!(table.get_and_touch(&id).is_none(), "expected expiry past TTL");
}

#[test]
fn session_sliding_refresh_keeps_alive() {
    let clock = Arc::new(StubClock(std::sync::Mutex::new(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())));
    let table = SessionTable::new(Duration::from_secs(120), clock.clone());
    let id = table.insert("12345".into(), "alice".into()).unwrap();
    clock.advance(60);
    assert!(table.get_and_touch(&id).is_some());  // bumps last_seen
    clock.advance(90);
    assert!(table.get_and_touch(&id).is_some(), "sliding refresh should keep alive");
    clock.advance(150);
    assert!(table.get_and_touch(&id).is_none());
}
```

- [ ] **Step 3: Run — fails to compile**

```bash
cargo test -p twitch-1337-web --test auth_session 2>&1 | head -10
```

Expected: missing module errors.

- [ ] **Step 4: Implement session table**

Create `crates/web/src/auth/mod.rs`:

```rust
pub mod csrf;
pub mod mod_check;
pub mod session;

mod routes;
pub use routes::{auth_router, require_mod};
```

Create `crates/web/src/auth/session.rs`:

```rust
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::RngExt as _;
use rand::rngs::OsRng;

use twitch_1337_core::util::clock::Clock;

pub type SessionId = String;

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: String,
    pub user_login: String,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_mod_check: DateTime<Utc>,
    pub csrf_value: [u8; 32],
}

pub struct SessionTable {
    inner: RwLock<HashMap<SessionId, Session>>,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl SessionTable {
    pub fn new(ttl: Duration, clock: Arc<dyn Clock>) -> Self {
        Self { inner: RwLock::new(HashMap::new()), ttl, clock }
    }

    pub fn insert(&self, user_id: String, user_login: String) -> eyre::Result<SessionId> {
        let now = self.clock.now();
        let mut id_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut id_bytes);
        let mut csrf = [0u8; 32];
        OsRng.fill_bytes(&mut csrf);
        let id = hex::encode(id_bytes);
        self.inner.write().unwrap().insert(id.clone(), Session {
            user_id,
            user_login,
            issued_at: now,
            last_seen: now,
            last_mod_check: now,
            csrf_value: csrf,
        });
        Ok(id)
    }

    pub fn get_and_touch(&self, id: &str) -> Option<Session> {
        let now = self.clock.now();
        let ttl = chrono::Duration::from_std(self.ttl).ok()?;
        let mut g = self.inner.write().unwrap();
        let session = g.get_mut(id)?;
        if now.signed_duration_since(session.last_seen) > ttl {
            g.remove(id);
            return None;
        }
        session.last_seen = now;
        Some(session.clone())
    }

    pub fn drop_session(&self, id: &str) {
        self.inner.write().unwrap().remove(id);
    }

    pub fn record_mod_check(&self, id: &str) {
        let now = self.clock.now();
        if let Some(s) = self.inner.write().unwrap().get_mut(id) {
            s.last_mod_check = now;
        }
    }
}
```

- [ ] **Step 5: Run session test — should pass**

```bash
cargo test -p twitch-1337-web --test auth_session
```

Expected: 3 passed.

- [ ] **Step 6: CSRF tests + impl**

Create `crates/web/tests/auth_csrf.rs`:

```rust
use twitch_1337_web::auth::csrf;

#[test]
fn token_round_trips() {
    let token = [42u8; 32];
    let encoded = csrf::encode(&token);
    let decoded = csrf::decode(&encoded).expect("ok");
    assert_eq!(decoded, token);
}

#[test]
fn verify_accepts_match() {
    let token = [7u8; 32];
    let encoded = csrf::encode(&token);
    assert!(csrf::verify(&encoded, &token));
}

#[test]
fn verify_rejects_mismatch() {
    let token = [7u8; 32];
    let other = [8u8; 32];
    assert!(!csrf::verify(&csrf::encode(&token), &other));
}

#[test]
fn verify_rejects_garbage() {
    assert!(!csrf::verify("not-hex", &[0u8; 32]));
}
```

Create `crates/web/src/auth/csrf.rs`:

```rust
pub fn encode(value: &[u8; 32]) -> String { hex::encode(value) }
pub fn decode(encoded: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(encoded).ok()?;
    bytes.try_into().ok()
}
pub fn verify(encoded: &str, expected: &[u8; 32]) -> bool {
    decode(encoded)
        .map(|got| constant_time_eq(&got, expected))
        .unwrap_or(false)
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
```

Run:

```bash
cargo test -p twitch-1337-web --test auth_csrf
```

Expected: 4 passed.

- [ ] **Step 7: HelixClient trait + reqwest impl**

Create `crates/web/src/helix.rs`:

```rust
//! Minimal Twitch helix client (broadcaster id, moderator list, user lookup).
//! Mirrors the AviationClient pattern in core for testability — boxed behind
//! a trait so route tests can inject fakes.

use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr as _, eyre};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

#[async_trait]
pub trait HelixClient: Send + Sync {
    async fn fetch_user_by_id(&self, user_id: &str) -> Result<Option<HelixUser>>;
    async fn fetch_user_by_login(&self, login: &str) -> Result<Option<HelixUser>>;
    /// Follows pagination until exhausted; returns true if `user_id` is in the moderator list.
    async fn is_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<bool>;
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelixUser {
    pub id: String,
    pub login: String,
    pub display_name: String,
}

pub struct ReqwestHelixClient {
    pub http: reqwest::Client,
    pub client_id: SecretString,
    pub access_token_provider: Arc<dyn AccessTokenProvider>,
}

#[async_trait]
pub trait AccessTokenProvider: Send + Sync {
    async fn current_access_token(&self) -> Result<String>;
}

#[async_trait]
impl HelixClient for ReqwestHelixClient {
    async fn fetch_user_by_id(&self, user_id: &str) -> Result<Option<HelixUser>> {
        self.fetch_user(&[("id", user_id)]).await
    }
    async fn fetch_user_by_login(&self, login: &str) -> Result<Option<HelixUser>> {
        self.fetch_user(&[("login", login)]).await
    }
    async fn is_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Mod { user_id: String }
        #[derive(Deserialize)]
        struct Pagination { cursor: Option<String> }
        #[derive(Deserialize)]
        struct ModResp { data: Vec<Mod>, pagination: Option<Pagination> }

        let mut cursor: Option<String> = None;
        loop {
            let mut url = url::Url::parse("https://api.twitch.tv/helix/moderation/moderators")?;
            url.query_pairs_mut()
                .append_pair("broadcaster_id", broadcaster_id)
                .append_pair("first", "100");
            if let Some(c) = &cursor {
                url.query_pairs_mut().append_pair("after", c);
            }
            let token = self.access_token_provider.current_access_token().await?;
            let resp: ModResp = self.http.get(url)
                .bearer_auth(&token)
                .header("Client-Id", self.client_id.expose_secret())
                .send().await?
                .error_for_status().wrap_err("helix moderators")?
                .json().await?;
            if resp.data.iter().any(|m| m.user_id == user_id) {
                return Ok(true);
            }
            match resp.pagination.and_then(|p| p.cursor) {
                Some(c) if !c.is_empty() => { cursor = Some(c); }
                _ => return Ok(false),
            }
        }
    }
}

impl ReqwestHelixClient {
    async fn fetch_user(&self, query: &[(&str, &str)]) -> Result<Option<HelixUser>> {
        #[derive(Deserialize)]
        struct UserResp { data: Vec<HelixUser> }
        let mut url = url::Url::parse("https://api.twitch.tv/helix/users")?;
        for (k, v) in query { url.query_pairs_mut().append_pair(k, v); }
        let token = self.access_token_provider.current_access_token().await?;
        let resp: UserResp = self.http.get(url)
            .bearer_auth(&token)
            .header("Client-Id", self.client_id.expose_secret())
            .send().await?
            .error_for_status().wrap_err("helix users")?
            .json().await?;
        Ok(resp.data.into_iter().next())
    }
}
```

- [ ] **Step 8: Helix pagination test (with wiremock)**

Create `crates/web/tests/helix_pagination.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use twitch_1337_web::helix::{AccessTokenProvider, HelixClient, ReqwestHelixClient};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path, query_param}};

struct StubToken;
#[async_trait]
impl AccessTokenProvider for StubToken {
    async fn current_access_token(&self) -> eyre::Result<String> { Ok("test-token".into()) }
}

#[tokio::test]
async fn is_moderator_follows_cursor() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/helix/moderation/moderators"))
        .and(query_param("broadcaster_id", "100"))
        .and(query_param("first", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "user_id": "999", "user_login": "other", "user_name": "Other" }],
            "pagination": { "cursor": "page2" }
        })))
        .up_to_n_times(1)
        .mount(&server).await;

    Mock::given(method("GET"))
        .and(path("/helix/moderation/moderators"))
        .and(query_param("after", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "user_id": "12345", "user_login": "alice", "user_name": "Alice" }],
            "pagination": {}
        })))
        .mount(&server).await;

    // Override the helix base URL via a wrapper would be cleaner; this test
    // uses the real ReqwestHelixClient with a `helix_base` field. Add that
    // field next.
    let client = ReqwestHelixClient::with_base(
        reqwest::Client::new(),
        secrecy::SecretString::new("client-id".into()),
        Arc::new(StubToken),
        server.uri(),
    );
    assert!(client.is_moderator("100", "12345").await.unwrap());
}
```

Modify `ReqwestHelixClient` to accept a configurable base URL:

```rust
pub struct ReqwestHelixClient {
    pub http: reqwest::Client,
    pub client_id: SecretString,
    pub access_token_provider: Arc<dyn AccessTokenProvider>,
    pub helix_base: String,  // "https://api.twitch.tv" in prod, mock URI in tests
}

impl ReqwestHelixClient {
    pub fn new(http: reqwest::Client, client_id: SecretString, provider: Arc<dyn AccessTokenProvider>) -> Self {
        Self::with_base(http, client_id, provider, "https://api.twitch.tv".into())
    }
    pub fn with_base(http: reqwest::Client, client_id: SecretString, provider: Arc<dyn AccessTokenProvider>, base: String) -> Self {
        Self { http, client_id, access_token_provider: provider, helix_base: base }
    }
}
```

Replace every `url::Url::parse("https://api.twitch.tv/...")` with `format!("{}/...", self.helix_base)`.

Add to `crates/web/Cargo.toml` `[dev-dependencies]`:

```toml
wiremock = "0.6"
```

Run:

```bash
cargo test -p twitch-1337-web --test helix_pagination
```

Expected: PASS.

- [ ] **Step 9: Mod check test + impl**

Create `crates/web/tests/auth_mod_check.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use twitch_1337_web::auth::mod_check::{ModCheckOutcome, check_is_mod};
use twitch_1337_web::helix::{HelixClient, HelixUser};

struct FakeHelix {
    moderators: Vec<String>,
    users: std::collections::HashMap<String, HelixUser>,
}

#[async_trait]
impl HelixClient for FakeHelix {
    async fn fetch_user_by_id(&self, id: &str) -> eyre::Result<Option<HelixUser>> { Ok(self.users.get(id).cloned()) }
    async fn fetch_user_by_login(&self, login: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(self.users.values().find(|u| u.login == login).cloned())
    }
    async fn is_moderator(&self, _broadcaster: &str, user_id: &str) -> eyre::Result<bool> { Ok(self.moderators.contains(&user_id.to_string())) }
}

#[tokio::test]
async fn hidden_admin_short_circuits() {
    let helix = FakeHelix { moderators: vec![], users: Default::default() };
    let outcome = check_is_mod(&helix, "12345", "200", &["12345".into()]).await.unwrap();
    assert!(matches!(outcome, ModCheckOutcome::Allow));
}

#[tokio::test]
async fn broadcaster_short_circuits() {
    let helix = FakeHelix { moderators: vec![], users: Default::default() };
    let outcome = check_is_mod(&helix, "200", "200", &[]).await.unwrap();
    assert!(matches!(outcome, ModCheckOutcome::Allow));
}

#[tokio::test]
async fn moderator_path_admits() {
    let helix = FakeHelix { moderators: vec!["999".into()], users: Default::default() };
    let outcome = check_is_mod(&helix, "999", "200", &[]).await.unwrap();
    assert!(matches!(outcome, ModCheckOutcome::Allow));
}

#[tokio::test]
async fn non_mod_denied() {
    let helix = FakeHelix { moderators: vec![], users: Default::default() };
    let outcome = check_is_mod(&helix, "555", "200", &[]).await.unwrap();
    assert!(matches!(outcome, ModCheckOutcome::Deny));
}
```

Create `crates/web/src/auth/mod_check.rs`:

```rust
use crate::helix::HelixClient;

pub enum ModCheckOutcome {
    Allow,
    Deny,
}

pub async fn check_is_mod(
    helix: &dyn HelixClient,
    user_id: &str,
    broadcaster_id: &str,
    hidden_admins: &[String],
) -> eyre::Result<ModCheckOutcome> {
    if hidden_admins.iter().any(|s| s == user_id) {
        return Ok(ModCheckOutcome::Allow);
    }
    if user_id == broadcaster_id {
        return Ok(ModCheckOutcome::Allow);
    }
    if helix.is_moderator(broadcaster_id, user_id).await? {
        return Ok(ModCheckOutcome::Allow);
    }
    Ok(ModCheckOutcome::Deny)
}
```

Run:

```bash
cargo test -p twitch-1337-web --test auth_mod_check
```

Expected: 4 passed.

- [ ] **Step 10: Define `WebError` enum**

Replace `crates/web/src/error.rs`:

```rust
use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("unauthenticated; redirect to login")]
    Unauthenticated { next: String },
    #[error("forbidden")]
    Forbidden,
    #[error("csrf mismatch")]
    CsrfMismatch,
    #[error("validation: {field}: {msg}")]
    Validation { field: String, msg: String },
    #[error("duplicate name: {name}")]
    DuplicateName { name: String },
    #[error("conflict")]
    Conflict { kind: String, id: String, current_body: String, current_mtime: u64, draft: String },
    #[error("oauth exchange: {0}")]
    OAuthExchange(String),
    #[error("internal: {0}")]
    Internal(#[from] eyre::Report),
}

#[derive(Template)]
#[template(path = "auth/denied.html")]
struct DeniedTpl;

#[derive(Template)]
#[template(path = "memory/conflict.html")]
struct ConflictTpl<'a> { kind: &'a str, id: &'a str, current_body: &'a str, current_mtime: u64, draft: &'a str }

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        match self {
            WebError::Unauthenticated { next } => Redirect::to(&format!("/login?next={}", urlencoding::encode(&next))).into_response(),
            WebError::Forbidden => (StatusCode::FORBIDDEN, DeniedTpl).into_response(),
            WebError::CsrfMismatch => (StatusCode::FORBIDDEN, "Session expired, reload and try again").into_response(),
            WebError::Validation { field, msg } => (StatusCode::BAD_REQUEST, format!("validation: {field}: {msg}")).into_response(),
            WebError::DuplicateName { name } => (StatusCode::BAD_REQUEST, format!("ping `{name}` already exists")).into_response(),
            WebError::Conflict { kind, id, current_body, current_mtime, draft } => (
                StatusCode::CONFLICT,
                ConflictTpl { kind: &kind, id: &id, current_body: &current_body, current_mtime, draft: &draft }
            ).into_response(),
            WebError::OAuthExchange(msg) => (StatusCode::BAD_GATEWAY, format!("oauth exchange failed: {msg}")).into_response(),
            WebError::Internal(err) => {
                tracing::error!(target: "twitch_1337_web", ?error = err.as_ref() as &dyn std::error::Error, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
```

Add `urlencoding = "2"` to web crate deps.

- [ ] **Step 11: Define `WebState`**

Replace `crates/web/src/state.rs`:

```rust
use std::sync::{Arc, RwLock, atomic::AtomicBool};

use twitch_1337_core::ai::memory::store::MemoryStore;
use twitch_1337_core::config::WebConfig;
use twitch_1337_core::ping::PingManager;
use twitch_1337_core::util::clock::Clock;

use crate::auth::session::SessionTable;
use crate::helix::HelixClient;

#[derive(Clone)]
pub struct WebState {
    pub ping_manager: Arc<RwLock<PingManager>>,
    pub memory_store: Arc<MemoryStore>,
    pub sessions: Arc<SessionTable>,
    pub helix: Arc<dyn HelixClient>,
    pub irc_connected: Arc<AtomicBool>,
    pub config: Arc<WebConfig>,
    pub clock: Arc<dyn Clock>,
    pub channel: Arc<str>,         // primary channel login
    pub broadcaster_id: Arc<str>,  // resolved at startup
    pub hidden_admins: Arc<[String]>,
    pub oauth: Arc<crate::auth::OAuthCtx>,
}
```

(`PingManager` is imported from `core::ping`. Compare with the upstream type — adjust if `core::ping::PingManager` is private. Spec calls for promoting to `pub`; do so if needed.)

- [ ] **Step 12: OAuth context + login + callback handlers**

Create `crates/web/src/auth/routes.rs`:

```rust
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use oauth2::basic::BasicClient;
use oauth2::reqwest::async_http_client;
use oauth2::{AuthorizationCode, AuthUrl, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope, TokenResponse, TokenUrl};
use rand::RngExt as _;
use rand::rngs::OsRng;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tower_cookies::{Cookie, Cookies, cookie::SameSite};

use crate::auth::mod_check::{ModCheckOutcome, check_is_mod};
use crate::error::WebError;
use crate::state::WebState;

pub struct OAuthCtx {
    pub basic: BasicClient,
}

impl OAuthCtx {
    pub fn new(client_id: &str, client_secret: &SecretString, public_url: &str) -> eyre::Result<Self> {
        let redirect = format!("{}/auth/callback", public_url.trim_end_matches('/'));
        let basic = BasicClient::new(
            ClientId::new(client_id.to_owned()),
            Some(ClientSecret::new(client_secret.expose_secret().to_owned())),
            AuthUrl::new("https://id.twitch.tv/oauth2/authorize".into())?,
            Some(TokenUrl::new("https://id.twitch.tv/oauth2/token".into())?),
        ).set_redirect_uri(RedirectUrl::new(redirect)?);
        Ok(Self { basic })
    }
}

pub fn auth_router() -> Router<WebState> {
    Router::new()
        .route("/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/logout", post(logout))
}

async fn login(State(state): State<WebState>, cookies: Cookies) -> impl IntoResponse {
    let csrf = CsrfToken::new_random();
    cookies.add(Cookie::build(("tw1337_oauth_state", csrf.secret().clone()))
        .http_only(true).secure(true).same_site(SameSite::Lax)
        .path("/").max_age(time::Duration::minutes(10)).build());
    let (auth_url, _csrf) = state.oauth.basic
        .authorize_url(|| csrf.clone())
        .add_scope(Scope::new("user:read:email".to_owned()))
        .url();
    Redirect::to(auth_url.as_ref())
}

#[derive(Deserialize)]
struct CallbackParams { code: String, state: String }

async fn callback(
    State(state): State<WebState>,
    Query(params): Query<CallbackParams>,
    cookies: Cookies,
) -> Result<impl IntoResponse, WebError> {
    let stored = cookies.get("tw1337_oauth_state").ok_or(WebError::Forbidden)?;
    if stored.value() != params.state {
        return Err(WebError::Forbidden);
    }
    cookies.remove(Cookie::from("tw1337_oauth_state"));

    let token = state.oauth.basic
        .exchange_code(AuthorizationCode::new(params.code))
        .request_async(async_http_client).await
        .map_err(|e| WebError::OAuthExchange(e.to_string()))?;

    // helix /users with the user's access token returns the caller
    let user_token = token.access_token().secret().to_owned();
    let me = fetch_caller_user(&state, &user_token).await
        .map_err(|e| WebError::OAuthExchange(format!("user lookup: {e}")))?;

    match check_is_mod(state.helix.as_ref(), &me.id, &state.broadcaster_id, &state.hidden_admins).await
        .map_err(|e| WebError::OAuthExchange(format!("mod check: {e}")))?
    {
        ModCheckOutcome::Allow => {}
        ModCheckOutcome::Deny => return Err(WebError::Forbidden),
    }

    let sid = state.sessions.insert(me.id.clone(), me.login.clone())
        .map_err(WebError::Internal)?;

    let session = state.sessions.get_and_touch(&sid).expect("just inserted");
    let csrf_value_hex = hex::encode(session.csrf_value);

    cookies.add(Cookie::build(("tw1337_sid", sid))
        .http_only(true).secure(true).same_site(SameSite::Lax)
        .path("/").build());
    cookies.add(Cookie::build(("tw1337_csrf", csrf_value_hex))
        .secure(true).same_site(SameSite::Lax)
        .path("/").build());

    tracing::info!(target: "twitch_1337_web", user_id=%me.id, user_login=%me.login, action="login", result="ok");
    Ok(Redirect::to("/"))
}

async fn logout(State(state): State<WebState>, cookies: Cookies) -> impl IntoResponse {
    if let Some(c) = cookies.get("tw1337_sid") {
        state.sessions.drop_session(c.value());
    }
    cookies.remove(Cookie::from("tw1337_sid"));
    cookies.remove(Cookie::from("tw1337_csrf"));
    Redirect::to("/login")
}

async fn fetch_caller_user(state: &WebState, access_token: &str) -> eyre::Result<crate::helix::HelixUser> {
    use eyre::eyre;
    #[derive(Deserialize)]
    struct Resp { data: Vec<crate::helix::HelixUser> }
    let url = "https://api.twitch.tv/helix/users";
    let resp: Resp = reqwest::Client::new().get(url)
        .bearer_auth(access_token)
        .header("Client-Id", state.config.session_secret.expose_secret())  // PLACEHOLDER — fix to twitch.client_id below
        .send().await?.error_for_status()?
        .json().await?;
    resp.data.into_iter().next().ok_or_else(|| eyre!("empty user list"))
}
```

Note: `fetch_caller_user` needs the bot's `twitch.client_id`. Add `client_id: SecretString` to `WebState`. Update Step 11 to include `pub client_id: SecretString`.

- [ ] **Step 13: Mod-gate middleware**

Append to `crates/web/src/auth/routes.rs`:

```rust
pub async fn require_mod(
    State(state): State<WebState>,
    cookies: Cookies,
    mut req: axum::extract::Request,
    next: Next,
) -> Result<Response, WebError> {
    let sid = cookies.get("tw1337_sid")
        .ok_or_else(|| WebError::Unauthenticated { next: req.uri().path().to_owned() })?;
    let session = state.sessions.get_and_touch(sid.value())
        .ok_or_else(|| WebError::Unauthenticated { next: req.uri().path().to_owned() })?;

    // Optionally re-check mod after refresh interval
    let now = state.clock.now();
    let elapsed = now.signed_duration_since(session.last_mod_check).to_std().unwrap_or_default();
    if elapsed > state.config.mod_check_refresh {
        match check_is_mod(state.helix.as_ref(), &session.user_id, &state.broadcaster_id, &state.hidden_admins).await {
            Ok(ModCheckOutcome::Allow) => state.sessions.record_mod_check(sid.value()),
            Ok(ModCheckOutcome::Deny) => {
                state.sessions.drop_session(sid.value());
                return Err(WebError::Forbidden);
            }
            Err(e) => {
                tracing::warn!(target: "twitch_1337_web", ?error=&e as &dyn std::error::Error, "mod refresh failed; admitting on stale check");
            }
        }
    }

    req.extensions_mut().insert(session);
    Ok(next.run(req).await)
}

pub async fn require_csrf(
    State(state): State<WebState>,
    cookies: Cookies,
    headers: HeaderMap,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, WebError> {
    if !matches!(*req.method(), axum::http::Method::POST | axum::http::Method::DELETE) {
        return Ok(next.run(req).await);
    }
    let session = req.extensions().get::<crate::auth::session::Session>()
        .ok_or(WebError::Forbidden)?
        .clone();

    // Header takes precedence; fall back to form-field _csrf parsed in handler
    let header_token = headers.get("X-Csrf-Token").and_then(|v| v.to_str().ok());
    let cookie_token = cookies.get("tw1337_csrf").map(|c| c.value().to_owned());

    if let (Some(h), Some(c)) = (header_token, cookie_token.as_deref()) {
        if h == c && crate::auth::csrf::verify(h, &session.csrf_value) {
            return Ok(next.run(req).await);
        }
    }
    // Form-field path: defer to per-handler validation by passing the session through
    Ok(next.run(req).await)
}
```

(The form-field `_csrf` check happens in each POST handler since axum can't read the body twice. Document this in handler comments in later tasks.)

- [ ] **Step 14: Embedded assets**

Create `crates/web/src/routes/assets.rs`:

```rust
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "src/assets/"]
struct Assets;

pub fn router() -> Router {
    Router::new().route("/assets/{*path}", get(serve))
}

async fn serve(axum::extract::Path(path): axum::extract::Path<String>) -> impl IntoResponse {
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (StatusCode::OK, [(header::CONTENT_TYPE, mime.as_ref())], content.data.into_owned()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

Add `mime_guess = "2"` to web deps.

Drop placeholders into `crates/web/src/assets/`:
- `htmx.min.js` — fetch from https://unpkg.com/htmx.org@2.0.4/dist/htmx.min.js
- `pico.min.css` — fetch from https://unpkg.com/@picocss/pico@2/css/pico.min.css
- `app.css` — minimal:

```css
.layout { display: flex; min-height: 100vh; }
.sidebar { width: 220px; padding: 1rem; border-right: 1px solid #333; }
.content { flex: 1; padding: 1.5rem; }
.flash { background: #2d4a2d; padding: .5rem 1rem; border-radius: 4px; }
.error { background: #4a2d2d; padding: .5rem 1rem; border-radius: 4px; }
```

- `app.js`:

```js
document.body.addEventListener('htmx:configRequest', (evt) => {
  if (evt.detail.verb !== 'get') {
    const m = document.cookie.match(/(?:^|; )tw1337_csrf=([^;]+)/);
    if (m) evt.detail.headers['X-Csrf-Token'] = decodeURIComponent(m[1]);
  }
});
```

- [ ] **Step 15: Base templates**

Create `crates/web/src/templates/base.html`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>{% block title %}twitch-1337{% endblock %}</title>
  <link rel="stylesheet" href="/assets/pico.min.css">
  <link rel="stylesheet" href="/assets/app.css">
  <script src="/assets/htmx.min.js" defer></script>
  <script src="/assets/app.js" defer></script>
</head>
<body>
  {% block layout %}
  <div class="layout">
    {% include "sidebar.html" %}
    <main class="content">
      {% if let Some(msg) = flash %}<div class="flash">{{ msg }}</div>{% endif %}
      {% block content %}{% endblock %}
    </main>
  </div>
  {% endblock %}
</body>
</html>
```

Create `crates/web/src/templates/sidebar.html`:

```html
<aside class="sidebar">
  <header><strong>twitch-1337</strong></header>
  <nav>
    <ul>
      <li><a href="/pings">Pings</a></li>
      <li>Memory
        <ul>
          <li><a href="/memory/soul">SOUL</a></li>
          <li><a href="/memory/lore">LORE</a></li>
          <li><a href="/memory/users">Users</a></li>
          <li><a href="/memory/state">State</a></li>
        </ul>
      </li>
    </ul>
  </nav>
  <footer>
    <small>{{ user_login }}</small>
    <form method="post" action="/logout">
      <input type="hidden" name="_csrf" value="{{ csrf }}">
      <button type="submit" class="secondary">Logout</button>
    </form>
  </footer>
</aside>
```

Create `crates/web/src/templates/auth/login.html`:

```html
{% extends "base.html" %}
{% block layout %}
<main class="content">
  <h1>twitch-1337 dashboard</h1>
  <p><a href="/login" role="button">Login with Twitch</a></p>
</main>
{% endblock %}
```

Create `crates/web/src/templates/auth/denied.html`:

```html
{% extends "base.html" %}
{% block layout %}
<main class="content">
  <h1>Access denied</h1>
  <p>Only moderators of the configured channel may access this dashboard.</p>
  <p><a href="/login">Try again with a different Twitch account</a></p>
</main>
{% endblock %}
```

Create `crates/web/src/templates/memory/conflict.html` (placeholder; full impl in Task 6):

```html
{% extends "base.html" %}
{% block content %}
<h2>Conflict</h2>
<p>File changed since you opened it. Reconcile manually.</p>
{% endblock %}
```

- [ ] **Step 16: Wire up router in `lib.rs`**

Replace `crates/web/src/lib.rs::run_web`:

```rust
pub async fn run_web(deps: WebDeps, shutdown: Arc<Notify>) -> Result<()> {
    let app = build_router(deps.state.clone());
    let listener = TcpListener::bind(deps.bind_addr).await
        .wrap_err_with(|| format!("bind {}", deps.bind_addr))?;
    info!(target: "twitch_1337_web", addr = %deps.bind_addr, "Web dashboard listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await
        .wrap_err("web serve")?;
    Ok(())
}

pub fn build_router(state: WebState) -> Router {
    use tower_cookies::CookieManagerLayer;
    use tower_http::trace::TraceLayer;

    let public = Router::new()
        .merge(routes::health::router(state.irc_connected.clone()))
        .merge(routes::assets::router())
        .merge(auth::auth_router().with_state(state.clone()));

    let authed = Router::new()
        .route("/", axum::routing::get(|| async { axum::response::Redirect::to("/pings") }))
        // Pings + memory routers mount in later tasks
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_mod));

    public.merge(authed)
        .with_state(state)
        .layer(CookieManagerLayer::new())
        .layer(TraceLayer::new_for_http())
}
```

Adjust `WebDeps` to carry the full state:

```rust
pub struct WebDeps {
    pub bind_addr: SocketAddr,
    pub state: WebState,
}
```

- [ ] **Step 17: Build the helix access-token provider in core**

Create `crates/core/src/web_glue.rs`:

```rust
//! Bridge between the bot's RefreshingLoginCredentials and the web crate's
//! AccessTokenProvider trait. Lets the helix client reuse the bot's existing
//! refreshed access token from token.ron.

use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::login::LoginCredentials;

pub struct CredsTokenProvider<L> { pub creds: Arc<L> }

#[async_trait]
impl<L> twitch_1337_web::helix::AccessTokenProvider for CredsTokenProvider<L>
where
    L: LoginCredentials + Send + Sync,
    <L as LoginCredentials>::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn current_access_token(&self) -> Result<String> {
        let creds = self.creds.get_credentials().await
            .map_err(|e| eyre::eyre!("get_credentials: {e}"))?;
        Ok(creds.token.unwrap_or_default())
    }
}
```

Export in `crates/core/src/lib.rs`: `pub mod web_glue;`.

- [ ] **Step 18: Build `WebState` in `run_bot`**

Edit `crates/core/src/lib.rs::run_bot`. Before spawning the web task, build the state:

```rust
if config.web.enabled {
    let bind_addr: std::net::SocketAddr = config.web.bind_addr.parse()
        .wrap_err("parse web.bind_addr")?;

    // 1. Build helix client (reqwest impl) with the bot's existing creds
    let token_provider = Arc::new(crate::web_glue::CredsTokenProvider {
        creds: Arc::new(client.clone()),  // adjust per actual creds plumbing; may need separate Arc
    });
    let helix = Arc::new(twitch_1337_web::helix::ReqwestHelixClient::new(
        reqwest::Client::new(),
        config.twitch.client_id.clone(),
        token_provider,
    ));

    // 2. Resolve broadcaster id once
    let broadcaster = helix.fetch_user_by_login(&config.twitch.channel).await
        .wrap_err("resolve broadcaster id")?
        .ok_or_else(|| eyre::eyre!("channel `{}` not found on twitch", config.twitch.channel))?;

    // 3. Build OAuth ctx
    let oauth = Arc::new(twitch_1337_web::auth::OAuthCtx::new(
        config.twitch.client_id.expose_secret(),
        &config.twitch.client_secret,
        &config.web.public_url,
    ).wrap_err("build oauth context")?);

    // 4. Build sessions
    let sessions = Arc::new(twitch_1337_web::auth::session::SessionTable::new(
        config.web.session_ttl,
        clock.clone(),
    ));

    // 5. Assemble WebState
    let state = twitch_1337_web::state::WebState {
        ping_manager: ping_manager.clone(),
        memory_store: memory_store_arc.clone(),
        sessions,
        helix,
        irc_connected: services_irc_connected.clone(),
        config: Arc::new(config.web.clone()),
        clock: clock.clone(),
        channel: Arc::from(config.twitch.channel.as_str()),
        broadcaster_id: Arc::from(broadcaster.id.as_str()),
        hidden_admins: Arc::from(config.twitch.hidden_admins.clone().into_boxed_slice()),
        client_id: config.twitch.client_id.clone(),
        oauth,
    };

    let deps = twitch_1337_web::WebDeps { bind_addr, state };
    let notify = handlers.shutdown_notify.clone();
    web_handle = Some(tokio::spawn(async move {
        if let Err(e) = twitch_1337_web::run_web(deps, notify).await {
            tracing::error!(target: "twitch_1337_web", ?error = e.as_ref() as &dyn std::error::Error, "web task failed");
        }
    }));
}
```

`memory_store_arc` is the `MemoryStore` from `ai_memory_v2` — wrap as `Arc<MemoryStore>` if not already (`build_ai_memory_v2` returns it).

- [ ] **Step 19: Auth route tests**

Create `crates/web/tests/auth_routes.rs`:

```rust
// Tests:
// 1. Unauthenticated GET /pings returns 302 to /login?next=%2Fpings (Task 4 mounts /pings;
//    for now use a dummy authed route inserted via test-only `build_router_for_test`).
// 2. Authenticated non-mod (session present, mod check denies) → 403
// 3. Authenticated mod → 200
// 4. POST /logout drops session
```

Implement minimal versions hitting `build_router` against an in-process `WebState` built from fakes (`FakeHelix`, `MemoryStore::open(tempdir)`, empty `PingManager`, etc.). Use `tower::ServiceExt::oneshot`.

(Full handler bodies expand in Task 4. For now assert: GET / → 302 /pings → 302 /login?next=%2Fpings when no session cookie.)

```bash
cargo nextest run -p twitch-1337-web --test auth_routes
```

Expected: PASS.

- [ ] **Step 20: Format, clippy, run full suite**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
```

Expected: green.

- [ ] **Step 21: Manual smoke (optional, requires real Twitch app)**

Provision a real Twitch developer app with redirect `https://<your-tunnel>/auth/callback`, fill `[web]` in config.toml, `cargo run -p twitch-1337`, visit `/login`, complete flow, verify `/` → `/pings` redirect (will 404 since pings router not mounted yet — that's Task 4).

- [ ] **Step 22: Commit + PR**

```bash
git checkout -b feature/web-auth
git add -A
git commit -m "$(cat <<'EOF'
feat(web): twitch oauth + sessions + csrf + mod gate

Adds /login, /auth/callback, /logout backed by oauth2 + reqwest, an
in-memory SessionTable behind RwLock, double-submit CSRF (cookie +
header or form field, validated in middleware), and a mod-gate
middleware that re-checks the helix moderators list every
mod_check_refresh interval. Introduces a minimal HelixClient trait
modeled on AviationClient and a ReqwestHelixClient implementation
that follows pagination cursors. Base layout + sidebar shell ship
embedded assets via rust-embed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push -u origin feature/web-auth
gh pr create --title "feat(web): oauth, sessions, csrf, mod gate" --body "Auth slice of the web dashboard. Login + callback + logout + mod-gate middleware + base layout. Pings/memory routes still pending."
```

---

## Task 4: Pings (list / new / edit / delete)

Mounts `/pings` routes, templates, and HTMX delete buttons. Branch: `feature/web-pings`.

**Files:**
- Create: `crates/web/src/routes/pings.rs`
- Create: `crates/web/src/templates/pings/list.html`
- Create: `crates/web/src/templates/pings/form.html`
- Create: `crates/web/src/templates/pings/row.html`
- Modify: `crates/web/src/lib.rs` (mount pings router)
- Test: `crates/web/tests/pings_routes.rs`

### Steps

- [ ] **Step 1: Failing list test**

Create `crates/web/tests/pings_routes.rs` (with a shared `helpers::test_state` building a `WebState` over a tempdir and a `FakeHelix`):

```rust
mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

#[tokio::test]
async fn list_renders_existing_pings() {
    let (state, app) = helpers::authed_app().await;
    {
        let mut pings = state.ping_manager.write().unwrap();
        pings.create_ping("alice".into(), "hello @{sender}".into(), "12345".into()).unwrap();
    }

    let res = app.oneshot(helpers::auth_get("/pings")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = helpers::body(res).await;
    assert!(body.contains("alice"), "{body}");
}
```

(`helpers::authed_app` returns `(WebState, Router)` where the test pre-inserts a session cookie for a hidden admin so middleware passes. `helpers::auth_get` adds the cookie header.)

- [ ] **Step 2: Run — fails**

```bash
cargo nextest run -p twitch-1337-web --test pings_routes
```

Expected: 404 because `/pings` not mounted.

- [ ] **Step 3: List route + template**

Create `crates/web/src/routes/pings.rs`:

```rust
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use twitch_1337_core::ping::{Ping, PingManager};

use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash::{Flash, take_flash};
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/pings", get(list).post(create))
        .route("/pings/new", get(new_form))
        .route("/pings/{name}", get(edit_form).post(update))
        .route("/pings/{name}/delete", post(delete))
}

#[derive(Template)]
#[template(path = "pings/list.html")]
struct ListTpl<'a> {
    user_login: &'a str,
    csrf: &'a str,
    flash: Option<String>,
    pings: Vec<PingRow>,
}

struct PingRow { name: String, template: String, members: usize, created_by: String }

async fn list(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    cookies: tower_cookies::Cookies,
) -> Result<impl IntoResponse, WebError> {
    let pings = state.ping_manager.read().unwrap();
    let rows: Vec<PingRow> = pings.iter().map(|(name, p)| PingRow {
        name: name.clone(),
        template: p.template.clone(),
        members: p.members.len(),
        created_by: p.created_by.clone(),
    }).collect();
    drop(pings);

    Ok(ListTpl {
        user_login: &session.user_login,
        csrf: &hex::encode(session.csrf_value),
        flash: take_flash(&cookies),
        pings: rows,
    })
}
```

(`PingManager::iter` returns `impl Iterator<Item=(&String, &Ping)>`. If not present, add a `pub fn iter()` accessor on `PingManager` returning a stable iterator over its internal map.)

Create `crates/web/src/templates/pings/list.html`:

```html
{% extends "base.html" %}
{% block content %}
<h2>Pings</h2>
<p><a href="/pings/new" role="button">+ New ping</a></p>
<table>
  <thead><tr><th>Name</th><th>Template</th><th>Members</th><th>Created by</th><th></th></tr></thead>
  <tbody>
  {% for p in pings %}
    {% include "pings/row.html" %}
  {% endfor %}
  </tbody>
</table>
{% endblock %}
```

Create `crates/web/src/templates/pings/row.html`:

```html
<tr>
  <td><a href="/pings/{{ p.name }}">{{ p.name }}</a></td>
  <td><code>{{ p.template }}</code></td>
  <td>{{ p.members }}</td>
  <td>{{ p.created_by }}</td>
  <td>
    <button hx-post="/pings/{{ p.name }}/delete" hx-confirm="Delete ping {{ p.name }}?" hx-target="closest tr" hx-swap="outerHTML">Delete</button>
  </td>
</tr>
```

Mount in `crates/web/src/lib.rs::build_router`:

```rust
let authed = authed.merge(routes::pings::router());
```

- [ ] **Step 4: Run list test — passes**

```bash
cargo nextest run -p twitch-1337-web --test pings_routes list_renders_existing_pings
```

Expected: PASS.

- [ ] **Step 5: New + create**

Add to `pings.rs`:

```rust
#[derive(Template)]
#[template(path = "pings/form.html")]
struct FormTpl<'a> {
    user_login: &'a str,
    csrf: &'a str,
    flash: Option<String>,
    name: &'a str,
    template_text: &'a str,
    error: Option<String>,
    is_new: bool,
}

async fn new_form(/* extractors */) -> impl IntoResponse {
    FormTpl { user_login: ..., csrf: ..., flash: None, name: "", template_text: "", error: None, is_new: true }
}

#[derive(Deserialize)]
struct CreateForm { name: String, template: String, _csrf: String }

async fn create(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    cookies: tower_cookies::Cookies,
    axum::Form(form): axum::Form<CreateForm>,
) -> Result<Response, WebError> {
    verify_csrf_form(&session, &form._csrf)?;
    let mut mgr = state.ping_manager.write().unwrap();
    if mgr.ping_exists_ignore_case(&form.name) {
        return Err(WebError::DuplicateName { name: form.name });
    }
    mgr.create_ping(form.name.clone(), form.template, session.user_id.clone())
        .map_err(|e| WebError::Validation { field: "template".into(), msg: e.to_string() })?;
    drop(mgr);
    crate::flash::set_flash(&cookies, &format!("ping `{}` created", form.name));
    Ok(Redirect::to("/pings").into_response())
}

fn verify_csrf_form(session: &Session, submitted: &str) -> Result<(), WebError> {
    if crate::auth::csrf::verify(submitted, &session.csrf_value) {
        Ok(())
    } else {
        Err(WebError::CsrfMismatch)
    }
}
```

Create `crates/web/src/templates/pings/form.html`:

```html
{% extends "base.html" %}
{% block content %}
<h2>{% if is_new %}New ping{% else %}Edit `{{ name }}`{% endif %}</h2>
{% if let Some(e) = error %}<div class="error">{{ e }}</div>{% endif %}
<form method="post" action="{% if is_new %}/pings{% else %}/pings/{{ name }}{% endif %}">
  <input type="hidden" name="_csrf" value="{{ csrf }}">
  {% if is_new %}<label>Name <input name="name" required></label>{% endif %}
  <label>Template
    <textarea name="template" required>{{ template_text }}</textarea>
  </label>
  <button type="submit">Save</button>
</form>
{% endblock %}
```

- [ ] **Step 6: Create test — control char rejection + duplicate**

Add to `pings_routes.rs`:

```rust
#[tokio::test]
async fn create_rejects_control_chars() {
    let (state, app) = helpers::authed_app().await;
    let res = helpers::post_form(&app, "/pings", &[
        ("name", "newping"),
        ("template", "hello\nworld"),
        ("_csrf", &helpers::csrf_token(&state)),
    ]).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_rejects_duplicate_name() {
    let (state, app) = helpers::authed_app().await;
    {
        let mut pings = state.ping_manager.write().unwrap();
        pings.create_ping("dup".into(), "hi".into(), "u".into()).unwrap();
    }
    let res = helpers::post_form(&app, "/pings", &[
        ("name", "DUP"),
        ("template", "x"),
        ("_csrf", &helpers::csrf_token(&state)),
    ]).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
```

Run:

```bash
cargo nextest run -p twitch-1337-web --test pings_routes
```

Expected: PASS.

- [ ] **Step 7: Edit form + update**

Add to `pings.rs`:

```rust
async fn edit_form(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    Path(name): Path<String>,
    cookies: tower_cookies::Cookies,
) -> Result<Response, WebError> {
    let mgr = state.ping_manager.read().unwrap();
    let ping = mgr.get(&name).ok_or(WebError::Validation { field: "name".into(), msg: "unknown ping".into() })?;
    let template_text = ping.template.clone();
    drop(mgr);
    Ok(FormTpl {
        user_login: &session.user_login,
        csrf: &hex::encode(session.csrf_value),
        flash: take_flash(&cookies),
        name: &name,
        template_text: &template_text,
        error: None,
        is_new: false,
    }.into_response())
}

#[derive(Deserialize)]
struct UpdateForm { template: String, _csrf: String }

async fn update(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    Path(name): Path<String>,
    cookies: tower_cookies::Cookies,
    axum::Form(form): axum::Form<UpdateForm>,
) -> Result<Response, WebError> {
    verify_csrf_form(&session, &form._csrf)?;
    let mut mgr = state.ping_manager.write().unwrap();
    mgr.edit_template(&name, form.template)
        .map_err(|e| WebError::Validation { field: "template".into(), msg: e.to_string() })?;
    drop(mgr);
    crate::flash::set_flash(&cookies, &format!("ping `{name}` saved"));
    Ok(Redirect::to("/pings").into_response())
}
```

(Add `pub fn get(&self, name: &str) -> Option<&Ping>` to `PingManager` if not present.)

Add edit round-trip test mirroring create.

- [ ] **Step 8: Delete**

```rust
async fn delete(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    cookies: tower_cookies::Cookies,
) -> Result<Response, WebError> {
    // Header-based CSRF (HTMX path) — the form path also calls this same handler
    let header = headers.get("X-Csrf-Token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !crate::auth::csrf::verify(header, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let mut mgr = state.ping_manager.write().unwrap();
    mgr.delete_ping(&name).map_err(|e| WebError::Validation { field: "name".into(), msg: e.to_string() })?;
    drop(mgr);
    crate::flash::set_flash(&cookies, &format!("ping `{name}` deleted"));
    Ok((axum::http::StatusCode::OK, "").into_response())  // HTMX swaps the row out
}
```

Test:

```rust
#[tokio::test]
async fn delete_via_htmx_header_succeeds() {
    let (state, app) = helpers::authed_app().await;
    {
        let mut pings = state.ping_manager.write().unwrap();
        pings.create_ping("doomed".into(), "x".into(), "u".into()).unwrap();
    }
    let csrf = helpers::csrf_token(&state);
    let req = Request::builder()
        .method("POST").uri("/pings/doomed/delete")
        .header("Cookie", helpers::session_cookie(&state))
        .header("X-Csrf-Token", &csrf)
        .body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(state.ping_manager.read().unwrap().get("doomed").is_none());
}

#[tokio::test]
async fn delete_without_csrf_rejected() {
    let (state, app) = helpers::authed_app().await;
    {
        let mut pings = state.ping_manager.write().unwrap();
        pings.create_ping("safe".into(), "x".into(), "u".into()).unwrap();
    }
    let req = Request::builder()
        .method("POST").uri("/pings/safe/delete")
        .header("Cookie", helpers::session_cookie(&state))
        .body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(state.ping_manager.read().unwrap().get("safe").is_some());
}
```

- [ ] **Step 9: Format + clippy + suite**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run -p twitch-1337-web
```

Expected: green.

- [ ] **Step 10: Commit + PR**

```bash
git checkout -b feature/web-pings
git add -A
git commit -m "feat(web): pings list/new/edit/delete routes ..."
git push -u origin feature/web-pings
gh pr create ...
```

---

## Task 5: Memory read (tree + per-kind viewers, no writes)

Adds `/memory`, `/memory/soul`, `/memory/lore`, `/memory/users[/<id>]`, `/memory/state[/<slug>]` GET routes. No POSTs yet. Branch: `feature/web-memory-read`.

**Files:**
- Create: `crates/web/src/routes/memory.rs`
- Create: `crates/web/src/templates/memory/tree.html`
- Create: `crates/web/src/templates/memory/editor.html`
- Create: `crates/web/src/templates/memory/state_list.html`
- Modify: `crates/web/src/lib.rs` (mount memory router)
- Test: `crates/web/tests/memory_read.rs`

### Steps

- [ ] **Step 1: Failing test for tree counts**

Create `crates/web/tests/memory_read.rs`:

```rust
mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

#[tokio::test]
async fn tree_renders_section_counts() {
    let (state, app) = helpers::authed_app().await;
    state.memory_store.write(
        &twitch_1337_core::ai::memory::types::FileKind::User { user_id: "111".into() },
        "alice user file body",
        Some("alice"), Some("Alice"),
    ).await.unwrap();
    state.memory_store.write_state(
        &twitch_1337_core::ai::memory::types::FileKind::State { slug: "foo".into() },
        "state body", None,
    ).await.unwrap();

    let res = app.oneshot(helpers::auth_get("/memory")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = helpers::body(res).await;
    assert!(body.contains("users (1)"), "{body}");
    assert!(body.contains("state (1)"), "{body}");
}
```

- [ ] **Step 2: Implement tree route**

Create `crates/web/src/routes/memory.rs`:

```rust
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use twitch_1337_core::ai::memory::store::{MemoryStore, WriteError};
use twitch_1337_core::ai::memory::types::FileKind;

use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash::take_flash;
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/memory", get(tree))
        .route("/memory/soul", get(view_soul))
        .route("/memory/lore", get(view_lore))
        .route("/memory/users", get(list_users))
        .route("/memory/users/{user_id}", get(view_user))
        .route("/memory/state", get(list_state))
        .route("/memory/state/new", get(state_new_form))
        .route("/memory/state/{slug}", get(view_state))
}

#[derive(Template)]
#[template(path = "memory/tree.html")]
struct TreeTpl<'a> {
    user_login: &'a str,
    csrf: &'a str,
    flash: Option<String>,
    user_count: usize,
    state_count: usize,
}

async fn tree(/* extractors */) -> Result<impl IntoResponse, WebError> {
    // …
}
```

Create `crates/web/src/templates/memory/tree.html`:

```html
{% extends "base.html" %}
{% block content %}
<h2>Memory</h2>
<ul>
  <li><a href="/memory/soul">SOUL</a></li>
  <li><a href="/memory/lore">LORE</a></li>
  <li><a href="/memory/users">users ({{ user_count }})</a></li>
  <li><a href="/memory/state">state ({{ state_count }})</a></li>
</ul>
{% endblock %}
```

Mount router in `crates/web/src/lib.rs::build_router`.

Run:

```bash
cargo nextest run -p twitch-1337-web --test memory_read tree_renders_section_counts
```

Expected: PASS.

- [ ] **Step 3: SOUL/LORE/user/state viewers**

Add to `routes/memory.rs`:

```rust
#[derive(Template)]
#[template(path = "memory/editor.html")]
struct EditorTpl<'a> {
    user_login: &'a str,
    csrf: &'a str,
    flash: Option<String>,
    title: &'a str,
    body: &'a str,
    mtime: u64,
    save_url: &'a str,
    delete_url: Option<&'a str>,
    error: Option<String>,
    byte_cap: usize,
}

async fn view_soul(State(state): State<WebState>, session: axum::extract::Extension<Session>, cookies: tower_cookies::Cookies)
    -> Result<impl IntoResponse, WebError>
{
    let f = state.memory_store.read_kind(&FileKind::Soul).await
        .map_err(WebError::Internal)?;
    let mtime = read_mtime(&state.memory_store, &FileKind::Soul).await?;
    Ok(EditorTpl {
        user_login: &session.user_login,
        csrf: &hex::encode(session.csrf_value),
        flash: take_flash(&cookies),
        title: "SOUL",
        body: &f.body,
        mtime,
        save_url: "/memory/soul",
        delete_url: None,
        error: None,
        byte_cap: state.memory_store.caps().soul_bytes,
    })
}
```

(`read_mtime` is a helper using `tokio::fs::metadata(memories_dir.join(kind.relative_path())).modified()` → `as_millis()`. Add as a method `MemoryStore::current_mtime(&self, kind: &FileKind) -> Result<u64>` or a free function in `routes/memory.rs`. Prefer the method on `MemoryStore` so the conflict path can reuse it.)

Repeat for `view_lore`, `view_user`, `view_state`. List handlers iterate `list_users`/`list_state`.

Editor template:

```html
{% extends "base.html" %}
{% block content %}
<h2>{{ title }}</h2>
{% if let Some(e) = error %}<div class="error">{{ e }}</div>{% endif %}
<form method="post" action="{{ save_url }}">
  <input type="hidden" name="_csrf" value="{{ csrf }}">
  <input type="hidden" name="mtime" value="{{ mtime }}">
  <label>Body (max {{ byte_cap }} bytes)
    <textarea name="body" rows="20">{{ body }}</textarea>
  </label>
  <button type="submit">Save</button>
</form>
{% if let Some(url) = delete_url %}
<form method="post" action="{{ url }}" onsubmit="return confirm('Delete?')">
  <input type="hidden" name="_csrf" value="{{ csrf }}">
  <button type="submit" class="secondary">Delete</button>
</form>
{% endif %}
{% endblock %}
```

- [ ] **Step 4: User-id regex validation at route level**

In `view_user` handler:

```rust
async fn view_user(
    State(state): State<WebState>,
    Path(user_id): Path<String>,
    /* … */
) -> Result<impl IntoResponse, WebError> {
    if !user_id.chars().all(|c| c.is_ascii_digit()) || user_id.is_empty() || user_id.len() > 32 {
        return Err(WebError::Validation { field: "user_id".into(), msg: "must be numeric, 1-32 digits".into() });
    }
    // …
}
```

Test:

```rust
#[tokio::test]
async fn user_id_path_traversal_rejected() {
    let (_state, app) = helpers::authed_app().await;
    let res = app.oneshot(helpers::auth_get("/memory/users/..%2F..%2Fetc%2Fpasswd")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 5: state route precedence**

Test:

```rust
#[tokio::test]
async fn memory_state_new_resolves_to_create_form() {
    let (state, app) = helpers::authed_app().await;
    state.memory_store.write_state(
        &twitch_1337_core::ai::memory::types::FileKind::State { slug: "new".into() },
        "should never resolve here", None,
    ).await.unwrap();  // will fail if MemoryStore enforces reserved slug; that's expected — adjust test to skip if so

    let res = app.oneshot(helpers::auth_get("/memory/state/new")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = helpers::body(res).await;
    assert!(body.contains("New state note"), "{body}");
}
```

Reserved-slug enforcement comes in Task 6; for now the route precedence alone makes this test pass.

- [ ] **Step 6: Format/clippy/suite + commit + PR**

Standard.

---

## Task 6: Memory write (write_with_guard + state CRUD + conflict UI)

Adds `MemoryStore::write_with_guard` + `Mtime`, mounts POST routes, conflict template, reserved-slug enforcement, byte-cap mapping. Branch: `feature/web-memory-write`.

**Files:**
- Modify: `crates/core/src/ai/memory/store.rs` (add `Mtime`, `WriteOutcome`, `write_with_guard`, `current_mtime`, reserved-slug check)
- Modify: `crates/core/src/ai/memory/types.rs` (potential `Caps` access cleanup if needed)
- Create: `crates/web/src/templates/memory/conflict.html` (full version)
- Modify: `crates/web/src/routes/memory.rs` (POST handlers)
- Test: `crates/core/src/ai/memory/store.rs` `tests` mod (mtime conflict)
- Test: `crates/web/tests/memory_write.rs`

### Steps

- [ ] **Step 1: `MemoryStore` type additions — failing test**

Append to `crates/core/src/ai/memory/store.rs` `tests` mod:

```rust
#[tokio::test]
async fn write_with_guard_detects_concurrent_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemoryStore::open(dir.path(), Caps::default()).await.unwrap();

    let kind = FileKind::Soul;
    let mtime0 = store.current_mtime(&kind).await.unwrap();

    // External writer mutates the file (simulate dreamer ritual)
    store.write(&kind, "rewritten by ritual", None, None).await.unwrap();

    // Web write supplying the stale token is rejected
    let outcome = store.write_with_guard(kind.clone(), "", "user draft", Some(mtime0)).await.unwrap();
    match outcome {
        WriteOutcome::Conflict { current_body, .. } => {
            assert_eq!(current_body.trim(), "rewritten by ritual");
        }
        _ => panic!("expected Conflict"),
    }

    let current_mtime = store.current_mtime(&kind).await.unwrap();
    let outcome = store.write_with_guard(kind.clone(), "", "user draft 2", Some(current_mtime)).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Written { .. }));
}

#[tokio::test]
async fn write_with_guard_unconditional_when_expected_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemoryStore::open(dir.path(), Caps::default()).await.unwrap();
    let outcome = store.write_with_guard(FileKind::Soul, "", "x", None).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Written { .. }));
}

#[tokio::test]
async fn state_reserved_slugs_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemoryStore::open(dir.path(), Caps::default()).await.unwrap();
    let err = store.write_state(&FileKind::State { slug: "new".into() }, "x", None).await.unwrap_err();
    assert!(matches!(err, WriteError::InvalidSlug));
    let err = store.write_state(&FileKind::State { slug: "delete".into() }, "x", None).await.unwrap_err();
    assert!(matches!(err, WriteError::InvalidSlug));
}
```

- [ ] **Step 2: Run — fails**

```bash
cargo nextest run -p twitch-1337-core ai::memory::store::tests::write_with_guard
```

Expected: missing `current_mtime`, `write_with_guard`, `WriteOutcome`, `WriteError::InvalidSlug`.

- [ ] **Step 3: Implement**

Edit `crates/core/src/ai/memory/store.rs`. Add at module top:

```rust
pub type Mtime = u64;

pub enum WriteOutcome {
    Written { new_mtime: Mtime },
    Conflict { current_body: String, current_mtime: Mtime },
}
```

Add to `WriteError`:

```rust
#[error("invalid_slug")]
InvalidSlug,
```

Add reserved slug list as a const:

```rust
pub const RESERVED_STATE_SLUGS: &[&str] = &["new", "delete"];

fn validate_state_slug(slug: &str) -> Result<(), WriteError> {
    if slug.is_empty() || slug.len() > 64 { return Err(WriteError::InvalidSlug); }
    if RESERVED_STATE_SLUGS.contains(&slug) { return Err(WriteError::InvalidSlug); }
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err(WriteError::InvalidSlug);
    }
    Ok(())
}
```

Call `validate_state_slug(slug)?` at the top of `write_state` (existing function).

Add methods on `impl MemoryStore`:

```rust
pub async fn current_mtime(&self, kind: &FileKind) -> Result<Mtime, WriteError> {
    let abs = self.inner.memories_dir.join(kind.relative_path());
    match tokio::fs::metadata(&abs).await {
        Ok(meta) => {
            let modified = meta.modified()
                .map_err(|e| WriteError::Io(eyre!("modified: {e}")))?;
            let dur = modified.duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| WriteError::Io(eyre!("epoch: {e}")))?;
            Ok(u64::try_from(dur.as_millis()).unwrap_or(u64::MAX))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(WriteError::Io(eyre!("metadata: {e}"))),
    }
}

pub async fn write_with_guard(
    &self,
    kind: FileKind,
    id: &str,
    body: &str,
    expected: Option<Mtime>,
) -> Result<WriteOutcome, WriteError> {
    let _ = id; // id is encoded in `kind`; arg kept for ergonomic call sites
    let rel = kind.relative_path();
    let lock = self.lock_for(&rel).await;
    let _g = lock.lock().await;

    if let Some(exp) = expected {
        let current = self.current_mtime(&kind).await?;
        if current != exp {
            let body = self.read_kind(&kind).await
                .map(|f| f.body)
                .unwrap_or_default();
            return Ok(WriteOutcome::Conflict { current_body: body, current_mtime: current });
        }
    }

    drop(_g);
    // Delegate to the existing write paths (which take their own per-path lock)
    match &kind {
        FileKind::State { slug } => self.write_state(&FileKind::State { slug: slug.clone() }, body, None).await?,
        _ => self.write(&kind, body, None, None).await?,
    }
    let new_mtime = self.current_mtime(&kind).await?;
    Ok(WriteOutcome::Written { new_mtime })
}
```

Note: dropping the outer guard between the mtime check and the inner write opens a TOCTOU window. Acceptable for v1: the dreamer ritual + `write_file` tool both end up serialized inside the per-path mutex, and the conflict UX explicitly accepts that the user reconciles manually. Add a one-line comment in the code stating that.

Run:

```bash
cargo nextest run -p twitch-1337-core ai::memory::store::tests::write_with_guard
cargo nextest run -p twitch-1337-core ai::memory::store::tests::state_reserved_slugs_rejected
```

Expected: PASS.

- [ ] **Step 4: Web write handlers**

Add to `crates/web/src/routes/memory.rs`:

```rust
pub fn write_router() -> Router<WebState> {
    Router::new()
        .route("/memory/soul", post(save_soul))
        .route("/memory/lore", post(save_lore))
        .route("/memory/users/{user_id}", post(save_user))
        .route("/memory/state", post(create_state))
        .route("/memory/state/{slug}", post(save_state))
        .route("/memory/state/{slug}/delete", post(delete_state))
}

#[derive(Deserialize)]
struct SaveForm { body: String, mtime: u64, _csrf: String }

async fn save_soul(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    cookies: tower_cookies::Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    save_kind(&state, &session, &cookies, FileKind::Soul, "SOUL", form, "/memory/soul").await
}

async fn save_kind(
    state: &WebState, session: &Session, cookies: &tower_cookies::Cookies,
    kind: FileKind, label: &str, form: SaveForm, redirect_to: &str,
) -> Result<Response, WebError> {
    if !crate::auth::csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    match state.memory_store.write_with_guard(kind.clone(), "", &form.body, Some(form.mtime)).await {
        Ok(WriteOutcome::Written { .. }) => {
            crate::flash::set_flash(cookies, &format!("{label} saved"));
            tracing::info!(target: "twitch_1337_web", user_id=%session.user_id, action="memory_write", target=%label, result="ok");
            Ok(Redirect::to(redirect_to).into_response())
        }
        Ok(WriteOutcome::Conflict { current_body, current_mtime }) => Err(WebError::Conflict {
            kind: label.into(), id: String::new(),
            current_body, current_mtime, draft: form.body,
        }),
        Err(WriteError::Full) => Err(WebError::Validation { field: "body".into(), msg: "exceeds byte cap".into() }),
        Err(WriteError::InvalidSlug) => Err(WebError::Validation { field: "slug".into(), msg: "reserved or invalid".into() }),
        Err(WriteError::StateFull) => Err(WebError::Validation { field: "slug".into(), msg: "state full".into() }),
        Err(WriteError::Io(e)) => Err(WebError::Internal(e)),
    }
}
```

Equivalent for save_lore, save_user (validate user_id regex), save_state, create_state, delete_state.

For `create_state`:

```rust
#[derive(Deserialize)]
struct CreateStateForm { slug: String, body: String, _csrf: String }

async fn create_state(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    cookies: tower_cookies::Cookies,
    axum::Form(form): axum::Form<CreateStateForm>,
) -> Result<Response, WebError> {
    if !crate::auth::csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    state.memory_store.write_state(
        &FileKind::State { slug: form.slug.clone() },
        &form.body, Some(&session.user_id),
    ).await.map_err(|e| match e {
        WriteError::InvalidSlug => WebError::Validation { field: "slug".into(), msg: "reserved or invalid".into() },
        WriteError::Full => WebError::Validation { field: "body".into(), msg: "exceeds byte cap".into() },
        WriteError::StateFull => WebError::Validation { field: "slug".into(), msg: "state full".into() },
        WriteError::Io(err) => WebError::Internal(err),
    })?;
    crate::flash::set_flash(&cookies, &format!("state `{}` created", form.slug));
    Ok(Redirect::to(&format!("/memory/state/{}", form.slug)).into_response())
}

async fn delete_state(
    State(state): State<WebState>,
    session: axum::extract::Extension<Session>,
    Path(slug): Path<String>,
    cookies: tower_cookies::Cookies,
    axum::Form(form): axum::Form<CsrfOnly>,
) -> Result<Response, WebError> {
    if !crate::auth::csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    state.memory_store.delete_state(&slug).await.map_err(WebError::Internal)?;
    crate::flash::set_flash(&cookies, &format!("state `{slug}` deleted"));
    Ok(Redirect::to("/memory/state").into_response())
}

#[derive(Deserialize)]
struct CsrfOnly { _csrf: String }
```

Mount `write_router()` alongside the read router in `crates/web/src/lib.rs`.

- [ ] **Step 5: Conflict template (full)**

Replace `crates/web/src/templates/memory/conflict.html`:

```html
{% extends "base.html" %}
{% block content %}
<h2>Edit conflict — {{ kind }}</h2>
<p class="error">File changed since you opened it (dreamer ritual or AI tool). Your draft is preserved on the right — copy what you need into the textarea and resubmit.</p>
<div style="display:flex;gap:1rem">
  <div style="flex:1">
    <h3>Current on-disk</h3>
    <pre>{{ current_body }}</pre>
  </div>
  <form method="post" action="" style="flex:1">
    <input type="hidden" name="_csrf" value="{{ csrf }}">
    <input type="hidden" name="mtime" value="{{ current_mtime }}">
    <label>Your draft (resubmit)
      <textarea name="body" rows="20">{{ draft }}</textarea>
    </label>
    <button type="submit">Save (overwrite current)</button>
  </form>
</div>
{% endblock %}
```

Update `WebError::Conflict` rendering to inject `csrf` from a session looked up via headers (or thread session through the error). Simplest: change `WebError::Conflict` to include the csrf hex string built by the handler, since the handler already has the session:

```rust
Conflict { kind: String, id: String, current_body: String, current_mtime: u64, draft: String, csrf: String }
```

And update every callsite.

- [ ] **Step 6: Tests for write paths**

Create `crates/web/tests/memory_write.rs`:

```rust
mod helpers;

use axum::http::StatusCode;
use tower::ServiceExt as _;

#[tokio::test]
async fn save_soul_written_when_mtime_matches() {
    let (state, app) = helpers::authed_app().await;
    let mtime = state.memory_store.current_mtime(&twitch_1337_core::ai::memory::types::FileKind::Soul).await.unwrap();
    let csrf = helpers::csrf_token(&state);

    let res = helpers::post_form(&app, "/memory/soul", &[
        ("body", "new soul"),
        ("mtime", &mtime.to_string()),
        ("_csrf", &csrf),
    ]).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let f = state.memory_store.read_kind(&twitch_1337_core::ai::memory::types::FileKind::Soul).await.unwrap();
    assert_eq!(f.body.trim(), "new soul");
}

#[tokio::test]
async fn save_soul_409_on_stale_mtime() {
    let (state, app) = helpers::authed_app().await;
    // Force file to update so the stale mtime no longer matches
    state.memory_store.write(&twitch_1337_core::ai::memory::types::FileKind::Soul, "after", None, None).await.unwrap();
    let csrf = helpers::csrf_token(&state);

    let res = helpers::post_form(&app, "/memory/soul", &[
        ("body", "draft"),
        ("mtime", "0"),
        ("_csrf", &csrf),
    ]).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = helpers::body(res).await;
    assert!(body.contains("after"), "{body}");
    assert!(body.contains("draft"), "{body}");
}

#[tokio::test]
async fn save_oversized_body_returns_400() {
    let (state, app) = helpers::authed_app().await;
    let mtime = state.memory_store.current_mtime(&twitch_1337_core::ai::memory::types::FileKind::Soul).await.unwrap();
    let csrf = helpers::csrf_token(&state);
    let huge = "x".repeat(5000);  // SOUL cap = 4096

    let res = helpers::post_form(&app, "/memory/soul", &[
        ("body", &huge),
        ("mtime", &mtime.to_string()),
        ("_csrf", &csrf),
    ]).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_state_reserved_slug_rejected() {
    let (state, app) = helpers::authed_app().await;
    let csrf = helpers::csrf_token(&state);
    let res = helpers::post_form(&app, "/memory/state", &[
        ("slug", "new"),
        ("body", "x"),
        ("_csrf", &csrf),
    ]).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_state_round_trip() {
    let (state, app) = helpers::authed_app().await;
    state.memory_store.write_state(
        &twitch_1337_core::ai::memory::types::FileKind::State { slug: "doomed".into() },
        "x", None,
    ).await.unwrap();
    let csrf = helpers::csrf_token(&state);
    let res = helpers::post_form(&app, "/memory/state/doomed/delete", &[("_csrf", &csrf)]).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let exists = std::path::Path::new(state.memory_store.memories_dir())
        .join("state/doomed.md").exists();
    assert!(!exists);
}
```

Run:

```bash
cargo nextest run -p twitch-1337-web --test memory_write
```

Expected: 5 PASS.

- [ ] **Step 7: Wire `delete_url` for state in editor**

In `view_state`, set `delete_url: Some(&format!("/memory/state/{slug}/delete"))` on `EditorTpl`. SOUL/LORE/user keep `None` (no delete endpoint mounted).

- [ ] **Step 8: Format / clippy / full suite**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
```

Expected: all green.

- [ ] **Step 9: Manual smoke**

With `[web]` enabled, log in via Twitch as a hidden_admin or moderator. Hit:
- `/pings` → list, create one, edit it, delete via HTMX button, watch row disappear.
- `/memory/soul` → edit body, save, verify on-disk file updated.
- Open SOUL in two browser tabs, save in tab 1, save in tab 2 → tab 2 sees 409 conflict page with current body + preserved draft.
- `/memory/state` → create note, edit, delete.
- `/memory/state/new` → resolves to create form (route precedence).
- Try `/memory/state` POST with slug=`new` → 400.
- Try `/memory/users/../../etc/passwd` → 400.

- [ ] **Step 10: Commit + PR**

```bash
git checkout -b feature/web-memory-write
git add -A
git commit -m "feat(web): memory write paths with mtime guard + state CRUD ..."
git push -u origin feature/web-memory-write
gh pr create ...
```

---

## Self-Review Checklist (executed inline; no agent dispatch)

**1. Spec coverage**

| Spec section | Task |
|---|---|
| Crate topology | Task 1 |
| `[web]` config + humantime + validation | Task 2 |
| `irc_connected` + latency plumbing | Task 2 |
| `--healthcheck` + Dockerfile | Task 2 |
| OAuth flow | Task 3 |
| Mod check (hidden → broadcaster → helix w/ pagination) | Task 3 |
| Sessions (TTL, sliding refresh, in-memory) | Task 3 |
| CSRF (per-session token, header + form) | Task 3 |
| HelixClient trait + ReqwestHelixClient | Task 3 |
| Base layout + sidebar + embedded assets | Task 3 |
| WebError variants | Task 3 + Task 6 (Conflict adds csrf field) |
| Pings CRUD | Task 4 |
| Memory tree + viewers | Task 5 |
| User-id regex validation | Task 5 |
| State route precedence (`/state/new`) | Task 5 |
| `MemoryStore::write_with_guard` + `Mtime` | Task 6 |
| Reserved slug enforcement in store | Task 6 |
| Conflict UI | Task 6 |
| Flash messages | Task 4 (used) + helper introduced in Task 3 |
| Login throttling | Out of scope per spec (deferred to Cloudflare) — no task |

**2. Placeholder scan**

- Step 7 of Task 3 contains a `// PLACEHOLDER — fix to twitch.client_id` comment in `fetch_caller_user`. **Fix:** replace with `state.client_id.expose_secret()` and update Step 11 to add `pub client_id: SecretString` to `WebState`. Already noted inline.
- "// adjust per actual creds plumbing" in Step 18 — clarified that `client.clone()` may not be the right Arc; the engineer should grab the credentials from the existing `RefreshingLoginCredentials` plumbed via `setup_and_verify_twitch_client`. Add this note.

**3. Type consistency**

- `MemoryStore::current_mtime` referenced in Task 5 (Step 3 — `read_mtime` helper note) but only implemented in Task 6 Step 3. **Fix:** make Task 5 introduce a stub `current_mtime` that returns 0 if the engineer follows the order strictly, OR move the `current_mtime` impl into Task 5 since it's read-only. **Resolution:** move `current_mtime` into Task 5 Step 3 (simple, no behavior change) so the editor template renders a real mtime token before write paths land. `write_with_guard` and `WriteOutcome` stay in Task 6.

Apply that resolution as an inline edit:
- Task 5 Step 3: include the `current_mtime` method on `MemoryStore` (the body shown in Task 6 Step 3, minus the WriteOutcome type).
- Task 6 Step 3: drop `current_mtime` (already exists), only add `Mtime` type alias, `WriteOutcome`, `WriteError::InvalidSlug`, `RESERVED_STATE_SLUGS`, `validate_state_slug`, `write_with_guard`.

(Captured in this self-review; the engineer reading the plan top-to-bottom should follow this resolution.)

**4. Other consistency:**
- `flash::take_flash` / `flash::set_flash` referenced in Tasks 3, 4, 5, 6 but module never explicitly created. Add to Task 3 Step 14 a `flash.rs` skeleton:

```rust
// crates/web/src/flash.rs
use tower_cookies::{Cookie, Cookies, cookie::SameSite};

pub fn set_flash(cookies: &Cookies, msg: &str) {
    cookies.add(Cookie::build(("tw1337_flash", msg.to_owned()))
        .path("/").max_age(time::Duration::seconds(60))
        .same_site(SameSite::Lax).build());
}

pub fn take_flash(cookies: &Cookies) -> Option<String> {
    let v = cookies.get("tw1337_flash").map(|c| c.value().to_owned())?;
    cookies.remove(Cookie::from("tw1337_flash"));
    Some(v)
}
```

And `pub mod flash;` in `crates/web/src/lib.rs`. Add to Task 3 file list.

---

Plan complete and saved to `docs/superpowers/plans/2026-05-09-web-dashboard.md`.

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task with two-stage review between tasks; fastest iteration loop and tightest blast radius.
2. **Inline Execution** — execute tasks in this session via executing-plans, batched with checkpoints for review.
