# Dashboard-managed settings — design

**Status:** draft
**Date:** 2026-05-12
**Scope:** v1 plumbing + cooldowns + pings.public/cooldown
**Module:** `core::settings`

## 1. Goal

Move a small set of settings out of `config.toml` and into a dashboard-editable
store that applies changes immediately, without restarting the bot. Build the
plumbing once so subsequent settings sections (schedules, AI knobs, …) can be
migrated incrementally.

Migration is **a move, not an override**: once a section migrates, it stops
existing in `config.toml` entirely. `config.toml` retains credentials,
bootstrap configuration (channel, bind addr, owner), and sections not yet
migrated. Each migration is one section at a time — no big-bang flag day.

There is **no upgrade migrator**. The current deployment is a single instance
that has tolerated config drift on every prior change; switching from
`[cooldowns]` / `[pings]` in TOML to `settings.ron` may revert those values
to compile-time defaults on first boot of the new code. Explicitly accepted
trade-off in exchange for a simpler model.

## 2. Non-goals

- Migrating credentials (`refresh_token`, `client_id`, `client_secret`,
  `ai.api_key`) to the dashboard. Secrets stay in `config.toml`.
- Migrating schedules, AI knobs, aviation, suspend, or memory limits in v1.
  These get their own follow-up specs that reuse this plumbing.
- A tiered permission system. v1 has a single `owner` ID; tiered RBAC lands
  later.
- Audit-log viewer UI. The log is written in v1 as structured JSON so a
  viewer page can be added later for free.
- Schedule file-watcher replacement. The existing notify-debouncer-based
  watcher for `[[schedules]]` stays as-is until schedules migrate.

## 3. v1 settings scope

| Section | Key | Type | Bound |
|---|---|---|---|
| `cooldowns` | `ai` | `u64` (seconds) | `1..=3600` |
| `cooldowns` | `news` | `u64` | `1..=3600` |
| `cooldowns` | `up` | `u64` | `1..=3600` |
| `cooldowns` | `feedback` | `u64` | `1..=3600` |
| `cooldowns` | `doener` | `u64` | `1..=3600` |
| `pings` | `cooldown` | `u64` (seconds) | `1..=86400` |
| `pings` | `public` | `bool` | — |

Defaults match the values currently documented in `config.toml.example` and
are now compile-time constants in `core::settings`. After v1, the
`[cooldowns]` and `[pings]` sections are deleted from `config.toml.example`;
any keys still present in a deployed `config.toml` are ignored.

## 4. Architecture

### 4.1 Layers

Settings resolve in two layers, highest precedence first:

1. **`$DATA_DIR/settings.ron`** — sparse overrides written by the dashboard.
   Only keys that have ever been edited live here.
2. **Compile-time defaults** in `core::settings::defaults`.

`config.toml` is no longer involved in cooldowns or pings runtime knobs.
Sparse overrides remove the "what does an empty key mean" ambiguity and let
us delete an override (revert to default) without inventing a sentinel
value. "Reset to default" in the dashboard reverts to compile-time
constants.

### 4.2 Types

```rust
// crates/core/src/settings/mod.rs

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub schema_version: u32,
    pub cooldowns: Cooldowns,
    pub pings: PingsSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cooldowns {
    pub ai: u64,
    pub news: u64,
    pub up: u64,
    pub feedback: u64,
    pub doener: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PingsSettings {
    pub cooldown: u64,
    pub public: bool,
}

// Sparse on-disk shape (every field optional, every section optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsOverrides {
    pub schema_version: u32,
    #[serde(default)]
    pub cooldowns: CooldownsOverrides,
    #[serde(default)]
    pub pings: PingsOverrides,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CooldownsOverrides {
    #[serde(default)] pub ai: Option<u64>,
    #[serde(default)] pub news: Option<u64>,
    #[serde(default)] pub up: Option<u64>,
    #[serde(default)] pub feedback: Option<u64>,
    #[serde(default)] pub doener: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PingsOverrides {
    #[serde(default)] pub cooldown: Option<u64>,
    #[serde(default)] pub public: Option<bool>,
}

pub type SettingsHandle = Arc<arc_swap::ArcSwap<Settings>>;
```

