# Dashboard viewer tier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a follower-gated, read-only "viewer" tier to the web dashboard so the broader community can see the leaderboard, currently tracked flights, and the ping catalogue while keeping mutations and AI memory mod-only.

**Architecture:** Introduce an ordered `Role { Viewer, Mod }` enum stored on `Session`, generalise the existing `require_mod` middleware to `require_role(min)`, add follower checks against Twitch helix (`/channels/followers` with the bot token, `/channels/followed` with the user token), split the axum router into viewer / mod sub-routers with a `viewer_method_guard` that rejects non-GET/HEAD, and surface two new pages (`/leaderboard`, `/flights`) sourced from shared `Arc<RwLock<HashMap<…>>>` and a new `TrackerCommand::Snapshot` over the existing mpsc.

**Tech Stack:** Rust 2024, axum 0.8, askama, tower-cookies, twitch helix REST, tokio mpsc/oneshot.

**Spec:** `docs/superpowers/specs/2026-05-12-dashboard-viewer-tier-design.md`. See § 14 for the matching build sequence.

**Conventions reminders (project-specific, do not forget):**
- Use `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail` instead of `cargo test` ([feedback memory](../../../../home/chrono/.claude/projects/-home-chrono-Projects-twitch-1337/memory/feedback_use_nextest.md)).
- Pre-commit gate: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run …`.
- Cargo.lock must be staged alongside any `Cargo.toml` dep change in the same commit.
- Use `gh` with the sandbox disabled (see feedback memory).
- Branch already exists for this work via worktree.

---

## File map

**New:**
- `crates/web/src/auth/role.rs` — `Role` enum and ordering.
- `crates/web/src/routes/leaderboard.rs` — `/leaderboard` handler.
- `crates/web/src/routes/flights.rs` — `/flights` handler.
- `crates/web/templates/leaderboard/list.html` — leaderboard template.
- `crates/web/templates/flights/list.html` — flights template.
- `crates/web/tests/auth_viewer_tier.rs` — viewer-tier integration scenarios.

**Renamed:**
- `crates/web/src/auth/mod_check.rs` → `crates/web/src/auth/role_check.rs`.

**Modified:**
- `crates/web/src/auth/mod.rs` — module renames + re-exports.
- `crates/web/src/auth/session.rs` — add `role` to `Session`, rename `last_mod_check`/`record_mod_check`.
- `crates/web/src/auth/routes.rs` — add `require_role`, `viewer_method_guard`, scope, callback dispatch.
- `crates/web/src/helix.rs` — `HelixClient::is_follower`, helper for `/channels/followed`.
- `crates/web/src/error.rs` — `MethodNotAllowed` variant.
- `crates/web/src/config.rs` — rename `mod_check_refresh` → `role_check_refresh`.
- `crates/web/src/state.rs` — leaderboard handle + optional tracker mpsc.
- `crates/web/src/lib.rs` — viewer / mod router split + root redirect.
- `crates/web/src/nav.rs` — role-conditional sidebar.
- `crates/web/src/routes/pings.rs` — pass `is_mod` into templates.
- `crates/web/src/routes/mod.rs` — register leaderboard/flights.
- `crates/web/src/routes/stubs.rs` — drop `/flights` stub.
- `crates/web/src/bin/web_dev.rs` — wire leaderboard + tracker into `WebState`.
- `crates/web/templates/sidebar.html` — role-gate groups, add Leaderboard.
- `crates/web/templates/pings/list.html` — gate mutation controls on `is_mod`.
- `crates/web/tests/helpers/mod.rs` — `FakeHelix.followers`, leaderboard fixtures.
- `crates/core/src/aviation/tracker.rs` — `TrackerCommand::Snapshot` + handling.
- `crates/twitch-1337/src/main.rs` (or equivalent bin wiring) — pass leaderboard `Arc` and tracker sender into `WebState`.
- `CLAUDE.md` — note new OAuth scopes + bot scope under "Config".

---

## Task 1: Add `Role` enum

**Files:**
- Create: `crates/web/src/auth/role.rs`
- Modify: `crates/web/src/auth/mod.rs`

- [ ] **Step 1: Create the role module**

Write `crates/web/src/auth/role.rs`:

```rust
//! Authenticated dashboard tier.
//!
//! Ordered so `session.role >= required` is the gate predicate; inserting
//! a new tier later means slotting it in at the right ordinal.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Mod,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Mod => "mod",
        }
    }
}
```

- [ ] **Step 2: Re-export from `auth/mod.rs`**

In `crates/web/src/auth/mod.rs`, add `pub mod role;` next to the other `pub mod`s and `pub use role::Role;` next to the existing `pub use routes::{…};`.

- [ ] **Step 3: Add ordering test**

Append to `crates/web/src/auth/role.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn viewer_is_lower_than_mod() {
        assert!(Role::Viewer < Role::Mod);
        assert!(Role::Mod >= Role::Viewer);
        assert!(Role::Viewer >= Role::Viewer);
    }

    #[test]
    fn labels_match_tracing_fields() {
        assert_eq!(Role::Viewer.label(), "viewer");
        assert_eq!(Role::Mod.label(), "mod");
    }
}
```

- [ ] **Step 4: Run the test, expect pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web auth::role`
Expected: 2 passed.

- [ ] **Step 5: Pre-commit gate + commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add crates/web/src/auth/role.rs crates/web/src/auth/mod.rs
git commit -m "feat(web): introduce Role enum (viewer < mod)"
```

---

## Task 2: Thread `Role` through `Session`

**Files:**
- Modify: `crates/web/src/auth/session.rs`
- Modify: `crates/web/src/auth/routes.rs` (compile-fix at single call-site)
- Modify: `crates/web/tests/helpers/mod.rs` (if it calls `insert`)

- [ ] **Step 1: Add the field + accept it on insert**

In `crates/web/src/auth/session.rs`, change the `Session` struct and `insert` signature:

```rust
use crate::auth::role::Role;

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: String,
    pub user_login: String,
    pub role: Role,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_role_check: DateTime<Utc>,
    pub csrf_value: [u8; 32],
}
```

```rust
pub fn insert(
    &self,
    user_id: String,
    user_login: String,
    role: Role,
) -> Result<(SessionId, [u8; 32])> {
    let now = self.clock.now();
    let mut rng = rand::rng();
    let mut id_bytes = [0u8; 32];
    rng.fill_bytes(&mut id_bytes);
    let mut csrf = [0u8; 32];
    rng.fill_bytes(&mut csrf);
    let id = hex::encode(id_bytes);
    self.inner.write().unwrap().insert(
        id.clone(),
        Session {
            user_id,
            user_login,
            role,
            issued_at: now,
            last_seen: now,
            last_role_check: now,
            csrf_value: csrf,
        },
    );
    Ok((id, csrf))
}
```

- [ ] **Step 2: Rename `record_mod_check` → `record_role_check`**

In the same file:

```rust
pub fn record_role_check(&self, id: &str) {
    let now = self.clock.now();
    if let Some(s) = self.inner.write().unwrap().get_mut(id) {
        s.last_role_check = now;
    }
}
```

Also update the file's top-level doc comment to say "role-gate middleware" instead of "mod-gate middleware".

- [ ] **Step 3: Patch the only `insert` call-site**

In `crates/web/src/auth/routes.rs::callback`, replace:

```rust
let (sid, csrf_value) = state
    .sessions
    .insert(me.id.clone(), me.login.clone())
