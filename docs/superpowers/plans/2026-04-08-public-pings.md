# Public Pings Toggle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a global `public` config toggle so anyone can trigger pings, not just members.

**Architecture:** Add `public: bool` to `PingsConfig`, thread it to `PingTriggerCommand`, conditionally skip the membership check. Also make `render_template()` always exclude the sender from `{mentions}`.

**Tech Stack:** Rust, serde, tokio, async-trait

**Spec:** `docs/superpowers/specs/2026-04-08-public-pings-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/ping.rs` | Modify | Exclude sender from `{mentions}` in `render_template()` |
| `src/commands/ping_trigger.rs` | Modify | Add `public` field, conditionally skip membership check |
| `src/main.rs` | Modify | Add `public` to `PingsConfig`, pass it through to `PingTriggerCommand` |
| `config.toml.example` | Modify | Document the new `public` option |
| `CLAUDE.md` | Modify | Add `public` to `PingsConfig` docs |

---

### Task 1: Self-exclusion in render_template

**Files:**
- Modify: `src/ping.rs:179-194` (render_template + tests)

- [ ] **Step 1: Write failing tests for sender self-exclusion**

Add these tests at the end of the `mod tests` block in `src/ping.rs` (before the closing `}`):

```rust
#[test]
fn render_template_excludes_sender() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = test_manager(dir.path());
    mgr.add_member("test", "alice").unwrap();
    mgr.add_member("test", "bob").unwrap();

    let result = mgr.render_template("test", "alice").unwrap();
    assert!(result.contains("@bob"), "should mention bob");
    assert!(!result.contains("@alice"), "should not mention sender alice");
}

#[test]
fn render_template_returns_none_when_sender_is_only_member() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = test_manager(dir.path());
    mgr.add_member("test", "alice").unwrap();

    let result = mgr.render_template("test", "alice");
    assert!(result.is_none(), "should return None when only member is sender");
}

#[test]
fn render_template_excludes_sender_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = test_manager(dir.path());
    mgr.add_member("test", "alice").unwrap();
    mgr.add_member("test", "bob").unwrap();

    let result = mgr.render_template("test", "Alice").unwrap();
    assert!(!result.contains("@alice"), "should exclude sender case-insensitively");
    assert!(result.contains("@bob"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ping::tests -- -v`
Expected: `render_template_excludes_sender` and `render_template_returns_none_when_sender_is_only_member` FAIL because the current implementation includes the sender in mentions.

- [ ] **Step 3: Implement sender exclusion in render_template**

Replace the `render_template` method in `src/ping.rs:179-194` with:

```rust
/// Render a ping's template with placeholders replaced.
/// Returns None if ping doesn't exist or has no mentionable members
/// (i.e., all members are the sender).
pub fn render_template(&self, ping_name: &str, sender: &str) -> Option<String> {
    let ping = self.store.pings.get(ping_name)?;
    let sender_lower = sender.to_lowercase();
    let mentions = ping.members.iter()
        .filter(|m| **m != sender_lower)
        .map(|m| format!("@{m}"))
        .collect::<Vec<_>>()
        .join(" ");
    if mentions.is_empty() {
        return None;
    }
    let rendered = ping.template
        .replace("{mentions}", &mentions)
        .replace("{sender}", sender);
    Some(rendered)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ping::tests -- -v`
Expected: All tests pass, including the three new ones.

- [ ] **Step 5: Commit**

```bash
git add src/ping.rs
git commit -m "feat: exclude sender from ping mentions (#7)"
```

---

### Task 2: Add public field to PingTriggerCommand

**Files:**
- Modify: `src/commands/ping_trigger.rs:11-22` (struct + constructor)
- Modify: `src/commands/ping_trigger.rs:52-54` (membership check)

- [ ] **Step 1: Add `public` field to struct and constructor**

In `src/commands/ping_trigger.rs`, change the struct (line 11-14) to:

```rust
pub struct PingTriggerCommand {
    ping_manager: Arc<RwLock<PingManager>>,
    default_cooldown: u64,
    public: bool,
}
```

Change the `new` method (line 17-22) to:

```rust
pub fn new(ping_manager: Arc<RwLock<PingManager>>, default_cooldown: u64, public: bool) -> Self {
    Self {
        ping_manager,
        default_cooldown,
        public,
    }
}
```

- [ ] **Step 2: Make membership check conditional on `public`**

In `src/commands/ping_trigger.rs`, replace the membership check (line 52-54):

```rust
if !manager.is_member(ping_name, sender) {
    return Ok(());
}
```

with:

