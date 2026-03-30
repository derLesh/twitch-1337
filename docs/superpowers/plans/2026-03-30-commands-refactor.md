# Command System Refactor + !lb and !fb Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor monolithic command handling into a trait-based system with one module per command, migrate all existing commands, and add `!lb` (leaderboard) and `!fb` (feedback).

**Architecture:** A `Command` trait with `name()`, `enabled()`, and `execute()` methods. Each command is a struct in its own file under `src/commands/`. The dispatcher builds a `Vec<Box<dyn Command>>` at startup and matches incoming messages by first word. Inline modules (`streamelements`, `openrouter`, `database`, `aviation`) are extracted to standalone files.

**Tech Stack:** Rust, async-trait, tokio, twitch-irc, chrono/chrono-tz

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `src/commands/mod.rs` | Create | `Command` trait, `CommandContext`, dispatcher fn |
| `src/commands/toggle_ping.rs` | Create | `!tp` command (was `!toggle-ping`) |
| `src/commands/list_pings.rs` | Create | `!lp` command (was `!list-pings`) |
| `src/commands/ai.rs` | Create | `!ai` command |
| `src/commands/random_flight.rs` | Create | `!fl` command |
| `src/commands/flights_above.rs` | Create | `!up` command |
| `src/commands/leaderboard.rs` | Create | `!lb` command (new) |
| `src/commands/feedback.rs` | Create | `!fb` command (new) |
| `src/streamelements.rs` | Create | Extracted from `main.rs` inline module (lines 36-183) |
| `src/openrouter.rs` | Create | Extracted from `main.rs` inline module (lines 189-355) |
| `src/database.rs` | Create | Extracted from `main.rs` inline module (lines 2496-2724) |
| `src/aviation.rs` | Create | Extracted from `main.rs` inline module (lines 2726-3297) |
| `src/main.rs` | Modify | Remove inline modules, remove command functions, add shared leaderboard, update handler spawning |

---

### Task 1: Extract inline modules to standalone files

Extract the four inline modules from `main.rs` to their own files with no logic changes. This is a pure move — no code modifications.

**Files:**
- Create: `src/streamelements.rs`
- Create: `src/openrouter.rs`
- Create: `src/database.rs`
- Create: `src/aviation.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Extract `streamelements` module**

Copy the contents of `mod streamelements { ... }` (lines 37-182 of `main.rs`) to `src/streamelements.rs`. Remove the outer `mod streamelements { }` wrapper — the file itself is the module. Keep all inner imports (`use eyre::...`, etc.).

Add `#[derive(Clone)]` to `SEClient` (line 107) — it's a newtype over `reqwest::Client` which is `Clone`, but the derive is needed for command registration to clone it across multiple commands.

- [ ] **Step 2: Extract `openrouter` module**

Copy the contents of `mod openrouter { ... }` (lines 190-354 of `main.rs`) to `src/openrouter.rs`. Remove the outer `mod openrouter { }` wrapper.

- [ ] **Step 3: Extract `database` module**

Copy the contents of `mod database { ... }` (lines 2497-2723 of `main.rs`) to `src/database.rs`. Remove the outer wrapper.

- [ ] **Step 4: Extract `aviation` module**

Copy the contents of `mod aviation { ... }` (lines 2727-3297 of `main.rs`) to `src/aviation.rs`. Remove the outer wrapper.

- [ ] **Step 5: Update `main.rs` module declarations**

Replace the four inline `mod` blocks with:

```rust
mod streamelements;
mod openrouter;
mod database;
mod aviation;
```

Keep the existing `use` statements that reference these modules (lines 357-358):

```rust
use crate::openrouter::{ChatCompletionRequest, Message, OpenRouterClient};
use crate::streamelements::SEClient;
```

- [ ] **Step 6: Build and verify**

Run: `cargo check`
Expected: compiles with no errors (warnings OK)

- [ ] **Step 7: Commit**

```bash
git add src/streamelements.rs src/openrouter.rs src/database.rs src/aviation.rs src/main.rs
git commit -m "refactor: extract inline modules to standalone files"
```

---

### Task 2: Create the Command trait and CommandContext

Define the trait and context struct that all commands will implement.

**Files:**
- Create: `src/commands/mod.rs`
- Modify: `src/main.rs` (add `mod commands;`)

- [ ] **Step 1: Create `src/commands/mod.rs`**