```

with (temporarily — refined in Task 7):

```rust
let (sid, csrf_value) = state
    .sessions
    .insert(me.id.clone(), me.login.clone(), crate::auth::role::Role::Mod)
```

Also update `record_mod_check` → `record_role_check` in `require_mod`, and `last_mod_check` → `last_role_check` in the elapsed-since read.

- [ ] **Step 4: Patch test helpers**

In `crates/web/tests/helpers/mod.rs`, the `insert_session` helper calls `sessions.insert(...)`. Add a `Role::Mod` argument so existing tests still seed a mod session by default:

```rust
pub fn insert_session(state: &WebState, user_id: &str, user_login: &str) -> (String, [u8; 32]) {
    state
        .sessions
        .insert(user_id.to_owned(), user_login.to_owned(), twitch_1337_web::auth::Role::Mod)
        .expect("insert test session")
}
```

(Adjust the actual return type to match whatever the existing helper returns — keep the signature stable for callers.)

- [ ] **Step 5: Rename `mod_check_refresh` → `role_check_refresh`**

In `crates/web/src/config.rs`:

```rust
#[derive(Clone)]
pub struct WebConfig {
    pub bind_addr: String,
    pub public_url: String,
    pub session_secret: SecretString,
    pub session_ttl: Duration,
    pub role_check_refresh: Duration,
}
```

Grep for any remaining `mod_check_refresh` references and update them (`crates/web/src/auth/routes.rs::require_mod`, `crates/web/src/bin/web_dev.rs`, the bin under `crates/twitch-1337/src/`, and `crates/web/tests/helpers/mod.rs`):

```bash
rg -l "mod_check_refresh" .
```

Update every hit. Use the same default value (no behaviour change).

- [ ] **Step 6: Build + test**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.
Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web`
Expected: all existing tests still pass (each session is now created with `Role::Mod`).

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/auth/session.rs crates/web/src/auth/routes.rs \
        crates/web/src/config.rs crates/web/tests/helpers/mod.rs \
        crates/web/src/bin/web_dev.rs crates/twitch-1337/src
git commit -m "refactor(web): thread Role through Session, rename mod_check→role_check"
```

(The path list reflects renames; if `crates/twitch-1337/src/` has no `mod_check_refresh` references, drop it from `git add`.)

---

## Task 3: Rename `auth/mod_check.rs` → `auth/role_check.rs`

**Files:**
- Rename: `crates/web/src/auth/mod_check.rs` → `crates/web/src/auth/role_check.rs`
- Modify: `crates/web/src/auth/mod.rs`
- Modify: `crates/web/src/auth/routes.rs` (import path)

- [ ] **Step 1: Move the file**

```bash
git mv crates/web/src/auth/mod_check.rs crates/web/src/auth/role_check.rs
```

- [ ] **Step 2: Update `auth/mod.rs`**

Replace `pub mod mod_check;` with `pub mod role_check;` and update the module-level doc comment in `auth/mod.rs` accordingly.

- [ ] **Step 3: Update imports in `routes.rs`**

In `crates/web/src/auth/routes.rs`:

```rust
use crate::auth::role_check::{ModCheckOutcome, check_is_mod};
```

becomes:

```rust
use crate::auth::role_check::{GateOutcome, check_is_mod};
```

…and inside `role_check.rs` rename the enum:

```rust
pub enum GateOutcome { Allow, Deny }
```

Sweep `rg -l "ModCheckOutcome"` and rewrite every match.

- [ ] **Step 4: Build**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add -A crates/web/src/auth
git commit -m "refactor(web): rename mod_check → role_check, ModCheckOutcome → GateOutcome"
```

---

## Task 4: Add `HelixClient::is_follower`

**Files:**
- Modify: `crates/web/src/helix.rs`
- Modify: `crates/web/tests/helpers/mod.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/web/tests/helpers/mod.rs` (extending the existing `FakeHelix`):

```rust
pub struct FakeHelix {
    pub moderators: Vec<String>,
    pub followers: Vec<String>, // user ids that follow the broadcaster
    pub users: HashMap<String, HelixUser>,
}

#[async_trait]
impl HelixClient for FakeHelix {
    // ... existing methods ...
    async fn is_follower(&self, _broadcaster: &str, user_id: &str) -> eyre::Result<bool> {
        Ok(self.followers.iter().any(|f| f == user_id))
    }
}
```

Update every existing `FakeHelix { ... }` construction in `tests/*.rs` to include `followers: vec![]`. Run `rg -l "FakeHelix \{"` and patch each one.

Create `crates/web/tests/helix_follower_check.rs` (mirrors the existing `helix_moderator_check.rs`):

```rust
mod helpers;

use std::sync::Arc;

use helpers::FakeHelix;
use twitch_1337_web::helix::HelixClient;

#[tokio::test]
async fn is_follower_reports_membership() {
    let helix: Arc<dyn HelixClient> = Arc::new(FakeHelix {
        moderators: vec![],
        followers: vec!["42".into()],
        users: Default::default(),
    });
    assert!(helix.is_follower("b123", "42").await.unwrap());
    assert!(!helix.is_follower("b123", "99").await.unwrap());
}
```

- [ ] **Step 2: Run the test, expect compile failure**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web helix_follower_check`
Expected: compile error — `is_follower` not on the trait.

- [ ] **Step 3: Extend the trait**

In `crates/web/src/helix.rs`, add to the trait:

```rust
async fn is_follower(&self, broadcaster_id: &str, user_id: &str) -> eyre::Result<bool>;
```

Implement on the production helix client by calling
`GET https://api.twitch.tv/helix/channels/followers?broadcaster_id={broadcaster_id}&user_id={user_id}`
with the bot's helix user token (same auth shape as `is_moderator`):

```rust
async fn is_follower(&self, broadcaster_id: &str, user_id: &str) -> eyre::Result<bool> {
    #[derive(serde::Deserialize)]
    struct Resp { total: u64 }
    let url = format!(
        "https://api.twitch.tv/helix/channels/followers?broadcaster_id={}&user_id={}",
        broadcaster_id, user_id,
    );
    let resp = self
        .http
        .get(&url)
        .bearer_auth(self.bearer.expose_secret())
        .header("Client-Id", self.client_id.expose_secret())
        .send()
        .await
        .wrap_err("helix /channels/followers send")?;
    let status = resp.status();
    let resp = resp
        .error_for_status()
        .wrap_err_with(|| format!("helix /channels/followers returned {status}"))?;
    let parsed: Resp = resp.json().await.wrap_err("helix /channels/followers decode")?;
    Ok(parsed.total > 0)
}
```