```rust
if !self.public && !manager.is_member(ping_name, sender) {
    return Ok(());
}
```

- [ ] **Step 3: Verify it compiles (will fail — call site not updated yet)**

Run: `cargo check 2>&1 | head -10`
Expected: Error at `src/main.rs` where `PingTriggerCommand::new()` is called with 2 args instead of 3. This is correct — we'll fix it in Task 3.

- [ ] **Step 4: Commit**

```bash
git add src/commands/ping_trigger.rs
git commit -m "feat: add public flag to PingTriggerCommand (#7)"
```

---

### Task 3: Add public to PingsConfig and wire it through

**Files:**
- Modify: `src/main.rs:145-157` (PingsConfig)
- Modify: `src/main.rs:1080-1097` (handler spawn)
- Modify: `src/main.rs:1533-1544` (run_generic_command_handler signature)
- Modify: `src/main.rs:1599-1604` (PingTriggerCommand construction)

- [ ] **Step 1: Add `public` field to `PingsConfig`**

In `src/main.rs`, change `PingsConfig` (line 145-149) to:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
struct PingsConfig {
    #[serde(default = "default_cooldown")]
    default_cooldown: u64,
    #[serde(default)]
    public: bool,
}
```

Change `impl Default for PingsConfig` (line 151-157) to:

```rust
impl Default for PingsConfig {
    fn default() -> Self {
        Self {
            default_cooldown: default_cooldown(),
            public: false,
        }
    }
}
```

- [ ] **Step 2: Extract `public` and pass to handler**

In `src/main.rs`, in the handler spawn block (around line 1087), add after the `default_cooldown` line:

```rust
let pings_public = config.pings.public;
```

Then update the `run_generic_command_handler` call (line 1091-1095) to include it:

```rust
run_generic_command_handler(
    broadcast_tx, client, openrouter_config, leaderboard,
    ping_manager, hidden_admin_ids, default_cooldown, pings_public,
    tracker_tx, aviation_client,
).await
```

- [ ] **Step 3: Add `pings_public` parameter to `run_generic_command_handler`**

In `src/main.rs`, update the function signature (line 1534-1544). Add `pings_public: bool` after `default_cooldown`:

```rust
async fn run_generic_command_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    openrouter_config: Option<OpenRouterConfig>,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    ping_manager: Arc<tokio::sync::RwLock<ping::PingManager>>,
    hidden_admin_ids: Vec<String>,
    default_cooldown: u64,
    pings_public: bool,
    tracker_tx: tokio::sync::mpsc::Sender<flight_tracker::TrackerCommand>,
    aviation_client: aviation::AviationClient,
) {
```

Also update the `#[instrument(skip(...))]` attribute (line 1533) to include `pings_public` if you want, or leave it — it's just a bool so logging it is fine.

- [ ] **Step 4: Pass `pings_public` to PingTriggerCommand::new()**

In `src/main.rs`, update the `PingTriggerCommand` construction (line 1601-1604):

```rust
commands.push(Box::new(commands::ping_trigger::PingTriggerCommand::new(
    ping_manager,
    default_cooldown,
    pings_public,
)));
```

- [ ] **Step 5: Verify it compiles and tests pass**

Run: `cargo check && cargo test --quiet`
Expected: Compiles with no errors, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire public pings config through to trigger command (#7)"
```

---

### Task 4: Update config.toml.example and CLAUDE.md

**Files:**
- Modify: `config.toml.example:27-29`
- Modify: `CLAUDE.md` (PingsConfig section)

- [ ] **Step 1: Update config.toml.example**

In `config.toml.example`, replace the pings section (line 27-29):

```toml
# Optional: Pings configuration
# [pings]
# default_cooldown = 300  # Cooldown in seconds between triggers (default: 300)
```

with:

```toml
# Optional: Pings configuration
# [pings]
# default_cooldown = 300  # Cooldown in seconds between triggers (default: 300)
# public = false  # Allow anyone to trigger pings (default: false, members-only)
```

- [ ] **Step 2: Update CLAUDE.md**

In `CLAUDE.md`, find the `PingsConfig` section:

```
**`PingsConfig`**
- `default_cooldown` - Default cooldown between ping triggers in seconds (default: 300)
```

Replace with:

```
**`PingsConfig`**
- `default_cooldown` - Default cooldown between ping triggers in seconds (default: 300)
- `public` - Allow anyone to trigger pings, not just members (default: false)
```

- [ ] **Step 3: Commit**

```bash
git add config.toml.example CLAUDE.md
git commit -m "docs: document public pings config option (#7)"
```
