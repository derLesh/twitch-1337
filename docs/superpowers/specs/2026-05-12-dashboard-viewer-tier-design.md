# Dashboard viewer tier (follower read-only access)

Status: design / ready for plan.

## 1. Problem

The web dashboard (`crates/web/`) is mod-gated end-to-end: the broadcaster, configured `hidden_admins`, and helix moderators of the channel are the only authenticated audience. Everyone else gets `403` at `require_mod`.

We want to let the wider community see a curated slice of the dashboard (leaderboard, currently tracked flights, the ping catalogue) while keeping all mutation surfaces and AI memory mod-only.

## 2. Goals / non-goals

**Goals**

- Add a viewer tier gated by "is currently a follower of the broadcaster".
- Viewer tier is strictly read-only.
- Mods retain full access (no regression).
- Reuse the existing session / sliding-recheck machinery; do not invent a parallel auth path.
- Surface two new pages (leaderboard, live flights) and a stripped-down pings index.

**Non-goals**

- Sub-only or VIP-only tiers. Followers covers the chosen audience for v1; sub gating was considered and dropped because it requires either a broadcaster-token in config or persisting per-user OAuth tokens. See § 9.
- Public unauthenticated access. Viewer still logs in via Twitch OAuth.
- Per-user permissions / capabilities. Two ordered roles (`Viewer` < `Mod`) cover the requirement; capability sets are explicitly YAGNI here.
- Writes by viewers — not even self-scoped writes. The route layer rejects non-GET/HEAD unconditionally.