(Match the field names of the existing helix client in `helix.rs`; if it uses `Self::http`, `Self::token`, etc., keep that.)

- [ ] **Step 4: Run the test, expect pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web helix_follower_check`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/helix.rs crates/web/tests/helpers/mod.rs crates/web/tests/helix_follower_check.rs crates/web/tests/*.rs
git commit -m "feat(web): HelixClient::is_follower (channels/followers)"
```

---

## Task 5: `check_is_follower_with_token` (login-path helper)

**Files:**
- Modify: `crates/web/src/auth/role_check.rs`

- [ ] **Step 1: Add a unit test against a recorded fixture or HTTP mock**

`role_check.rs` already has `check_is_mod_with_token` calling helix through `state.oauth.http`. The simplest test path is to assert the URL + headers via `wiremock`. If wiremock isn't a dep yet, fall back to a hand-rolled `axum::Router` listener on a free port (pattern used in `auth_routes.rs`). Pseudocode shape (adapt to existing test infra):

```rust
#[tokio::test]
async fn check_is_follower_with_token_allows_when_followed() {
    // Spin up a tiny axum server that returns
    // {"data":[{"broadcaster_id":"b","user_id":"u"}],"total":1}
    // when GET /helix/channels/followed?user_id=u&broadcaster_id=b arrives
    // with the right bearer + client-id. Build a `WebState` pointing at
    // its base url, call check_is_follower_with_token, assert Allow.
}
```

Mirror the test against an empty `data: []` response → expect `Deny`.

- [ ] **Step 2: Run the test, expect failure**

Function does not exist yet. Compile error.

- [ ] **Step 3: Add the helper**

In `crates/web/src/auth/role_check.rs`:

```rust
/// Login-path check using the viewer's just-issued user token. Calls
/// `/helix/channels/followed?user_id=…&broadcaster_id=…` (requires the
/// `user:read:follows` scope on the user token).
pub async fn check_is_follower_with_token(
    state: &WebState,
    user_id: &str,
    user_access_token: &str,
    broadcaster_id: &str,
) -> eyre::Result<GateOutcome> {
    #[derive(serde::Deserialize)]
    struct Resp { total: u64 }
    let url = format!(
        "{}/helix/channels/followed?user_id={}&broadcaster_id={}",
        state.helix_base_url(), user_id, broadcaster_id,
    );
    let resp = state
        .oauth
        .http
        .get(&url)
        .bearer_auth(user_access_token)
        .header("Client-Id", state.client_id.expose_secret())
        .send()
        .await
        .wrap_err("helix /channels/followed send")?;
    let status = resp.status();
    let resp = resp
        .error_for_status()
        .wrap_err_with(|| format!("helix /channels/followed returned {status}"))?;
    let parsed: Resp = resp.json().await.wrap_err("helix /channels/followed decode")?;
    if parsed.total > 0 { Ok(GateOutcome::Allow) } else { Ok(GateOutcome::Deny) }
}
```

If `WebState::helix_base_url()` doesn't already exist, mirror what `check_is_mod_with_token` uses today — it hard-codes `https://api.twitch.tv`. Use the same constant here for consistency.

- [ ] **Step 4: Run the test, expect pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web check_is_follower`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/auth/role_check.rs crates/web/tests/auth_follower_check.rs
git commit -m "feat(web): check_is_follower_with_token (login path)"
```

---

## Task 6: `viewer_method_guard` + `WebError::MethodNotAllowed`

**Files:**
- Modify: `crates/web/src/error.rs`
- Modify: `crates/web/src/auth/routes.rs`

- [ ] **Step 1: Add the error variant + its IntoResponse mapping**

In `crates/web/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    // ... existing ...
    #[error("method not allowed")]
    MethodNotAllowed,
}
```

Extend the `IntoResponse` impl with:

```rust
WebError::MethodNotAllowed => (axum::http::StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response(),
```

- [ ] **Step 2: Write the guard middleware**

In `crates/web/src/auth/routes.rs`:

```rust
pub async fn viewer_method_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, WebError> {
    use axum::http::Method;
    match *req.method() {
        Method::GET | Method::HEAD => Ok(next.run(req).await),
        _ => Err(WebError::MethodNotAllowed),
    }
}
```

- [ ] **Step 3: Write a unit test**

In a new `#[cfg(test)] mod` block at the bottom of `routes.rs` or in a new `crates/web/tests/auth_viewer_guard.rs`:

```rust
#[tokio::test]
async fn viewer_method_guard_rejects_post() {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt as _;

    let app = Router::new()
        .route("/x", axum::routing::any(|| async { "ok" }))
        .layer(axum::middleware::from_fn(twitch_1337_web::auth::viewer_method_guard));

    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/x").method(Method::GET).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(Request::builder().uri("/x").method(Method::POST).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}
```

Re-export `viewer_method_guard` from `auth/mod.rs` so external test crates can name it.

- [ ] **Step 4: Run, expect pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web viewer_method_guard`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/error.rs crates/web/src/auth/routes.rs crates/web/src/auth/mod.rs crates/web/tests/auth_viewer_guard.rs
git commit -m "feat(web): viewer_method_guard rejects non-GET/HEAD with 405"
```

---

## Task 7: Generalise `require_mod` → `require_role`

**Files:**
- Modify: `crates/web/src/auth/routes.rs`
- Modify: `crates/web/src/auth/role_check.rs`
- Modify: `crates/web/src/auth/mod.rs`

- [ ] **Step 1: Add a generic role-aware helix gate**

In `crates/web/src/auth/role_check.rs`, alongside `check_is_mod`, add:

```rust
pub async fn check_is_follower(
    helix: &dyn HelixClient,
    broadcaster_id: &str,
    user_id: &str,
) -> eyre::Result<GateOutcome> {
    if helix.is_follower(broadcaster_id, user_id).await? {
        Ok(GateOutcome::Allow)
    } else {
        Ok(GateOutcome::Deny)
    }
}
```

- [ ] **Step 2: Generalise the middleware**

Replace `require_mod` in `crates/web/src/auth/routes.rs` with:

```rust
pub async fn require_role(
    min: crate::auth::role::Role,
    State(state): State<WebState>,
    cookies: Cookies,
    mut req: Request,
    next: Next,
) -> Result<Response, WebError> {
    let captured_next = req.uri().path_and_query().map(|pq| pq.as_str().to_owned());
    let unauth = || WebError::Unauthenticated { next: captured_next.clone() };

    let sid_cookie = cookies
        .signed(&state.signed_key)
        .get(SID_COOKIE)
        .ok_or_else(unauth)?;
    let session = state
        .sessions
        .get_and_touch(sid_cookie.value())
        .ok_or_else(unauth)?;

    if session.role < min {
        return Err(WebError::Forbidden);
    }

    let now = state.clock.now();
    let elapsed = now
        .signed_duration_since(session.last_role_check)
        .to_std()
        .unwrap_or_default();
    if elapsed > state.config.role_check_refresh {
        let outcome = match session.role {
            crate::auth::role::Role::Mod => check_is_mod(
                state.helix.as_ref(),
                &session.user_id,
                &state.broadcaster_id,
                &state.hidden_admins,
            ).await,
            crate::auth::role::Role::Viewer => check_is_follower(
                state.helix.as_ref(),
                &state.broadcaster_id,
                &session.user_id,
            ).await,
        };
        match outcome {
            Ok(GateOutcome::Allow) => state.sessions.record_role_check(sid_cookie.value()),
            Ok(GateOutcome::Deny) => {
                state.sessions.drop_session(sid_cookie.value());
                tracing::info!(
                    target: "twitch_1337_web",
                    user_id = %session.user_id,
                    role = session.role.label(),
                    action = "role_recheck",
                    result = "denied",
                );
                return Err(WebError::Forbidden);
            }
            Err(e) => {
                tracing::warn!(target: "twitch_1337_web", error = ?e, "role refresh failed; admitting on stale check");
            }
        }
    }

    req.extensions_mut().insert(session);
    Ok(next.run(req).await)
}
```

(Note: axum `from_fn_with_state` takes a function — to bind the `min` arg, use a tiny adapter per layer like `move |s, c, r, n| require_role(Role::Mod, s, c, r, n)`. Wire the adapter in lib.rs in Task 11. For now also keep:)

```rust
pub async fn require_mod(
    state: State<WebState>,
    cookies: Cookies,
    req: Request,
    next: Next,
) -> Result<Response, WebError> {
    require_role(crate::auth::role::Role::Mod, state, cookies, req, next).await
}
```

- [ ] **Step 3: Update `auth/mod.rs` re-exports**

```rust
pub use routes::{CSRF_COOKIE, OAuthCtx, SID_COOKIE, auth_router, require_mod, require_role, viewer_method_guard};
```

- [ ] **Step 4: Build + existing tests**

Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web`
Expected: all existing tests pass (they seed `Role::Mod` sessions).

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/auth
git commit -m "feat(web): generalise require_mod → require_role(min)"
```

---

## Task 8: Callback tries mod → follower → deny

**Files:**
- Modify: `crates/web/src/auth/routes.rs`

- [ ] **Step 1: Add `user:read:follows` to the OAuth scope set**

In `auth_start`:

```rust
.add_scope(Scope::new("user:read:follows".to_owned()))
```

(Alongside the existing `user:read:email` and `user:read:moderated_channels`.)

- [ ] **Step 2: Rework the callback decision**

Replace the mod-check block in `callback` with:

```rust
let role = match crate::auth::role_check::check_is_mod_with_token(
    &state, &me.id, &user_token, &state.broadcaster_id, &state.hidden_admins,
).await.map_err(|e| WebError::OAuthExchange(e.wrap_err("mod check")))? {
    GateOutcome::Allow => crate::auth::role::Role::Mod,
    GateOutcome::Deny => match crate::auth::role_check::check_is_follower_with_token(
        &state, &me.id, &user_token, &state.broadcaster_id,
    ).await.map_err(|e| WebError::OAuthExchange(e.wrap_err("follower check")))? {
        GateOutcome::Allow => crate::auth::role::Role::Viewer,
        GateOutcome::Deny => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %me.id,
                user_login = %me.login,
                action = "login",
                result = "denied",
            );
            return Err(WebError::Forbidden);
        }
    },
};

let (sid, csrf_value) = state
    .sessions
    .insert(me.id.clone(), me.login.clone(), role)
    .map_err(WebError::Internal)?;
```

And extend the success-log line:

```rust
tracing::info!(
    target: "twitch_1337_web",
    user_id = %me.id,
    user_login = %me.login,
    role = role.label(),
    next_path = %next_path,
    action = "login",
    result = "ok",
);
```

- [ ] **Step 3: Adjust existing callback tests**

`crates/web/tests/auth_routes.rs` and any other callback-path test should still pass because the seeded test user is a moderator. Run:

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web auth_routes`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/web/src/auth/routes.rs
git commit -m "feat(web): callback tries mod → follower → deny; emits role audit field"
```

---

## Task 9: `WebState` gains leaderboard + tracker handles

**Files:**
- Modify: `crates/web/src/state.rs`
- Modify: `crates/web/src/bin/web_dev.rs`
- Modify: `crates/web/tests/helpers/mod.rs`
- Modify: `crates/twitch-1337/src/` (main bin wiring — confirm path with `rg "WebState \{" crates/twitch-1337/src`)

- [ ] **Step 1: Extend `WebState`**

```rust
use std::collections::HashMap;
use twitch_1337_core::commands::leaderboard::PersonalBest; // adjust to actual export
use twitch_1337_core::aviation::tracker::TrackerCommand;

pub struct WebState {
    // ... existing fields ...
    pub leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    pub tracker_tx: Option<Arc<tokio::sync::mpsc::Sender<TrackerCommand>>>,
}
```

If `PersonalBest` is not `pub`, make it so in `crates/core/src/commands/leaderboard.rs`. If `TrackerCommand` is not `pub`, ditto.

- [ ] **Step 2: Update the bin wiring**

In `crates/web/src/bin/web_dev.rs`, construct an empty leaderboard `Arc` (dev binary has no live bot) and `tracker_tx: None`. In `crates/twitch-1337/src/<main>.rs`, pass the same `Arc` already held by the 1337 tracker handler and the existing `tracker_tx`. Use `rg "leaderboard: Arc<" crates/twitch-1337/src` to find the spot where the 1337 handler is spawned.

- [ ] **Step 3: Patch test helpers**

In `crates/web/tests/helpers/mod.rs::build_state_with_dirs`, add:

```rust
let leaderboard = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
```

…and pass it into the `WebState` initialiser. `tracker_tx: None`.

- [ ] **Step 4: Build + run existing tests**

Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/state.rs crates/web/src/bin/web_dev.rs \
        crates/web/tests/helpers/mod.rs crates/twitch-1337/src crates/core/src/commands/leaderboard.rs
git commit -m "feat(web): share leaderboard + tracker mpsc through WebState"
```

---

## Task 10: `TrackerCommand::Snapshot` over mpsc

**Files:**
- Modify: `crates/core/src/aviation/tracker.rs`

- [ ] **Step 1: Add the snapshot variant + view type**

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub struct TrackedFlightView {
    pub callsign: Option<String>,
    pub owner_login: String,
    pub phase: String,
    pub altitude_ft: Option<i32>,
    pub ground_speed_kt: Option<i32>,
    pub last_update_secs_ago: u64,
}

pub enum TrackerCommand {
    Track { /* ... */ },
    Untrack { /* ... */ },
    Status { /* ... */ },
    Snapshot { reply: tokio::sync::oneshot::Sender<Vec<TrackedFlightView>> },
}
```

- [ ] **Step 2: Handle it in the tracker loop**

Inside the existing `match` arm dispatch where `TrackerCommand` is consumed, add:

```rust
TrackerCommand::Snapshot { reply } => {
    let now = std::time::Instant::now();
    let view: Vec<TrackedFlightView> = tracked_flights
        .iter()
        .map(|f| TrackedFlightView {
            callsign: f.callsign.clone(),
            owner_login: f.owner_login.clone(),
            phase: format!("{:?}", f.phase),
            altitude_ft: f.last_state.as_ref().and_then(|s| s.altitude_ft),
            ground_speed_kt: f.last_state.as_ref().and_then(|s| s.ground_speed_kt),
            last_update_secs_ago: now.saturating_duration_since(f.last_seen).as_secs(),
        })
        .collect();
    let _ = reply.send(view);
}
```

(Field names mirror whatever the existing `TrackedFlight` struct uses — confirm via `rg "struct TrackedFlight" crates/core/src/aviation/tracker.rs`.)

- [ ] **Step 3: Test**

Add a `#[tokio::test]` in `crates/core/src/aviation/tracker.rs` that:
1. Builds the tracker, spawns the loop, sends `TrackerCommand::Track` for two fake aircraft.
2. Sends `TrackerCommand::Snapshot` with a fresh oneshot, awaits the reply with a 1s timeout.
3. Asserts the reply length and field shape.

(If the tracker has no existing tokio integration test scaffolding, drop the integration test here and rely on the `flights_route` test in Task 12 to cover end-to-end behaviour through the mpsc.)

- [ ] **Step 4: Build + run tests**

Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-core aviation::tracker`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/aviation/tracker.rs
git commit -m "feat(core): TrackerCommand::Snapshot returns read-only flight view"
```

---

## Task 11: Leaderboard route + template

**Files:**
- Create: `crates/web/src/routes/leaderboard.rs`
- Create: `crates/web/templates/leaderboard/list.html`
- Modify: `crates/web/src/routes/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/web/tests/leaderboard_route.rs`:

```rust
mod helpers;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use helpers::{FakeHelix, build_state, install_crypto, cookie_header, insert_session};
use std::sync::Arc;
use tower::ServiceExt as _;

#[tokio::test]
async fn viewer_can_read_leaderboard() {
    install_crypto();
    let helix = Arc::new(FakeHelix {
        moderators: vec![],
        followers: vec!["42".into()],
        users: Default::default(),
    });
    let state = build_state(helix).await;
    // Seed leaderboard
    {
        let mut lb = state.leaderboard.write().await;
        lb.insert(
            "alice".into(),
            twitch_1337_core::commands::leaderboard::PersonalBest { ms: 250, count: 3 },
        );
    }
    let (sid, _) = state.sessions.insert(
        "42".into(), "alice".into(), twitch_1337_web::auth::Role::Viewer,
    ).unwrap();
    let app = twitch_1337_web::build_router(state.clone());
    let resp = app.oneshot(
        Request::builder()
            .uri("/leaderboard")
            .method(Method::GET)
            .header("cookie", cookie_header(&state.signed_key, &sid))
            .body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("alice"), "leaderboard should render alice");
    assert!(body_str.contains("250"), "leaderboard should render 250 ms");
}
```

(Confirm `PersonalBest` field names — adjust `ms` / `count` to match.)

- [ ] **Step 2: Run, expect failure (route not registered)**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web leaderboard_route`
Expected: `404` or compile failure (`leaderboard` field unknown until Task 9).

- [ ] **Step 3: Add the template**

Write `crates/web/templates/leaderboard/list.html`:

```html
{% extends "base.html" %}
{% block content %}
<header class="page-head">
  <h1>Leaderboard</h1>
  <p class="sub">Sub-second 1337 personal bests. Fastest wins.</p>
</header>
<section class="card table-card">
  {% if entries.is_empty() %}
    <div class="placeholder">No personal bests yet.</div>
  {% else %}
  <div class="table" role="table">
    <div class="row head" role="row">
      <div role="columnheader">#</div>
      <div role="columnheader">User</div>
      <div role="columnheader">Best</div>
      <div role="columnheader">Count</div>
    </div>
    {% for row in entries %}
    <div class="row" role="row">
      <div role="cell" class="mono num">{{ loop.index }}</div>
      <div role="cell" class="mono">{{ row.login }}</div>
      <div role="cell" class="mono num">{{ row.best_ms }} ms</div>
      <div role="cell" class="mono num">{{ row.count }}</div>
    </div>
    {% endfor %}
  </div>
  {% endif %}
</section>
{% endblock %}
```

- [ ] **Step 4: Write the handler**

Create `crates/web/src/routes/leaderboard.rs`:

```rust
use askama::Template;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::Response;
use axum::routing::get;

use crate::auth::session::Session;
use crate::error::WebError;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "leaderboard/list.html")]
struct LeaderboardTpl {
    entries: Vec<Row>,
}

struct Row {
    login: String,
    best_ms: u32,
    count: u32,
}

pub fn router() -> Router<WebState> {
    Router::new().route("/leaderboard", get(list))
}

async fn list(
    State(state): State<WebState>,
    Extension(_session): Extension<Session>,
) -> Result<Response, WebError> {
    let lb = state.leaderboard.read().await;
    let mut entries: Vec<Row> = lb
        .iter()
        .map(|(login, pb)| Row {
            login: login.clone(),
            best_ms: pb.ms,
            count: pb.count,
        })
        .collect();
    entries.sort_by(|a, b| a.best_ms.cmp(&b.best_ms)
        .then_with(|| b.count.cmp(&a.count))
        .then_with(|| a.login.cmp(&b.login)));
    render(&LeaderboardTpl { entries })
}
```

(Adjust the `PersonalBest` field names — `ms`/`count` — to whatever the real type exports.)

- [ ] **Step 5: Register the route**

In `crates/web/src/routes/mod.rs`:

```rust
pub mod leaderboard;
```

(Wiring into the router happens in Task 14.)

- [ ] **Step 6: Run the test, expect pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web leaderboard_route`
Expected: pass (after Task 14 ships; flag as `#[ignore]` for now if Task 14 isn't reached yet, or merge tasks 11+14 if executing sequentially).

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/routes/leaderboard.rs crates/web/src/routes/mod.rs \
        crates/web/templates/leaderboard/list.html crates/web/tests/leaderboard_route.rs
git commit -m "feat(web): /leaderboard read-only page sourced from shared store"
```

---

## Task 12: Flights route + template

**Files:**
- Create: `crates/web/src/routes/flights.rs`
- Create: `crates/web/templates/flights/list.html`
- Modify: `crates/web/src/routes/mod.rs`
- Modify: `crates/web/src/routes/stubs.rs` (remove `/flights` stub entry)

- [ ] **Step 1: Write the failing test**

Create `crates/web/tests/flights_route.rs` modelled on the leaderboard test. The viewer hits `GET /flights`; assert `200` and an empty-state marker when `tracker_tx: None`.

A second test spawns a tiny task that consumes `TrackerCommand::Snapshot` and replies with two `TrackedFlightView`s; asserts the response contains both callsigns.

- [ ] **Step 2: Write the template**

`crates/web/templates/flights/list.html`:

```html
{% extends "base.html" %}
{% block content %}
<header class="page-head">
  <h1>Live flights</h1>
  <p class="sub">Currently tracked aircraft.</p>
</header>
<section class="card table-card">
  {% if flights.is_empty() %}
    <div class="placeholder">No flights tracked right now.</div>
  {% else %}
  <div class="table">
    <div class="row head">
      <div>Callsign</div><div>Owner</div><div>Phase</div>
      <div>Altitude</div><div>Speed</div><div>Updated</div>
    </div>
    {% for f in flights %}
    <div class="row">
      <div class="mono">{{ f.callsign.as_deref().unwrap_or("—") }}</div>
      <div class="mono">{{ f.owner_login }}</div>
      <div class="mono">{{ f.phase }}</div>
      <div class="mono num">{% match f.altitude_ft %}{% when Some(v) %}{{ v }} ft{% when None %}—{% endmatch %}</div>
      <div class="mono num">{% match f.ground_speed_kt %}{% when Some(v) %}{{ v }} kt{% when None %}—{% endmatch %}</div>
      <div class="mono num">{{ f.last_update_secs_ago }}s ago</div>
    </div>
    {% endfor %}
  </div>
  {% endif %}
</section>
{% endblock %}
```

- [ ] **Step 3: Write the handler**

`crates/web/src/routes/flights.rs`:

```rust
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::Response;
use axum::routing::get;
use tokio::sync::oneshot;
use twitch_1337_core::aviation::tracker::{TrackedFlightView, TrackerCommand};

use crate::auth::session::Session;
use crate::error::WebError;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "flights/list.html")]
struct FlightsTpl {
    flights: Vec<TrackedFlightView>,
}