`Settings::compiled_defaults() -> Settings` produces layer 2.
`Settings::resolve(&defaults, &overrides) -> Settings` merges
layers 1 + 2 (per-field: `Some` override wins, otherwise default).

### 4.3 SettingsStore

```rust
pub struct SettingsStore {
    path: PathBuf,                      // $DATA_DIR/settings.ron
    defaults: Settings,          // compile-time defaults (layer 2)
    handle: SettingsHandle,             // shared swap target
    audit: Arc<dyn AuditLog>,
    write_lock: tokio::sync::Mutex<()>, // serializes file writers
}

impl SettingsStore {
    pub async fn apply(&self, patch: SettingsOverrides, actor: Actor)
        -> Result<Settings, SettingsError>;

    pub async fn reset(&self, section: SettingsSection, actor: Actor)
        -> Result<Settings, SettingsError>;

    pub fn current(&self) -> arc_swap::Guard<Arc<Settings>>;
}
```

`apply` flow:

1. Acquire `write_lock`.
2. Re-read overrides from disk (never clobber concurrent external edits).
3. Merge `patch` over loaded overrides (per-field, `Some` wins).
4. `let resolved = Settings::resolve(&self.defaults, &merged)`.
5. `resolved.validate()?` — bounds check, all-or-nothing.
6. Atomic write: `settings.ron.tmp` → fsync → rename to `settings.ron`.
7. `handle.store(Arc::new(resolved.clone()))`.
8. `audit.append(actor, diff)`.
9. Return resolved (drop lock implicitly).

`reset(section, …)` clears that section's overrides (sets each field to
`None`) and re-runs the same flow. Resulting values fall through to
compile-time defaults.

`current()` is a thin pass-through to `handle.load()` for code that already
has a `&SettingsStore`. Handlers receive the bare `SettingsHandle` instead
and use it directly.

### 4.4 Validation

`Settings::validate(&self) -> Result<(), Vec<FieldError>>` runs the
bound checks. `FieldError { field: "cooldowns.ai", message: "…" }` so the
dashboard can render inline errors. The dashboard POST handler and the
startup loader both call this; on startup a corrupt or out-of-bound file
falls back to compile-time defaults with a logged error (never crash). The
corrupt file is renamed to `settings.ron.quarantine-<ts>` so a subsequent
dashboard save starts from a clean slate.

## 5. Propagation

Handlers hold `SettingsHandle` and read on each use:

```rust
let snapshot = self.settings.load();   // arc-swap guard, lock-free
let cd = Duration::from_secs(snapshot.cooldowns.ai);
```

No subscription, no `await`. Reads are O(1) and contention-free.

### 5.1 Wiring

- `Services` struct (in `crates/core/src/lib.rs`) gains
  `pub settings: SettingsHandle`.
- `WebState` (in `crates/web/src/state.rs`) gains `pub settings: SettingsHandle`
  and `pub settings_store: Arc<SettingsStore>` (store needed by the POST
  handler; bare handle is enough for read-only pages).
- Bin (`crates/web/src/bin/...` and `crates/twitch-1337/src/main.rs`)
  constructs one `SettingsStore` + one `SettingsHandle` and shares both via
  the existing `Arc`-clone pattern.

### 5.2 Call-site changes

- `PingManager::new` signature changes from owned cooldown/public to
  `SettingsHandle`; reads `.load().pings` per trigger evaluation.
- Cooldown handler call sites (`!ai`, `!news`, `!up`, `!fb`, `!dpi`) read
  `settings.load().cooldowns.<field>` instead of holding a cloned value.

No other handler touched in v1.

### 5.3 Concurrency model

- One in-process writer: the dashboard POST handler, serialized by
  `SettingsStore::write_lock`.
- N lock-free readers: any handler holding `SettingsHandle`.
- External edits to `settings.ron` while the bot runs are ignored (no
  watcher in v1). Documented in `config.toml.example`.
- `arc_swap` epoch-based reclamation handles reader/writer visibility; no
  torn reads possible.

## 6. Owner gate

### 6.1 Config