```rust
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::message::PrivmsgMessage;

use crate::AuthenticatedTwitchClient;

pub mod toggle_ping;
pub mod list_pings;
pub mod ai;
pub mod random_flight;
pub mod flights_above;
pub mod leaderboard;
pub mod feedback;

/// Context passed to every command execution.
pub struct CommandContext<'a> {
    /// The chat message that triggered the command.
    pub privmsg: &'a PrivmsgMessage,
    /// The IRC client for sending responses.
    pub client: &'a Arc<AuthenticatedTwitchClient>,
    /// Remaining words after the command name.
    pub args: Vec<&'a str>,
}

/// Trait implemented by all bot commands.
#[async_trait]
pub trait Command: Send + Sync {
    /// The command trigger including "!" prefix (e.g., "!lb").
    fn name(&self) -> &str;

    /// Whether the command is currently enabled.
    fn enabled(&self) -> bool {
        true
    }

    /// Execute the command with the given context.
    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()>;
}
```

- [ ] **Step 2: Add module declaration to `main.rs`**

Add `mod commands;` alongside the other module declarations in `main.rs`.

- [ ] **Step 3: Create placeholder files for submodules**

Create empty files so the module declarations in `mod.rs` don't cause compile errors. Each file should contain just a comment for now:

- `src/commands/toggle_ping.rs`
- `src/commands/list_pings.rs`
- `src/commands/ai.rs`
- `src/commands/random_flight.rs`
- `src/commands/flights_above.rs`
- `src/commands/leaderboard.rs`
- `src/commands/feedback.rs`

Each placeholder:
```rust
// TODO: implement in subsequent tasks
```

- [ ] **Step 4: Build and verify**

Run: `cargo check`
Expected: compiles (warnings about unused imports/dead code OK)

- [ ] **Step 5: Commit**

```bash
git add src/commands/
git commit -m "refactor: add Command trait and module structure"
```

---

### Task 3: Migrate !toggle-ping to !tp

Move the toggle_ping_command function from `main.rs` into the new command struct.

**Files:**
- Create: `src/commands/toggle_ping.rs` (replace placeholder)
- Modify: `src/main.rs` (remove old function)

- [ ] **Step 1: Write `src/commands/toggle_ping.rs`**

```rust
use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use regex::Regex;
use tracing::{debug, error, instrument};

use crate::AuthenticatedTwitchClient;
use crate::streamelements::SEClient;
use super::{Command, CommandContext};

const PING_COMMANDS: &[&str] = &[
    "ackern",
    "amra",
    "arbeitszeitbetrug",
    "dayz",
    "dbd",
    "deadlock",
    "eft",
    "euv",
    "fetentiere",
    "front",
    "hoi",
    "kluft",
    "kreuzzug",
    "ron",
    "ttt",
    "vicky",
];

pub struct TogglePingCommand {
    se_client: SEClient,
    channel_id: String,
}

impl TogglePingCommand {
    pub fn new(se_client: SEClient, channel_id: String) -> Self {
        Self { se_client, channel_id }
    }
}

#[async_trait]
impl Command for TogglePingCommand {
    fn name(&self) -> &str {
        "!tp"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        toggle_ping(ctx.privmsg, ctx.client, &self.se_client, &self.channel_id, ctx.args.first().copied()).await
    }
}

#[instrument(skip(privmsg, client, se_client, channel_id))]
async fn toggle_ping(
    privmsg: &twitch_irc::message::PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    command_name: Option<&str>,
) -> Result<()> {
    // Copy the entire body of toggle_ping_command from main.rs (lines 2004-2104)
    // It stays identical except for the function name
    let Some(command_name) = command_name else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das kann ich nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'no command name' error message");
        }
        return Ok(());
    };

    if !PING_COMMANDS.contains(&command_name) {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das finde ich nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'command not found' error message");
        }
        return Ok(());
    }

    let commands = se_client
        .get_all_commands(channel_id)
        .await
        .wrap_err("Failed to fetch commands from StreamElements API")?;

    let Some(mut command) = commands
        .into_iter()
        .find(|command| command.command == command_name)
    else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, String::from("Das gibt es nicht FDM"))
            .await
        {
            error!(error = ?e, "Failed to send 'command not found' error message");
        }
        return Ok(());
    };

    let escaped_username = regex::escape(&privmsg.sender.login);
    let re = Regex::new(&format!("(?i)@?\\s*{}", escaped_username))
        .wrap_err("Failed to create username regex")?;

    let mut has_added_ping = false;
    let new_reply = if re.is_match(&command.reply) {
        re.replace_all(&command.reply, "").to_string()
    } else {
        has_added_ping = true;
        if let Some(at_pos) = command.reply.find('@') {
            let after_at = &command.reply[at_pos..];
            let token_end = after_at.find(' ').unwrap_or(after_at.len());
            let insert_pos = at_pos + token_end;
            let (head, tail) = command.reply.split_at(insert_pos);
            format!("{head} @{}{tail}", privmsg.sender.name)
        } else {
            format!("@{} {}", privmsg.sender.name, command.reply)
        }
    };

    command.reply = new_reply.split_whitespace().collect::<Vec<_>>().join(" ");

    debug!(
        command_name = %command_name,
        user = %privmsg.sender.login,
        new_reply = %command.reply,
        "Updating ping command"
    );

    se_client
        .update_command(channel_id, command)
        .await
        .wrap_err("Failed to update command via StreamElements API")?;

    client
        .say_in_reply_to(
            privmsg,
            format!(
                "Hab ich {} gemacht Okayge",
                match has_added_ping {
                    true => "an",
                    false => "aus",
                }
            ),
        )
        .await
        .wrap_err("Failed to send success confirmation message")?;

    Ok(())
}

/// Re-export PING_COMMANDS for use by list_pings command.
pub fn ping_commands() -> &'static [&'static str] {
    PING_COMMANDS
}
```