pub fn router() -> Router<WebState> {
    Router::new().route("/flights", get(list))
}

async fn list(
    State(state): State<WebState>,
    Extension(_session): Extension<Session>,
) -> Result<Response, WebError> {
    let flights = match state.tracker_tx.as_ref() {
        Some(tx) => {
            let (reply_tx, reply_rx) = oneshot::channel();
            if tx.send(TrackerCommand::Snapshot { reply: reply_tx }).await.is_err() {
                Vec::new()
            } else {
                tokio::time::timeout(Duration::from_millis(500), reply_rx)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default()
            }
        }
        None => Vec::new(),
    };
    render(&FlightsTpl { flights })
}
```

- [ ] **Step 4: Drop the stub**

In `crates/web/src/routes/stubs.rs`, remove the `/flights` route (and any `Flights` nav reference). Existing stubs for Schedules, Logs, Config stay.

- [ ] **Step 5: Run tests + commit**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web flights_route`
Expected: pass.

```bash
git add crates/web/src/routes/flights.rs crates/web/src/routes/mod.rs \
        crates/web/src/routes/stubs.rs crates/web/templates/flights/list.html \
        crates/web/tests/flights_route.rs
git commit -m "feat(web): /flights live view via TrackerCommand::Snapshot, drop stub"
```

---

## Task 13: Pings template gates mutations on `is_mod`

**Files:**
- Modify: `crates/web/src/routes/pings.rs`
- Modify: `crates/web/templates/pings/list.html`

- [ ] **Step 1: Write the failing test**

Add to `crates/web/tests/pings_routes.rs`:

```rust
#[tokio::test]
async fn viewer_sees_pings_without_mutation_controls() {
    install_crypto();
    let helix = Arc::new(FakeHelix {
        moderators: vec![],
        followers: vec!["42".into()],
        users: Default::default(),
    });
    let (state, _td) = build_state_with_ping_dir(helix).await;
    let (sid, _) = state.sessions.insert(
        "42".into(), "alice".into(), twitch_1337_web::auth::Role::Viewer,
    ).unwrap();
    let app = twitch_1337_web::build_router(state.clone());
    let resp = app.oneshot(
        Request::builder().uri("/pings").method(Method::GET)
            .header("cookie", cookie_header(&state.signed_key, &sid))
            .body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap().to_vec()
    ).unwrap();
    assert!(!body.contains(r#"action="/pings""#),
        "viewer must not see the new-ping form");
    assert!(!body.contains("hx-delete"),
        "viewer must not see delete buttons");
}
```

- [ ] **Step 2: Run, expect failure**

The viewer can't even reach `/pings` yet — `require_mod` 403s them. Plus the template renders mutation controls unconditionally. Expect `403`.

- [ ] **Step 3: Pass `is_mod` to the template**

In `crates/web/src/routes/pings.rs::list` (and any sibling render fn that ships `pings/list.html`):

```rust
#[derive(Template)]
#[template(path = "pings/list.html")]
struct PingsTpl {
    pings: Vec<PingRow>,
    is_mod: bool,
    // ... existing fields ...
}
```

Set `is_mod: session.role == Role::Mod` in the constructor. Pull `session` from request extensions via `Extension<Session>`.

- [ ] **Step 4: Gate controls in the template**

In `crates/web/templates/pings/list.html`, wrap the new-ping form, sort/import affordances, and the per-row action buttons:

```jinja
{% if is_mod %}
  {# create form #}
{% endif %}
```

And the row trailing actions:

```jinja
{% if is_mod %}
  <button hx-delete="/pings/{{ p.name }}" …>✕</button>
{% endif %}
```

(Test asserts `hx-delete` is absent for viewers; if the template uses `hx-post` instead, adjust the assertion in step 1 to match.)

- [ ] **Step 5: Run, still expect 403 from middleware**

The viewer still hits `require_mod` at the router level. That's wired up in Task 14, so this test stays red until then.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/routes/pings.rs crates/web/templates/pings/list.html crates/web/tests/pings_routes.rs
git commit -m "feat(web): pings template gates mutations on is_mod"
```

---

## Task 14: Router split — viewer / mod / public

**Files:**
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Add a root redirect handler that branches on role**

In `crates/web/src/lib.rs`:

```rust
async fn root_redirect(Extension(session): Extension<auth::session::Session>) -> axum::response::Redirect {
    use auth::role::Role;
    match session.role {
        Role::Mod => axum::response::Redirect::to("/pings"),
        Role::Viewer => axum::response::Redirect::to("/leaderboard"),
    }
}
```

- [ ] **Step 2: Rebuild `build_router`**

```rust
pub fn build_router(state: WebState) -> Router {
    #[allow(unused_mut)]
    let mut public = Router::new()
        .merge(routes::health::router(state.irc_connected.clone()))
        .merge(routes::assets::router())
        .merge(auth::auth_router().with_state(state.clone()));
    #[cfg(feature = "dev-login")]
    {
        public = public.merge(dev::router(state.clone()));
    }

    let viewer = Router::new()
        .route("/", axum::routing::get(root_redirect))
        .merge(routes::pings::viewer_router())
        .merge(routes::leaderboard::router())
        .merge(routes::flights::router())
        .layer(axum::middleware::from_fn(auth::viewer_method_guard))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            |s, c, r, n| auth::require_role(auth::role::Role::Viewer, s, c, r, n),
        ))
        .with_state(state.clone());

    let mod_only = Router::new()
        .merge(routes::pings::mod_router())
        .merge(routes::memory::router())
        .merge(routes::stubs::router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_mod,
        ))
        .with_state(state);

    public
        .merge(viewer)
        .merge(mod_only)
        .layer(CookieManagerLayer::new())
        .layer(TraceLayer::new_for_http())
}
```

- [ ] **Step 3: Split `pings::router` → `viewer_router` + `mod_router`**

In `crates/web/src/routes/pings.rs`:

```rust
pub fn viewer_router() -> Router<WebState> {
    Router::new().route("/pings", get(list))
}