```toml
[twitch]
# Twitch user ID with full dashboard access, including settings page.
# Single value for v1; tiered permissions land later.
owner = "12345678"
```

`TwitchConfiguration` gains `pub owner: Option<String>`. Absent → no owner
exists; existing tiers behave exactly as today. Settings page returns 403
for everyone in that state.

### 6.2 Semantics

The owner ID is a **strict superset** of every existing tier. The owner can
read viewer pages, write mod pages, and edit settings, without also being
listed in `hidden_admins` or being the broadcaster.

| Tier | Today | v1 |
|---|---|---|
| `viewer_allowlist` | dashboard read | unchanged |
| `mod` | dashboard write | unchanged |
| `broadcaster` | dashboard write | unchanged |
| `hidden_admins` | chat ping admin | unchanged |
| **`owner`** | — | full dashboard + settings page |

Implementation: a `session_is_owner(&session, &state) -> bool` helper; every
existing tier check OR-s with it. New axum extractor `RequireOwner` gates
the `/settings` router.

### 6.3 Audit log

Append-only `$DATA_DIR/settings_audit.log`, one JSON object per line:

```json
{"ts":"2026-05-12T13:37:00+02:00","actor_id":"12345678","actor_login":"chronophylos","changes":[{"key":"cooldowns.ai","old":30,"new":15}]}
```

- One entry per dashboard POST (may carry multiple field changes).
- Berlin-local ISO 8601 timestamps.
- `fsync()` on each append; volume is tiny and durability matters.
- No rotation in v1.

`AuditLog` is a trait so tests can substitute `MemoryAuditLog`. Default impl
`FileAuditLog` opens the file in append mode per write to survive truncation
or unlink-during-run cleanly.

## 7. Dashboard UI

### 7.1 Routes

- `GET /settings` — render form, behind `RequireOwner`.
- `POST /settings` — validate, persist, swap, audit; redirect with flash.
- `POST /settings/reset/cooldowns` — clear `[cooldowns]` overrides.
- `POST /settings/reset/pings` — clear `[pings]` overrides.

CSRF via the existing `tower_cookies` signed-key pattern (already in
`WebState.signed_key`).

### 7.2 Layout

Two cards (cooldowns, pings) using the existing askama template style. Each
input shows its compile-time default in subdued text beside the field so
"override vs. inherited" is visible. Each card has a "Reset to default"
button that POSTs to the matching reset route.

On submit, redirect to `/settings` with a flash message of the form
`saved (3 changes)`. Validation errors re-render the form with inline
per-field errors; previously entered values are preserved.

### 7.3 Nav

Settings entry appears in the dashboard nav only when the current session
satisfies `session_is_owner`.

## 8. Migration plan

### 8.1 v1 cutover

This is a **move, not an override**. On the v1 commit:

- `[cooldowns]` and `[pings]` are deleted from `config.toml.example`.
- `Configuration` (in `core::config`) drops its `cooldowns` and `pings`
  fields. Keys still present in a deployed `config.toml` are ignored by
  serde (no `deny_unknown_fields` on these structs); operator clean-up is
  cosmetic.
- No runtime migrator is written. First boot of the new code with no
  `settings.ron` present reverts both sections to their compile-time
  defaults. The current single-instance deployment accepts this trade-off
  (operator can re-enter values via dashboard immediately after boot).

### 8.2 Schema versioning

`Settings::schema_version` starts at `1`.

- **Additive change** (new field with a default): no version bump.
- **Rename / type change**: bump `schema_version`, write a one-shot migrator
  in `settings::migrations` that runs on load and rewrites the file.
  Old file copied to `settings.ron.bak.<old_version>` before rewrite.
- **Migration failure**: log error, quarantine file to
  `settings.ron.quarantine-<ts>`, fall back to compile-time defaults,
  dashboard shows a banner pointing to the log.

### 8.3 Future sections

Each future migration repeats the same pattern:

1. Extend `Settings` + `SettingsOverrides`.
2. Add a card to the settings page.
3. Bump `schema_version` if breaking, else just add.
4. Switch handler read sites to `SettingsHandle::load()`.
5. Delete the section from `config.toml.example` and from the `Configuration`
   struct in the same commit. No transitional override layer.