- [ ] **Step 2: Remove old code from `main.rs`**

Remove `toggle_ping_command` function (lines 1997-2105) and the `PING_COMMANDS` constant (lines 1950-1967) from `main.rs`.

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: compiles (the old dispatcher in main.rs will have errors referencing the removed function — that's expected, it will be replaced in Task 9)

- [ ] **Step 4: Commit**

```bash
git add src/commands/toggle_ping.rs src/main.rs
git commit -m "refactor: migrate !toggle-ping to !tp command module"
```

---

### Task 4: Migrate !list-pings to !lp

**Files:**
- Create: `src/commands/list_pings.rs` (replace placeholder)
- Modify: `src/main.rs` (remove old function)

- [ ] **Step 1: Write `src/commands/list_pings.rs`**

```rust
use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use tracing::error;

use crate::AuthenticatedTwitchClient;
use crate::streamelements::SEClient;
use super::{Command, CommandContext};
use super::toggle_ping::ping_commands;

pub struct ListPingsCommand {
    se_client: SEClient,
    channel_id: String,
}

impl ListPingsCommand {
    pub fn new(se_client: SEClient, channel_id: String) -> Self {
        Self { se_client, channel_id }
    }
}

#[async_trait]
impl Command for ListPingsCommand {
    fn name(&self) -> &str {
        "!lp"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        list_pings(ctx.privmsg, ctx.client, &self.se_client, &self.channel_id, ctx.args.first().copied()).await
    }
}

async fn list_pings(
    privmsg: &twitch_irc::message::PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    se_client: &SEClient,
    channel_id: &str,
    enabled_option: Option<&str>,
) -> Result<()> {
    let filter = enabled_option.unwrap_or("enabled");
    let ping_cmds = ping_commands();

    let commands = se_client
        .get_all_commands(channel_id)
        .await
        .wrap_err("Failed to fetch commands from StreamElements API")?;

    let response = match filter {
        "enabled" => commands
            .iter()
            .filter(|command| ping_cmds.contains(&command.command.as_str()))
            .filter(|command| {
                command
                    .reply
                    .to_lowercase()
                    .contains(&format!("@{}", privmsg.sender.login.to_lowercase()))
            })
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "disabled" => commands
            .iter()
            .filter(|command| ping_cmds.contains(&command.command.as_str()))
            .filter(|command| {
                !command
                    .reply
                    .to_lowercase()
                    .contains(&format!("@{}", privmsg.sender.login.to_lowercase()))
            })
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "all" => ping_cmds.join(" "),
        _ => "Das weiß ich nicht Sadding".to_string(),
    };

    if let Err(e) = client.say_in_reply_to(privmsg, response).await {
        error!(error = ?e, "Failed to send response message");
    }

    Ok(())
}
```

- [ ] **Step 2: Remove old `list_pings_command` from `main.rs`** (lines 2107-2156)

- [ ] **Step 3: Build and verify**

Run: `cargo check`

- [ ] **Step 4: Commit**

```bash
git add src/commands/list_pings.rs src/main.rs
git commit -m "refactor: migrate !list-pings to !lp command module"
```

---

### Task 5: Migrate !ai command

**Files:**
- Create: `src/commands/ai.rs` (replace placeholder)
- Modify: `src/main.rs` (remove old function)

- [ ] **Step 1: Write `src/commands/ai.rs`**

```rust
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{debug, error};

use crate::AuthenticatedTwitchClient;
use crate::openrouter::{ChatCompletionRequest, Message, OpenRouterClient};
use crate::{MAX_RESPONSE_LENGTH, truncate_response, execute_ai_request};
use super::{Command, CommandContext};

const AI_COMMAND_COOLDOWN: Duration = Duration::from_secs(30);

pub struct AiCommand {
    openrouter_client: Option<OpenRouterClient>,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl AiCommand {
    pub fn new(openrouter_client: Option<OpenRouterClient>) -> Self {
        Self {
            openrouter_client,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for AiCommand {
    fn name(&self) -> &str {
        "!ai"
    }

    fn enabled(&self) -> bool {
        self.openrouter_client.is_some()
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let openrouter = self.openrouter_client.as_ref().unwrap();
        let instruction: String = ctx.args.join(" ");
        ai_command(ctx.privmsg, ctx.client, openrouter, &self.cooldowns, &instruction).await
    }
}
```

Then copy the body of `ai_command` (lines 2177-2255 from main.rs) as a standalone `async fn ai_command(...)` in this file. Keep the exact same logic.

- [ ] **Step 2: Make `execute_ai_request`, `truncate_response`, and `MAX_RESPONSE_LENGTH` public in `main.rs`**

Change their visibility from private/`pub(crate)` to `pub(crate)` (if not already) so the command module can import them:
- `MAX_RESPONSE_LENGTH` (line 728) — already `pub(crate)`
- `truncate_response` (line 771) — already `pub(crate)`
- `execute_ai_request` (line 731) — make `pub(crate)`

- [ ] **Step 3: Remove old `ai_command` and `AI_COMMAND_COOLDOWN` from `main.rs`**

- [ ] **Step 4: Build and verify**

Run: `cargo check`

- [ ] **Step 5: Commit**

```bash
git add src/commands/ai.rs src/main.rs
git commit -m "refactor: migrate !ai to command module"
```

---

### Task 6: Migrate !fl command

**Files:**
- Create: `src/commands/random_flight.rs` (replace placeholder)
- Modify: `src/main.rs` (remove old function)

- [ ] **Step 1: Write `src/commands/random_flight.rs`**

```rust
use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use tracing::{error, warn};

use crate::AuthenticatedTwitchClient;
use crate::parse_flight_duration;
use super::{Command, CommandContext};

pub struct RandomFlightCommand;

impl RandomFlightCommand {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Command for RandomFlightCommand {
    fn name(&self) -> &str {
        "!fl"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        flight_command(ctx.privmsg, ctx.client, ctx.args.first().copied(), ctx.args.get(1).copied()).await
    }
}
```

Then copy the body of `flight_command` (lines 2258-2343 from main.rs) as a standalone function in this file. Keep the exact same logic.

- [ ] **Step 2: Make `parse_flight_duration` public in `main.rs`**

Change `fn parse_flight_duration` (line 688) to `pub(crate) fn parse_flight_duration`.

- [ ] **Step 3: Remove old `flight_command` from `main.rs`**

- [ ] **Step 4: Build and verify**

Run: `cargo check`

- [ ] **Step 5: Commit**

```bash
git add src/commands/random_flight.rs src/main.rs
git commit -m "refactor: migrate !fl to random_flight command module"
```

---

### Task 7: Migrate !up command

**Files:**
- Create: `src/commands/flights_above.rs` (replace placeholder)
- Modify: `src/main.rs` (remove old dispatch branch for !up)

- [ ] **Step 1: Write `src/commands/flights_above.rs`**

The `!up` command currently delegates to `aviation::up_command()`. The command struct wraps the aviation client and cooldowns.

```rust
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;

use crate::AuthenticatedTwitchClient;
use crate::aviation::AviationClient;
use super::{Command, CommandContext};

pub struct FlightsAboveCommand {
    aviation_client: AviationClient,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl FlightsAboveCommand {
    pub fn new(aviation_client: AviationClient) -> Self {
        Self {
            aviation_client,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for FlightsAboveCommand {
    fn name(&self) -> &str {
        "!up"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let input: String = ctx.args.join(" ");
        crate::aviation::up_command(ctx.privmsg, ctx.client, &self.aviation_client, &input, &self.cooldowns).await
    }
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/commands/flights_above.rs
git commit -m "refactor: migrate !up to flights_above command module"
```

---

### Task 8: Promote leaderboard to shared state

The leaderboard must become shared so both the 1337 handler and the `!lb` command can access it.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Load leaderboard in `main()` and wrap in `Arc<tokio::sync::RwLock<>>`**

After `ensure_data_dir().await?;` (line 1263), add:

```rust
// Load leaderboard and wrap in shared state for 1337 handler and !lb command
let leaderboard = Arc::new(tokio::sync::RwLock::new(load_leaderboard().await));
```

- [ ] **Step 2: Pass leaderboard to `run_1337_handler`**

Update the 1337 handler spawn (around line 1332) to pass `leaderboard.clone()`:

```rust
let handler_1337 = tokio::spawn({
    let broadcast_tx = broadcast_tx.clone();
    let client = client.clone();
    let channel = config.twitch.channel.clone();
    let latency = latency.clone();
    let leaderboard = leaderboard.clone();
    async move {
        run_1337_handler(broadcast_tx, client, channel, latency, leaderboard).await;
    }
});
```

- [ ] **Step 3: Update `run_1337_handler` signature and body**

Change the function signature to accept the shared leaderboard:

```rust
async fn run_1337_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    channel: String,
    latency: Arc<AtomicU32>,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
) {
```

Inside the function:
- Remove `let mut leaderboard = load_leaderboard().await;` (line 1568)
- Replace all `leaderboard` accesses with lock guards:
  - For the record check and update block (lines 1619-1662), acquire a write lock:
    ```rust
    let mut leaderboard = leaderboard.write().await;
    ```
  - For `save_leaderboard`, pass `&*leaderboard` (the inner HashMap)
- The write lock is held for the duration of the record check + update + save, then dropped

- [ ] **Step 4: Build and verify**

Run: `cargo check`

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "refactor: promote leaderboard to shared Arc<RwLock<>> state"
```

---

### Task 9: Implement !lb (leaderboard) command

**Files:**
- Create: `src/commands/leaderboard.rs` (replace placeholder)

- [ ] **Step 1: Write `src/commands/leaderboard.rs`**

```rust
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use tracing::error;

use crate::AuthenticatedTwitchClient;
use crate::PersonalBest;
use super::{Command, CommandContext};

pub struct LeaderboardCommand {
    leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,
}

impl LeaderboardCommand {
    pub fn new(leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>) -> Self {
        Self { leaderboard }
    }
}

#[async_trait]
impl Command for LeaderboardCommand {
    fn name(&self) -> &str {
        "!lb"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let leaderboard = self.leaderboard.read().await;

        let response = if let Some((username, pb)) = leaderboard
            .iter()
            .min_by_key(|(_, pb)| pb.ms)
        {
            let date = pb.date.format("%d.%m.%Y");
            format!(
                "Der schnellste 1337 ist {username} mit {}ms am {date}",
                pb.ms
            )
        } else {
            "Noch keine Einträge vorhanden".to_string()
        };

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send leaderboard response");
        }

        Ok(())
    }
}
```

- [ ] **Step 2: Make `PersonalBest` public in `main.rs`**

Change `struct PersonalBest` (line 858) to `pub(crate) struct PersonalBest` and its fields to `pub(crate)`.

- [ ] **Step 3: Build and verify**

Run: `cargo check`

- [ ] **Step 4: Commit**

```bash
git add src/commands/leaderboard.rs src/main.rs
git commit -m "feat: add !lb command showing all-time fastest 1337"
```

---

### Task 10: Implement !fb (feedback) command

**Files:**
- Create: `src/commands/feedback.rs` (replace placeholder)

- [ ] **Step 1: Write `src/commands/feedback.rs`**

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{debug, error, info};

use crate::AuthenticatedTwitchClient;
use super::{Command, CommandContext};

const FEEDBACK_COOLDOWN: Duration = Duration::from_secs(300);
const FEEDBACK_FILENAME: &str = "feedback.txt";

pub struct FeedbackCommand {
    data_dir: PathBuf,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl FeedbackCommand {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for FeedbackCommand {
    fn name(&self) -> &str {
        "!fb"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        let message: String = ctx.args.join(" ");

        // Check for empty message
        if message.trim().is_empty() {
            if let Err(e) = ctx.client
                .say_in_reply_to(ctx.privmsg, "Benutzung: !fb <nachricht>".to_string())
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        // Check cooldown
        {
            let cooldowns_guard = self.cooldowns.lock().await;
            if let Some(last_use) = cooldowns_guard.get(user) {
                let elapsed = last_use.elapsed();
                if elapsed < FEEDBACK_COOLDOWN {
                    let remaining = (FEEDBACK_COOLDOWN - elapsed).as_secs();
                    if let Err(e) = ctx.client
                        .say_in_reply_to(
                            ctx.privmsg,
                            format!("Bitte warte noch {remaining}s Waiting"),
                        )
                        .await
                    {
                        error!(error = ?e, "Failed to send cooldown message");
                    }
                    return Ok(());
                }
            }
        }

        // Update cooldown
        {
            let mut cooldowns_guard = self.cooldowns.lock().await;
            cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
        }

        // Write feedback to file
        let now = chrono::Utc::now()
            .with_timezone(&chrono_tz::Europe::Berlin)
            .format("%Y-%m-%dT%H:%M:%S");
        let line = format!("{now} {user}: {message}\n");

        let path = self.data_dir.join(FEEDBACK_FILENAME);
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(line.as_bytes()).await {
                    error!(error = ?e, "Failed to write feedback to file");
                    if let Err(e) = ctx.client
                        .say_in_reply_to(ctx.privmsg, "Da ist was schiefgelaufen FDM".to_string())
                        .await
                    {
                        error!(error = ?e, "Failed to send error message");
                    }
                    return Ok(());
                }
            }
            Err(e) => {
                error!(error = ?e, "Failed to open feedback file");
                if let Err(e) = ctx.client
                    .say_in_reply_to(ctx.privmsg, "Da ist was schiefgelaufen FDM".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send error message");
                }
                return Ok(());
            }
        }

        info!(user = %user, "Feedback saved");

        if let Err(e) = ctx.client
            .say_in_reply_to(ctx.privmsg, "Feedback gespeichert Okayge".to_string())
            .await
        {
            error!(error = ?e, "Failed to send confirmation message");
        }

        Ok(())
    }
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`