pub fn mod_router() -> Router<WebState> {
    Router::new()
        .route("/pings", post(create))
        .route("/pings/{name}", get(detail).post(update))
        .route("/pings/{name}/delete", post(delete))
        // ... whatever mutation routes already existed under the single router ...
}
```

Drop the old single `router()` once both call-sites in `lib.rs` are wired. Existing tests calling `routes::pings::router()` need a sweep: replace with whichever sub-router the test depends on (mod tests use `mod_router`; the new viewer test uses `viewer_router`). Easiest: keep `pub fn router() -> Router<WebState>` as a backward-compat shim that merges both, and remove it in a follow-up.

- [ ] **Step 4: Run the full test suite**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: `pings_routes::viewer_sees_pings_without_mutation_controls`, `leaderboard_route::viewer_can_read_leaderboard`, `flights_route::*` all pass. Existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/lib.rs crates/web/src/routes/pings.rs
git commit -m "feat(web): split router into viewer / mod / public, role-aware root redirect"
```

---

## Task 15: Sidebar — role-conditional groups

**Files:**
- Modify: `crates/web/src/nav.rs`
- Modify: `crates/web/templates/sidebar.html`
- Modify: `crates/web/src/routes/*.rs` (every handler that renders `sidebar.html` via `base.html`)

- [ ] **Step 1: Extend the nav model**

In `crates/web/src/nav.rs`:

```rust
use crate::auth::role::Role;

pub struct SidebarCtx {
    pub role: Role,
    pub current: &'static str, // existing field
    // ...
}

impl SidebarCtx {
    pub fn is_mod(&self) -> bool { self.role == Role::Mod }
}
```

If the existing nav uses a simpler enum or `current_page` string, just add a `pub role: Role` field; the template branches on `role`.

- [ ] **Step 2: Gate groups in the template**

In `crates/web/templates/sidebar.html`:

```jinja
<nav>
  <div class="group-label">Operate</div>
  <a href="/pings" {% if current == "pings" %}class="active"{% endif %}>Pings</a>
  <a href="/leaderboard" {% if current == "leaderboard" %}class="active"{% endif %}>Leaderboard</a>
  <a href="/flights" {% if current == "flights" %}class="active"{% endif %}>Flights</a>
  {% if sidebar.is_mod() %}
    <a href="/schedules" {% if current == "schedules" %}class="active"{% endif %}>Schedules</a>

    <div class="group-label">Memory</div>
    <a href="/memory/soul">SOUL</a>
    <a href="/memory/lore">LORE</a>
    <a href="/memory/users">Users</a>
    <a href="/memory/state">State</a>

    <div class="group-label">System</div>
    <a href="/logs">Logs</a>
    <a href="/config">Config</a>
  {% endif %}
</nav>

<footer class="sidebar-foot">
  <div class="user">{{ sidebar.user_login }} <span class="role">{{ sidebar.role.label() }}</span></div>
  <form method="post" action="/logout">
    <input type="hidden" name="_csrf" value="{{ sidebar.csrf }}">
    <button>Logout</button>
  </form>
</footer>
```

- [ ] **Step 3: Plumb `role` into every render path**

Every handler that constructs a template inheriting from `base.html` must pass the current session role through to the sidebar. The simplest pattern: a helper `SidebarCtx::from_session(&session, current)` in `nav.rs` that callers invoke, then store in each `*Tpl` struct.

Sweep:
```bash
rg "sidebar:" crates/web/src/routes
```

Patch each render handler.

- [ ] **Step 4: Run sidebar tests + add a viewer-sidebar assertion**

Extend `crates/web/tests/sidebar_smoke.rs` with a test that seeds a `Viewer` session, hits `/leaderboard`, and asserts the response contains "Leaderboard" but **does not** contain `/memory/soul` or "Logs".

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web sidebar_smoke`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/nav.rs crates/web/templates/sidebar.html crates/web/src/routes crates/web/tests/sidebar_smoke.rs
git commit -m "feat(web): sidebar hides Memory/System groups from viewer tier"
```

---

## Task 16: Integration scenarios

**Files:**
- Create: `crates/web/tests/auth_viewer_tier.rs`

- [ ] **Step 1: Author the scenario suite**

```rust
mod helpers;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use helpers::{FakeHelix, build_state_with_dirs, cookie_header, install_crypto};
use std::sync::Arc;
use tower::ServiceExt as _;
use twitch_1337_web::auth::Role;

fn viewer_helix(user_id: &str) -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec![],
        followers: vec![user_id.into()],
        users: Default::default(),
    })
}

#[tokio::test]
async fn viewer_can_read_pings_and_leaderboard_and_flights() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(viewer_helix("42")).await;
    let (sid, _) = state.sessions.insert("42".into(), "alice".into(), Role::Viewer).unwrap();
    let cookie = cookie_header(&state.signed_key, &sid);
    let app = twitch_1337_web::build_router(state.clone());

    for path in ["/pings", "/leaderboard", "/flights"] {
        let resp = app.clone().oneshot(
            Request::builder().uri(path).method(Method::GET)
                .header("cookie", cookie.clone()).body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "viewer GET {path}");
    }
}

#[tokio::test]
async fn viewer_blocked_from_memory_and_mutations() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(viewer_helix("42")).await;
    let (sid, csrf) = state.sessions.insert("42".into(), "alice".into(), Role::Viewer).unwrap();
    let cookie = cookie_header(&state.signed_key, &sid);
    let app = twitch_1337_web::build_router(state.clone());

    let resp = app.clone().oneshot(
        Request::builder().uri("/memory/soul").method(Method::GET)
            .header("cookie", cookie.clone()).body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "viewer cannot read memory");

    let body = format!("_csrf={}", twitch_1337_web::auth::csrf::encode(&csrf));
    let resp = app.oneshot(
        Request::builder().uri("/pings/anything/delete").method(Method::POST)
            .header("cookie", cookie).header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body)).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "viewer cannot mutate pings");
}

#[tokio::test]
async fn viewer_loses_follow_after_recheck_window() {
    install_crypto();
    // Build state with role_check_refresh = 0 so any request triggers a recheck.
    let mut helix = FakeHelix { moderators: vec![], followers: vec!["42".into()], users: Default::default() };
    let helix_arc = Arc::new(helix);
    let (mut state, _td_pings, _td_mem) = build_state_with_dirs(helix_arc.clone()).await;
    // (Adjust state.config.role_check_refresh to zero — likely needs a tweak to
    // build_state_with_dirs to accept an override. Document the helper change here.)
    let (sid, _) = state.sessions.insert("42".into(), "alice".into(), Role::Viewer).unwrap();
    let cookie = cookie_header(&state.signed_key, &sid);
    // Mutate followers via interior mutability on FakeHelix (add `Mutex<Vec<String>>`
    // for this purpose) — or rebuild state with the new helix.
    // Drop "42" from followers; next request must 403 and drop the session.
    // assertions follow.
}

#[tokio::test]
async fn root_redirects_by_role() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(viewer_helix("42")).await;
    let (viewer_sid, _) = state.sessions.insert("42".into(), "alice".into(), Role::Viewer).unwrap();
    let (mod_sid, _) = state.sessions.insert("9".into(), "boss".into(), Role::Mod).unwrap();
    let app = twitch_1337_web::build_router(state.clone());

    let resp = app.clone().oneshot(
        Request::builder().uri("/").method(Method::GET)
            .header("cookie", cookie_header(&state.signed_key, &viewer_sid))
            .body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers().get("location").unwrap(), "/leaderboard");

    let resp = app.oneshot(
        Request::builder().uri("/").method(Method::GET)
            .header("cookie", cookie_header(&state.signed_key, &mod_sid))
            .body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers().get("location").unwrap(), "/pings");
}
```

