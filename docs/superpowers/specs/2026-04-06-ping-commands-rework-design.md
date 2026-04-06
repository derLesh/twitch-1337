# Ping Commands Rework

Replace the StreamElements-backed ping toggle system with a fully local, file-based ping management system. Remove StreamElements integration entirely.

## Summary of Changes

- Remove `streamelements.rs`, `toggle_ping.rs`, `list_pings.rs`, SE config, `reqwest`/`regex` deps
- Add `PingManager` for state + persistence (`pings.ron`)
- Add `PingAdminCommand` (`!ping`) for admin and user self-service subcommands
- Add `PingTriggerCommand` for dynamic `!<name>` triggers
- Extend `Command` trait with `matches()` method
- Add `hidden_admins` to `[twitch]` config and `[pings]` config section

## Data Model

### `pings.ron`

```ron
PingStore(
    pings: {
        "dbd": Ping(
            name: "dbd",
            template: "{mentions} Dead by Daylight geht los!",
            members: ["alice", "bob", "charlie"],
            cooldown: Some(300),
            created_by: "somemod",
        ),
        "eft": Ping(
            name: "eft",
            template: "{sender} sagt EFT Zeit! {mentions}",
            members: ["alice"],
            cooldown: None,
            created_by: "streamer",
        ),
    },
)
```

- `pings`: `HashMap<String, Ping>` keyed by ping name
- `members`: Twitch login names (lowercase)
- `cooldown`: `Option<u64>` in seconds; `None` falls back to global default
- `created_by`: Twitch login of the user who created the ping

### `config.toml` additions

```toml
[twitch]
hidden_admins = ["12345678"]  # Twitch user IDs with admin privileges

[pings]
default_cooldown = 300  # seconds, fallback for pings without per-ping cooldown
```

## Architecture

### PingManager

Owns all ping state and persistence. Shared as `Arc<RwLock<PingManager>>`.

**State:**
- `pings: HashMap<String, Ping>` -- live ping data
- `last_triggered: HashMap<String, Instant>` -- cooldown tracking (in-memory only)

**Persistence:**
- Loaded once at startup; missing file = empty store
- Written after every mutation via write-to-temp + rename (`pings.ron.tmp` -> `pings.ron`)
- `RwLock` prevents concurrent read during write
- No file watching -- bot is sole owner

**Methods:**
- `load(path) -> Result<Self>`
- `save(&self) -> Result<()>`
- `create_ping(name, template, created_by, cooldown) -> Result<()>`
- `delete_ping(name) -> Result<()>`
- `add_member(ping_name, username) -> Result<()>`
- `remove_member(ping_name, username) -> Result<()>`
- `get_ping(name) -> Option<&Ping>`
- `list_pings_for_user(username) -> Vec<&str>`
- `is_member(ping_name, username) -> bool`
- `check_cooldown(ping_name, default_cooldown) -> bool`
- `record_trigger(ping_name)`

### Command Trait Change

Add `matches()` with default implementation:

```rust
fn matches(&self, word: &str) -> bool {
    self.name() == word
}
```

Dispatcher calls `matches()` instead of `name() ==`. All existing commands unchanged.

### PingAdminCommand (`!ping`)

Implements `Command` trait with `name() -> "!ping"`.

**Admin subcommands** (require broadcaster badge, mod badge, or user ID in `hidden_admins`):
- `!ping create <name> <template>` -- create a new ping
- `!ping delete <name>` -- delete a ping
- `!ping add <name> <user>` -- add a user to a ping
- `!ping remove <name> <user>` -- remove a user from a ping

**User subcommands** (anyone):
- `!ping join <name>` -- subscribe yourself
- `!ping leave <name>` -- unsubscribe yourself
- `!ping list` -- list your active pings

### PingTriggerCommand

Implements `Command` trait. Overrides `matches()` to check if the word (e.g. `!dbd`) corresponds to a registered ping name.

**Behavior:**
- Checks sender is a member of the ping
- Checks cooldown (per-ping override or global default)
- Renders template with `{mentions}` and `{sender}` placeholders
- Sends rendered message to chat
- Silent on: not a member, cooldown active, no members

### Permission Check

A user is an admin if any of:
- Message has broadcaster badge
- Message has moderator badge
- User's Twitch ID is in `config.twitch.hidden_admins`

## Commands & Responses

### Admin Commands

| Command | Success | Error |
|---|---|---|
| `!ping create dbd {mentions} DBD!` | `Ping "dbd" erstellt Okayge` | `Ping "dbd" gibt es schon FDM` |
| `!ping delete dbd` | `Ping "dbd" gelöscht Okayge` | `Ping "dbd" gibt es nicht FDM` |
| `!ping add dbd alice` | `alice zu "dbd" hinzugefügt Okayge` | `alice ist schon in "dbd" FDM` |
| `!ping remove dbd alice` | `alice aus "dbd" entfernt Okayge` | `alice ist nicht in "dbd" FDM` |
| Unauthorized user | -- | `Das darfst du nicht FDM` |

### User Commands

| Command | Success | Error |
|---|---|---|
| `!ping join dbd` | `Hab ich gemacht Okayge` | `Ping "dbd" gibt es nicht FDM` / `Bist du schon FDM` |
| `!ping leave dbd` | `Hab ich gemacht Okayge` | `Ping "dbd" gibt es nicht FDM` / `Bist du nicht drin FDM` |
| `!ping list` | `dbd eft hoi` | `Keine Pings` |

### Trigger Commands

| Scenario | Behavior |
|---|---|
| `!dbd` (member, off cooldown) | Renders template, sends to chat |
| `!dbd` (not a member) | Silent |
| `!dbd` (on cooldown) | Silent |
| `!dbd` (no members) | Silent |

## Template Placeholders

- `{mentions}` -- space-separated `@user1 @user2 @user3` list
- `{sender}` -- login name of the user who triggered the ping

## Cooldown Behavior

- Tracked per ping name in `HashMap<String, Instant>` (in-memory only)
- Effective cooldown: per-ping `cooldown` field if set, otherwise `config.pings.default_cooldown`
- Reset on bot restart (not persisted)
- Silent when on cooldown

## Removal Scope

Delete entirely:
- `src/streamelements.rs`
- `src/commands/toggle_ping.rs`
- `src/commands/list_pings.rs`

Remove from config:
- `[streamelements]` section from config types, validation, and `config.toml.example`
- `StreamelementsConfig` struct
- `SEClient` initialization in `main.rs`

Remove dependencies (if not used elsewhere):
- `reqwest`
- `regex`