- [ ] **Step 3: Commit**

```bash
git add src/commands/feedback.rs
git commit -m "feat: add !fb command for user feedback"
```

---

### Task 11: Replace the dispatcher in main.rs

Replace `handle_generic_commands`, `run_generic_command_handler_inner`, and `run_generic_command_handler` with the new trait-based dispatcher.

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Rewrite `run_generic_command_handler`**

Replace the entire function (lines 1777-1948) with:

```rust
async fn run_generic_command_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    se_config: StreamelementsConfig,
    openrouter_config: Option<OpenRouterConfig>,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
) {
    info!("Generic Command Handler started");

    let broadcast_rx = broadcast_tx.subscribe();

    // Initialize StreamElements client
    let se_client = match SEClient::new(se_config.api_token.expose_secret()) {
        Ok(client) => client,
        Err(e) => {
            error!(error = ?e, "Failed to initialize StreamElements client");
            return;
        }
    };

    // Initialize OpenRouter client (optional)
    let openrouter_client = if let Some(ref openrouter_cfg) = openrouter_config {
        match OpenRouterClient::new(openrouter_cfg.api_key.expose_secret(), &openrouter_cfg.model) {
            Ok(client) => {
                info!(model = %openrouter_cfg.model, "OpenRouter AI command enabled");
                Some(client)
            }
            Err(e) => {
                error!(error = ?e, "Failed to initialize OpenRouter client, AI command disabled");
                None
            }
        }
    } else {
        debug!("OpenRouter not configured, AI command disabled");
        None
    };

    // Initialize aviation client
    let aviation_client = match aviation::AviationClient::new() {
        Ok(client) => {
            info!("Aviation client initialized");
            Some(client)
        }
        Err(e) => {
            error!(error = ?e, "Failed to initialize aviation client, !up disabled");
            None
        }
    };

    let data_dir = get_data_dir();

    // Register all commands
    let commands: Vec<Box<dyn commands::Command>> = vec![
        Box::new(commands::toggle_ping::TogglePingCommand::new(
            se_client.clone(), se_config.channel_id.clone(),
        )),
        Box::new(commands::list_pings::ListPingsCommand::new(
            se_client, se_config.channel_id,
        )),
        Box::new(commands::ai::AiCommand::new(openrouter_client)),
        Box::new(commands::random_flight::RandomFlightCommand::new()),
        Box::new(commands::flights_above::FlightsAboveCommand::new(aviation_client)),
        Box::new(commands::leaderboard::LeaderboardCommand::new(leaderboard)),
        Box::new(commands::feedback::FeedbackCommand::new(data_dir)),
    ];

    run_command_dispatcher(broadcast_rx, client, commands).await;
}

async fn run_command_dispatcher(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    commands: Vec<Box<dyn commands::Command>>,
) {
    loop {
        match broadcast_rx.recv().await {
            Ok(message) => {
                let ServerMessage::Privmsg(privmsg) = message else {
                    continue;
                };

                let mut words = privmsg.message_text.split_whitespace();
                let Some(first_word) = words.next() else {
                    continue;
                };

                let Some(cmd) = commands.iter().find(|c| c.enabled() && c.name() == first_word) else {
                    continue;
                };

                let ctx = commands::CommandContext {
                    privmsg: &privmsg,
                    client: &client,
                    args: words.collect(),
                };

                if let Err(e) = cmd.execute(ctx).await {
                    error!(
                        error = ?e,
                        user = %privmsg.sender.login,
                        command = %first_word,
                        "Error handling command"
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                error!(skipped, "Command handler lagged, skipped messages");
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Broadcast channel closed, command handler exiting");
                break;
            }
        }
    }
}
```