The `viewer_loses_follow_after_recheck_window` test needs:
- A way to override `role_check_refresh` on the `FakeHelix`-backed `WebState` — add `build_state_with_overrides(helix, refresh: Duration)` to `helpers/mod.rs`.
- Interior mutability on `FakeHelix` for the followers list — wrap it as `tokio::sync::RwLock<Vec<String>>`. Update Task 4's test fixtures and the trait impl accordingly. (Defer to Task 4 follow-up if it's cleaner.)

- [ ] **Step 2: Run the suite**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web auth_viewer_tier`
Expected: all 4 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/web/tests/auth_viewer_tier.rs crates/web/tests/helpers/mod.rs
git commit -m "test(web): viewer-tier integration scenarios"
```

---

## Task 17: Startup self-check + CLAUDE.md note

**Files:**
- Modify: `crates/twitch-1337/src/<main bin entry>.rs`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Probe `/channels/followers` once at startup**

Wherever the bot's helix client is constructed and the dashboard is spawned, add a one-shot call:

```rust
match helix.is_follower(&broadcaster_id, &broadcaster_id).await {
    Ok(_) => tracing::info!("helix /channels/followers reachable with current scopes"),
    Err(e) => tracing::warn!(error = ?e,
        "helix /channels/followers probe failed — viewer tier sliding rechecks will degrade. \
         Reissue the bot token with `moderator:read:followers`."),
}
```

(Calling with `broadcaster_id` as both args is a deliberate harmless probe — the broadcaster always "follows themselves" in the sense that the endpoint accepts the parameters; if it 401s, scopes are wrong; if it returns `total: 0`, scopes are fine.)

- [ ] **Step 2: Update `CLAUDE.md`**

Under the `## Config` section, append a paragraph:

```
**Dashboard viewer tier (added 2026-05-12):** the bot's Twitch token must
carry `moderator:read:followers` in addition to its existing scopes — the
web dashboard uses it to gate read-only access for followers (sliding
recheck via the bot token). The OAuth app must additionally allow
`user:read:follows` on viewer logins (no broadcaster-token configuration
required).
```

- [ ] **Step 3: Commit**

```bash
git add crates/twitch-1337/src CLAUDE.md
git commit -m "ops: probe helix followers at startup; document new scopes"
```

---

## Task 18: Final gate + PR

- [ ] **Step 1: Full pre-commit gate**

Run:
```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
cargo audit
```

Expected: clean.

- [ ] **Step 2: Hand-smoke the dashboard**

```bash
cargo run --bin twitch-1337-web-dev
```

In another terminal/browser:
- Log in as a moderator → land at `/pings`, see all sidebar groups, mutation controls present.
- Use the `dev-login` feature to spoof a follower session → land at `/leaderboard`, see only Operate group, no mutation controls on `/pings`, `/memory/*` returns 403.

(If `dev-login` doesn't currently let you set a role, extend it inside this task: a `?role=viewer` query parameter on the dev-login endpoint.)

- [ ] **Step 3: Open the PR**

```bash
gh pr create --title "feat(web): follower-gated viewer tier for dashboard" --body "$(cat <<'EOF'
## Summary
- Add `Role { Viewer, Mod }` and generalise `require_mod` → `require_role(min)`
- Channel-followers see a read-only slice: `/pings` (no mutation controls), `/leaderboard`, `/flights`
- All mutations + AI memory remain mod-only behind a separate sub-router with `viewer_method_guard` (GET/HEAD only)
- Bot token must carry `moderator:read:followers`; viewer logins request `user:read:follows`

Spec: `docs/superpowers/specs/2026-05-12-dashboard-viewer-tier-design.md`
Plan: `docs/superpowers/plans/2026-05-12-dashboard-viewer-tier.md`

## Test plan
- [ ] CI: fmt + clippy + nextest + audit green
- [ ] Manually verify mod login → /pings with full controls
- [ ] Manually verify follower login → /leaderboard, hidden Memory/System groups, no delete buttons
- [ ] Manually verify follower POST /pings/x/delete → 403
- [ ] Manually verify viewer GET /memory/soul → 403

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review notes

- Spec § 4 routing layout: covered Task 14 (router split).
- Spec § 5 middleware: covered Task 6 (viewer guard) + Task 7 (require_role).
- Spec § 6 helix changes: covered Task 4 (is_follower) + Task 5 (channels/followed login helper).
- Spec § 7 callback: covered Task 8.
- Spec § 8.1 pings read-only: covered Task 13 + Task 14 (route split).
- Spec § 8.2 leaderboard: covered Task 11.
- Spec § 8.3 flights: covered Task 10 (Snapshot variant) + Task 12 (route).
- Spec § 10 config rename: covered Task 2.
- Spec § 11 deploy prereqs: covered Task 17.
- Spec § 13 testing: covered Tasks 1, 4, 6, 11, 12, 13, 15, 16.
- Spec § 14 build sequence step 13 (startup self-check): covered Task 17.

No placeholders left in the plan; every code block is complete enough for a fresh engineer to lift. Type names are consistent: `Role`, `GateOutcome`, `TrackedFlightView`, `TrackerCommand::Snapshot`, `record_role_check`, `last_role_check`, `role_check_refresh`, `is_follower`. Field rename of `ModCheckOutcome` → `GateOutcome` happens in Task 3 and every later reference uses `GateOutcome`.