Sections explicitly out of scope for v1 but already designed-for: schedule
CRUD, AI runtime knobs, aviation tuning, suspend defaults, memory byte
budgets. Each follows the same "delete from TOML in the migration commit"
rule and inherits this spec's "no runtime migrator" stance unless a future
section carries data the operator would not be able to re-enter from
memory.

## 9. File layout

```
crates/core/src/settings/
  mod.rs           — Settings, Cooldowns, PingsSettings, resolve(), validate()
  overrides.rs     — *Overrides structs, merge logic
  store.rs         — SettingsStore, apply, reset
  audit.rs         — AuditLog trait, FileAuditLog, MemoryAuditLog (test)
  migrations.rs    — schema_version migrators (empty in v1, scaffold only)

crates/web/src/auth/owner.rs
                   — RequireOwner extractor, session_is_owner helper

crates/web/src/routes/settings.rs
                   — GET/POST handlers, askama template, reset routes

crates/web/templates/settings.html
                   — page template

crates/twitch-1337/config.toml.example
                   — add [twitch].owner, document settings.ron behavior,
                     remove [cooldowns] and [pings] sections

crates/core/src/config.rs
                   — remove Configuration.cooldowns / Configuration.pings;
                     remove PingsConfig / CooldownsConfig structs unless
                     still referenced by non-runtime code (audit during impl)
```

## 10. Testing

| Layer | Coverage |
|---|---|
| `Settings::resolve` | sparse overrides merge over compile defaults per-field; empty overrides ≡ compile defaults; all-fields overrides ≡ overrides verbatim |
| `validate()` | each bound (cooldown range, ping cooldown range); error includes field name; multi-field errors collected |
| `SettingsStore::apply` | write → reload from disk → equal; concurrent writes serialize (two `tokio::spawn`s); corrupt RON on load → compile defaults + quarantine file written + logged error; atomic write: simulated mid-rename failure leaves original intact |
| `SettingsStore::reset` | per-section reset clears only that section's overrides |
| `AuditLog` | JSON shape stable; append-only across reopens; Berlin tz; multi-change entry shape |
| Owner middleware | non-owner session → 403; owner session → 200; absent `[twitch].owner` → 403 for all including broadcaster; missing session → existing redirect to login (not 403) |
| OR-with-owner on existing tiers | owner reads viewer pages; owner writes mod pages without being a mod |
| End-to-end (`crates/web/tests/`) | POST `/settings` with valid patch → `handle.load()` reflects new value within the same test; POST → audit file contains expected entry; reset POST removes override |
| `PingManager` | existing tests updated to construct with `SettingsHandle::test_default()`; runtime change to `pings.public` flips trigger eligibility without restart |

All tests run under `cargo nextest run` per repo convention.

## 11. Risks + mitigations

| Risk | Mitigation |
|---|---|
| v1 cutover reverts deployed `[cooldowns]` / `[pings]` overrides to compile defaults | Explicitly accepted (section 1). Single-instance deployment; operator re-enters values via dashboard at first boot. Compile-time defaults match the documented `config.toml.example` values, so any value the operator hadn't tuned away from default sees zero change. |
| Out-of-band edit to `settings.ron` clobbered by dashboard write | `apply` re-reads from disk under the write lock before merging. Document that external edits don't apply until restart. |
| Owner config typo locks owner out | A misconfigured ID just means no session matches the owner check — settings page returns 403 for everyone, no data loss. Recovery: edit `config.toml`, restart. Bin startup logs the resolved owner ID (without exposing other config) to make typos visible. |
| Corrupt `settings.ron` after crash | Atomic write+rename pattern (tmp file + fsync + rename). Load failure falls back to compile-time defaults with logged error; user re-saves from dashboard to recover. |
| `arc_swap` reader sees stale value briefly after a write | Acceptable: cooldowns and ping flags don't need linearizability across processes. Documented. |
| Audit log fills disk | v1: low volume (handful of edits per week). Add rotation when it bites. |
| Future migration breaks running deployments | `schema_version` + quarantine-on-failure path means a broken file degrades to defaults rather than crashing the bot. |