- [ ] **Step 2: Update handler spawn in `main()` to pass leaderboard**

Update the `handler_generic_commands` spawn (around line 1342):

```rust
let handler_generic_commands = tokio::spawn({
    let broadcast_tx = broadcast_tx.clone();
    let client = client.clone();
    let se_config = config.streamelements.clone();
    let openrouter_config = config.openrouter.clone();
    let leaderboard = leaderboard.clone();
    async move {
        run_generic_command_handler(broadcast_tx, client, se_config, openrouter_config, leaderboard).await
    }
});
```

- [ ] **Step 3: Remove all old command functions from `main.rs`**

Remove these functions and constants that have been migrated:
- `handle_generic_commands` (the old dispatcher)
- `run_generic_command_handler_inner`
- `PING_COMMANDS` (if not removed in Task 3)
- `toggle_ping_command` (if not removed in Task 3)
- `list_pings_command` (if not removed in Task 4)
- `ai_command` and `AI_COMMAND_COOLDOWN` (if not removed in Task 5)
- `flight_command` (if not removed in Task 6)

- [ ] **Step 4: Handle `FlightsAboveCommand` with optional `AviationClient`**

Since `AviationClient::new()` can fail, update `FlightsAboveCommand` to accept `Option<AviationClient>` and implement `enabled()`:

In `src/commands/flights_above.rs`, change:
```rust
pub struct FlightsAboveCommand {
    aviation_client: Option<AviationClient>,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl FlightsAboveCommand {
    pub fn new(aviation_client: Option<AviationClient>) -> Self {
        Self {
            aviation_client,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for FlightsAboveCommand {
    fn name(&self) -> &str {
        "!up"
    }

    fn enabled(&self) -> bool {
        self.aviation_client.is_some()
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let client = self.aviation_client.as_ref().unwrap();
        let input: String = ctx.args.join(" ");
        crate::aviation::up_command(ctx.privmsg, ctx.client, client, &input, &self.cooldowns).await
    }
}
```

- [ ] **Step 5: Build and verify**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 6: Run full build**

Run: `cargo build`
Expected: builds successfully

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/commands/flights_above.rs
git commit -m "refactor: replace command dispatcher with trait-based system"
```

---

### Task 12: Final verification and cleanup

**Files:**
- Modify: `src/main.rs` (cleanup any dead code)

- [ ] **Step 1: Run clippy**

Run: `cargo clippy`
Expected: no errors, fix any warnings related to the refactor

- [ ] **Step 2: Fix any clippy warnings**

Address warnings one by one. Common ones to expect:
- Unused imports from removed code
- Unnecessary `clone()` calls
- Functions that could take `&str` instead of `String`

- [ ] **Step 3: Verify final build**

Run: `cargo build`
Expected: clean build

- [ ] **Step 4: Commit cleanup**

```bash
git add -A
git commit -m "refactor: cleanup dead code and fix clippy warnings"
```