## 3. Roles

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Role { Viewer, Mod }
```

`Role` is ordered: `session.role >= required` is the gate predicate. Adding tiers later means inserting in the enum at the right ordinal — no call-site churn.

Stored on `Session` alongside `user_id`, `user_login`, `last_role_check` (renamed from `last_mod_check`).

## 4. Routing layout

`crates/web/src/lib.rs::build_router` becomes:

```
public                                       (no auth)
  /healthz, /login, /auth/start, /auth/callback, /logout, /assets/*

viewer  (require_role(Role::Viewer) + viewer_method_guard)
  GET /                       → role-aware redirect (viewer→/leaderboard, mod→/pings)
  GET /pings                  read-only render; mutation controls hidden by template
  GET /leaderboard            NEW
  GET /flights                NEW — replaces existing Flights stub

mod     (require_role(Role::Mod))
  POST   /pings, /pings/{name}/delete, /pings/{name}/edit
  GET    /memory/{soul,lore,users,state}/...
  POST   /memory/...   (existing CRUD, unchanged)
```

Two nested `Router::merge` layers under a single `CookieManagerLayer`. `viewer_method_guard` is a small `axum::middleware::from_fn` that returns `405 Method Not Allowed` for any method other than `GET`/`HEAD` on the viewer router — belt-and-suspenders: even if a write route were accidentally registered under the viewer layer, mutation can't slip through.

The mod router stays mounted on the same paths it owns today; nothing moves.

## 5. Middleware

### 5.1 `require_role(min: Role)`

Generalisation of the current `require_mod`. Responsibilities:

1. Read signed `tw1337_sid`; load and touch session. Missing/expired → `WebError::Unauthenticated { next }`.
2. If `session.role < min` → `WebError::Forbidden` (no recheck; static role mismatch).
3. If `elapsed_since(session.last_role_check) > config.role_check_refresh`:
   - If `session.role == Role::Mod`: call `check_is_mod(helix, …)` (existing path).
   - If `session.role == Role::Viewer`: call `check_is_follower(helix, broadcaster_id, user_id)` (new).
   - `Allow` → `record_role_check`. `Deny` → drop session, return `WebError::Forbidden`. `Err` → log + admit on stale check (existing policy preserved).
4. Insert the session into request extensions; pass to next handler.

`require_mod` becomes `pub async fn require_mod(...) { require_role(Role::Mod) ... }` — a thin alias so we don't fan out the rename at every call-site at once.

### 5.2 `viewer_method_guard`

```rust
async fn viewer_method_guard(req: Request, next: Next) -> Result<Response, WebError> {
    match *req.method() {
        Method::GET | Method::HEAD => Ok(next.run(req).await),
        _ => Err(WebError::MethodNotAllowed),
    }
}
```

Add `MethodNotAllowed` variant to `WebError`, mapping to `405`.

## 6. Helix changes

### 6.1 Trait

`crates/web/src/helix.rs::HelixClient` gains:

```rust
async fn is_follower(&self, broadcaster_id: &str, user_id: &str) -> eyre::Result<bool>;
```

Production impl calls `GET /helix/channels/followers?broadcaster_id=…&user_id=…` with the bot's helix user token (the same token the existing `check_is_mod` path uses). The endpoint requires the bearer to be a moderator of the broadcaster's channel (`moderator:read:followers` scope) — the bot already moderates the channel, so this works with a single scope addition. The response shape is `{ "total": N, "data": [...] }`; the predicate is `!data.is_empty()` (or `total > 0`).

`FakeHelixClient` in `crates/web/tests/common/` gains a matching `set_followers(Vec<(String, String)>)` builder so integration tests can configure follower state alongside the existing `set_moderators`.

### 6.2 Login-path check via the user's own token

The OAuth callback already authenticates the *user's* helix calls with the user's just-issued access token. A symmetric helper:

```rust
pub async fn check_is_follower_with_token(
    state: &WebState,
    user_id: &str,
    user_access_token: &str,
    broadcaster_id: &str,
) -> eyre::Result<ModCheckOutcome>;
```

calls `GET /helix/channels/followed?user_id=…&broadcaster_id=…` with the user token and the `user:read:follows` scope. This avoids depending on the bot token at first-login.

(Rename file `auth/mod_check.rs` → `auth/role_check.rs`; keep `ModCheckOutcome` or rename to `GateOutcome` for honesty. Rename has no external impact beyond `auth::mod_check` re-exports in `auth/mod.rs`.)

### 6.3 OAuth scopes

`auth_start` adds `Scope::new("user:read:follows".into())` next to the existing `user:read:email` and `user:read:moderated_channels`.

Existing logged-in sessions are unaffected — sessions are cookie-backed in-memory state, not token-backed; the OAuth token from earlier logins isn't replayed.

### 6.4 Bot token scope dependency

Bot's helix token (used by the sliding role recheck) must carry `moderator:read:followers`. This is a deploy prerequisite; the spec calls it out in § 11 and the implementation plan will fail loud on `401`.

## 7. Callback flow

`crates/web/src/auth/routes.rs::callback`:

1. Verify OAuth state cookie, exchange code, look up caller user (unchanged).
2. `check_is_mod_with_token(user_token, ...)` → `Allow` ⇒ insert session with `role = Mod`. Done.
3. Else `check_is_follower_with_token(user_token, ...)` → `Allow` ⇒ insert session with `role = Viewer`.
4. Else log `result=denied`, return `WebError::Forbidden`.

`SessionStore::insert` signature picks up `role: Role`; persistence still in-memory.

`tracing::info!` audit fields extend: `action=login, result=ok|denied, role=mod|viewer`.

## 8. Pages

### 8.1 Pings (read-only for viewers)

Handler passes `is_mod: bool` (derived from `session.role == Role::Mod`) into the template context. Existing `templates/pings/list.html` wraps:

- `+ New ping` button
- Row trailing-action ✎ / ✕ controls
- `Sort` / `Import` mutating affordances if any

…in `{% if is_mod %}` blocks. Row click and template/member columns stay visible.

POST routes (`/pings`, `/pings/{name}/delete`, `/pings/{name}/edit`) remain only on the mod router. The viewer router exposes `GET /pings` only; the form/edit pages are reachable from the mod router (which the viewer is forbidden from anyway).

### 8.2 Leaderboard `/leaderboard` (new)

- Data source: `LeaderboardStore` (already an `Arc` shared into bot handlers). Add a read handle into `WebState`. Reading is `leaderboard.snapshot()` → `Vec<LeaderboardEntry { user_id, login, best_ms, count }>`.
- Sort: ascending `best_ms`, ties broken by `count` desc then `login` asc.
- Template: a stat strip (entries total, fastest PB, today's fastest) above a CSS-grid table mirroring the Pings table chrome (per visual handoff DESIGN.md § 7.3). Columns: rank | user | best (mono tabular-nums) | count.
- Empty state: honest placeholder per the handoff's stub-page pattern; no fake rows.

### 8.3 Live flights `/flights` (new — replaces the existing stub)

- Data source: the flight tracker task. Today commands flow in over `Arc<mpsc::Sender<TrackerCommand>>`; the tracker holds the live state.
- Extend `TrackerCommand` with `Snapshot { reply: tokio::sync::oneshot::Sender<Vec<TrackedFlightView>> }`. `TrackedFlightView` is a serde-friendly read projection of the internal `TrackedFlight`: callsign, owner_login, phase, altitude_ft, ground_speed_kt, last_update_secs_ago. The handler `send`s the command and `await`s the oneshot with a short timeout (e.g. 500 ms — tracker loop is non-blocking, this is generous).
- Aviation-disabled fallback: `WebState` already holds an `Option<Arc<mpsc::Sender<TrackerCommand>>>`; `None` → render the existing stub.
- Template: table of active flights; empty state when zero are tracked.

Both new pages get a sidebar nav entry under the **Operate** group from the visual handoff (`Pings, Schedules, Flights` becomes `Pings, Leaderboard, Schedules, Flights`). Sidebar items are visibility-gated by role:

- Viewer: Operate (Pings, Leaderboard, Flights). Memory and System groups are hidden entirely.
- Mod: every group as before.

The `/` redirect handler reads `session.role` and redirects accordingly: `Mod → /pings`, `Viewer → /leaderboard`.

## 9. Why not subs

Sub-tier checks need a token that can call `/helix/subscriptions`:

- Broadcaster token (channel:read:subscriptions): a single new secret rotated by the broadcaster. Workable but adds an at-rest secret and a rotation chore.
- Per-user token persisted in the session: viewer issues `user:read:subscriptions`, we cache + refresh. Adds at-rest tokens and the refresh-token plumbing the bot doesn't otherwise carry today.

Followers covers "channel community" cleanly without either. If sub gating is wanted later, the cleanest path is the broadcaster-token approach plumbed through `HelixClient::is_subscriber` alongside `is_follower`; `Role` could grow `Subscriber` between `Viewer` and `Mod`.

## 10. Config

- No new config keys.
- Rename `WebConfig.mod_check_refresh` → `role_check_refresh`. Default unchanged. Config struct is internal (loaded from in-process state, not serialised to a user-edited file), so this is a code-only rename.

## 11. Deploy prerequisites

1. Bot's Twitch IRC/helix token reissued with **`moderator:read:followers`** added to its scope set. The bot already needs to be a moderator of the broadcaster's channel for the scope to be usable (existing invariant).
2. Twitch developer console OAuth app must permit **`user:read:follows`** as a requestable scope on the user-facing login. The code adds it to the authorise URL; the app config must allow it.
3. Document both in `CLAUDE.md § Config`.

Failure mode if (1) is forgotten: viewer sliding recheck logs the `401`, admits on stale check (preserved policy). Login still works because the login path uses the user token, not the bot token. A loud warning at startup that asserts the bot token can reach `/channels/followers` is a small addition — included in the plan.

## 12. Security considerations

- **Mutation surface**: viewer router has zero. `viewer_method_guard` enforces this at the layer level; route enumeration enforces it at the routing level.
- **CSRF**: viewer has no forms, no header-driven mutations. CSRF on mod mutations unchanged.
- **Session lifetime**: same TTL + sliding refresh as today. A demoted mod or unfollowed viewer is dropped on next request after the recheck TTL fires.
- **Log noise**: `action=role_recheck, result=denied, role=viewer` is expected churn (viewers unfollow). Not an error; log at `info`, not `warn`.
- **Open redirect / `?next=`**: existing `is_safe_redirect` filter unchanged. The root redirect handler is a server-issued `Redirect::to`, no user input.

## 13. Testing

### 13.1 Unit

- `Role` ordering: `Viewer < Mod`; `Mod >= Viewer`; `Viewer >= Viewer`.
- `check_is_follower` against `FakeHelixClient`: empty / matched / error → expected `GateOutcome`.
- `viewer_method_guard`: GET admitted, HEAD admitted, POST/PUT/DELETE/PATCH → `405`.

### 13.2 Integration (`tests/common/TestBotBuilder`)

Scenarios driven through the real router with a fake helix:

1. **Mod login**: callback issues `role=Mod`, can `GET /pings`, `GET /memory/soul`, `POST /pings/.../delete`. Regression coverage.
2. **Follower login**: callback issues `role=Viewer`. Can `GET /pings`, `/leaderboard`, `/flights`. `POST /pings/.../delete` → `403` (route lives on the mod router; `require_role(Mod)` rejects the viewer). `GET /memory/...` → `403`. `/` redirects to `/leaderboard`. Separately, hand-crafted `POST /leaderboard` (a viewer-router path with a method the guard rejects) → `405` from `viewer_method_guard`.
3. **Non-follower non-mod**: callback returns `403`, no session cookie issued.
4. **Follower-loses-follow mid-session**: recheck fakes `is_follower=false` after TTL; next request → `403` + session dropped.
5. **Mod-loses-mod mid-session**: existing test, kept green.
6. **Mod also follows**: lands as Mod, not Viewer (mod check runs first).

### 13.3 Template / render

- Render `templates/pings/list.html` with `is_mod=false`: assert no `<form action="/pings"`, no `data-action="delete"` (or whichever marker — exact assertion left to plan); assert row body still present.
- Render with `is_mod=true`: assert mutation controls present.

### 13.4 Flights snapshot

- Mock `TrackerCommand::Snapshot` returning 0 / 1 / many flights; assert empty-state branch vs table render. Confirm 500 ms timeout path doesn't poison the request (graceful degraded message).

## 14. Build sequence

Each step is independently mergeable / reviewable.

1. `Role` enum + `Session.role` + `Ord` impl + `SessionStore::insert(role)`. All call-sites (one) pass `Role::Mod` to start. No behaviour change.
2. Rename `last_mod_check` → `last_role_check`, `mod_check_refresh` → `role_check_refresh`, `mod_check.rs` → `role_check.rs`. Mechanical.
3. Add `HelixClient::is_follower` + production impl + `FakeHelixClient::set_followers`.
4. Add `check_is_follower_with_token` (user-token path).
5. Replace `require_mod` body with `require_role(Role::Mod)`; add `require_role` taking the minimum role parameter. `require_mod` becomes an alias.
6. Add `viewer_method_guard` + `WebError::MethodNotAllowed`.
7. Callback: try mod → try follower → deny. New OAuth scope `user:read:follows`. Audit field `role`.
8. Pings template: gate mutations on `is_mod`. Handler extension.
9. Leaderboard route + template + nav entry. `WebState` gains read handle.
10. Tracker `Snapshot` command + flights route + template + nav entry. Stub fallback when aviation is disabled.
11. Root `/` redirect by role.
12. Integration test scenarios per § 13.2.
13. Startup self-check: probe `/channels/followers` with the bot token; log `warn` if it `401`s so a missing `moderator:read:followers` scope is loud.

## 15. Open questions

- **Leaderboard time window**: "today" stat — do we want a today-PB chip in addition to all-time best? Punted to plan; the data is there in `leaderboard.ron` if we want it.
- **Flights view auth**: viewers see the owner login of each tracked flight. That's already public from the IRC `!flights` command, so no leak — flagged for sanity.
- **Sidebar group label for Leaderboard**: leaning **Operate** to match the visual handoff. Could argue for a new **Stats** group if we add more read-only pages later.
