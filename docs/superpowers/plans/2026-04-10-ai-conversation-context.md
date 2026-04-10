# AI Conversation Context Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-channel message history buffer to the `!ai` command so the LLM receives recent chat context via the `{chat_history}` template placeholder.

**Architecture:** A `VecDeque<(String, String)>` (username, message) wrapped in `Arc<Mutex<...>>` is created in `run_generic_command_handler` and shared between the command dispatcher (writes every main-channel PRIVMSG) and `AiCommand` (reads history + writes bot responses). The `instruction_template` gains a `{chat_history}` placeholder resolved at request time.

**Tech Stack:** Rust, tokio (Mutex), std::collections::VecDeque

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `src/main.rs` | Modify | Add `history_length` to `AiConfig`, validation, create buffer, wire through |
| `src/commands/ai.rs` | Modify | Accept history buffer, format `{chat_history}`, record bot responses |
| `config.toml.example` | Modify | Document `history_length` option |
| `CLAUDE.md` | Modify | Document `history_length` in config reference |

---

### Task 1: Add `history_length` to `AiConfig`

**Files:**
- Modify: `src/main.rs:94-119` (AiConfig struct)
- Modify: `src/main.rs:125-127` (default_instruction_template)
- Modify: `src/main.rs:236-242` (validate)

- [ ] **Step 1: Add the field and default function**

In `src/main.rs`, add `history_length` to `AiConfig` (after `timeout`):

```rust
/// Number of recent chat messages to include as context (0 = disabled, max 100)
#[serde(default)]
history_length: u64,
```

No default function needed — `serde(default)` gives `0`.

- [ ] **Step 2: Change the default instruction template**

In `src/main.rs`, update `default_instruction_template()`:

```rust
fn default_instruction_template() -> String {
    "{chat_history}\n{message}".to_string()
}
```

- [ ] **Step 3: Add validation**

In `src/main.rs`, inside `Configuration::validate()`, after the existing AI validation block (line ~242), add:

```rust
if let Some(ref ai) = self.ai
    && ai.history_length > 100
{
    bail!("ai.history_length must be <= 100 (got {})", ai.history_length);
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles successfully

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: add history_length field to AiConfig"
```

---

### Task 2: Create and wire the history buffer

**Files:**
- Modify: `src/main.rs:1558-1651` (run_generic_command_handler)
- Modify: `src/main.rs:1655-1709` (run_command_dispatcher)

- [ ] **Step 1: Add a type alias for readability**

At the top of `src/main.rs` (near other type aliases or after the imports), add:

```rust
type ChatHistory = Arc<tokio::sync::Mutex<VecDeque<(String, String)>>>;
```

Ensure `std::collections::VecDeque` is in the imports.

- [ ] **Step 2: Create the buffer in `run_generic_command_handler`**

In `run_generic_command_handler`, after the LLM client initialization block (after line ~1611) and before the commands vec, add:

```rust
let chat_history: Option<ChatHistory> = ai_config
    .as_ref()
    .filter(|cfg| cfg.history_length > 0)
    .map(|cfg| {
        Arc::new(tokio::sync::Mutex::new(VecDeque::with_capacity(
            cfg.history_length as usize,
        )))
    });
```

Note: `ai_config` is consumed by the `if let Some(ai_cfg) = ai_config` block above. We need to read `history_length` before that consumption. Restructure: extract `history_length` before the `if let`:

```rust
let history_length = ai_config.as_ref().map_or(0, |cfg| cfg.history_length);
```

Place this line before the `let llm_client: Option<...> = if let Some(ai_cfg) = ai_config {` block. Then create the buffer after the LLM client block:

```rust
let chat_history: Option<ChatHistory> = if history_length > 0 {
    Some(Arc::new(tokio::sync::Mutex::new(
        VecDeque::with_capacity(history_length as usize),
    )))
} else {
    None
};
```

- [ ] **Step 3: Pass history buffer to `AiCommand::new()`**

Update the `AiCommand::new()` call (line ~1634) to pass the history and bot username:

```rust
if let Some((client, cfg)) = llm_client {
    commands.push(Box::new(commands::ai::AiCommand::new(
        client,
        cfg.model,
        cfg.system_prompt,
        cfg.instruction_template,
        Duration::from_secs(cfg.timeout),
        chat_history.clone(),
        bot_username.clone(),
    )));
}
```

We need `bot_username` available here. Add it as a parameter to `run_generic_command_handler`:

```rust
async fn run_generic_command_handler(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    ai_config: Option<AiConfig>,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    ping_manager: Arc<tokio::sync::RwLock<ping::PingManager>>,
    hidden_admin_ids: Vec<String>,
    default_cooldown: u64,
    pings_public: bool,
    tracker_tx: tokio::sync::mpsc::Sender<flight_tracker::TrackerCommand>,
    aviation_client: aviation::AviationClient,
    admin_channel: Option<String>,
    bot_username: String,
) {
```

And update the call site (line ~1111) to pass `config.twitch.username.clone()`:

```rust
let bot_username = config.twitch.username.clone();
async move {
    run_generic_command_handler(
        broadcast_tx, client, ai_config, leaderboard,
        ping_manager, hidden_admin_ids, default_cooldown, pings_public,
        tracker_tx, aviation_client, admin_channel, bot_username,
    ).await
}
```

- [ ] **Step 4: Pass history buffer and channel to `run_command_dispatcher`**

Update `run_command_dispatcher` signature:

```rust
async fn run_command_dispatcher(
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    client: Arc<AuthenticatedTwitchClient>,
    commands: Vec<Box<dyn commands::Command>>,
    admin_channel: Option<String>,
    chat_history: Option<ChatHistory>,
) {
```

Update the call at the end of `run_generic_command_handler`:

```rust
run_command_dispatcher(broadcast_rx, client, commands, admin_channel, chat_history).await;
```

- [ ] **Step 5: Record every main-channel PRIVMSG in the dispatcher**

In `run_command_dispatcher`, after the admin channel gate (line ~1674) and before the word splitting (line ~1676), add:

```rust
// Record message in chat history (main channel only)
if let Some(ref history) = chat_history {
    let mut buf = history.lock().await;
    if buf.len() == buf.capacity() {
        buf.pop_front();
    }
    buf.push_back((
        privmsg.sender.login.clone(),
        privmsg.message_text.clone(),
    ));
}
```

This is placed after the admin channel check, so admin channel messages are never recorded. Messages that don't match any command are still recorded (which is what we want — all chat is context).

- [ ] **Step 6: Verify it compiles**

Run: `cargo check`
Expected: will fail because `AiCommand::new()` signature doesn't match yet — that's expected, we'll fix it in Task 3.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat: create and wire chat history buffer through dispatcher"
```

---

### Task 3: Update `AiCommand` to use history

**Files:**
- Modify: `src/commands/ai.rs`

- [ ] **Step 1: Add imports and fields**

Update imports at the top of `src/commands/ai.rs`:

```rust
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;
use tracing::{debug, error, instrument};

use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::{truncate_response, ChatHistory, MAX_RESPONSE_LENGTH};
```

Add fields to `AiCommand` struct:

```rust
pub struct AiCommand {
    llm_client: Box<dyn LlmClient>,
    model: String,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    system_prompt: String,
    instruction_template: String,
    timeout: Duration,
    chat_history: Option<ChatHistory>,
    bot_username: String,
}
```

- [ ] **Step 2: Update constructor**

```rust
impl AiCommand {
    pub fn new(
        llm_client: Box<dyn LlmClient>,
        model: String,
        system_prompt: String,
        instruction_template: String,
        timeout: Duration,
        chat_history: Option<ChatHistory>,
        bot_username: String,
    ) -> Self {
        Self {
            llm_client,
            model,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
            system_prompt,
            instruction_template,
            timeout,
            chat_history,
            bot_username,
        }
    }
}
```

- [ ] **Step 3: Format history and replace template placeholders in `execute()`**

In `execute()`, replace the `user_message` construction (line 105) and request building (lines 107-119) with:

```rust
let user_message = self.instruction_template.replace("{message}", &instruction);

// Build chat history string
let chat_history_text = if let Some(ref history) = self.chat_history {
    let buf = history.lock().await;
    if buf.is_empty() {
        String::new()
    } else {
        buf.iter()
            .map(|(user, msg)| format!("{user}: {msg}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
} else {
    String::new()
};

let user_message = user_message.replace("{chat_history}", &chat_history_text);

let request = ChatCompletionRequest {
    model: self.model.clone(),
    messages: vec![
        Message {
            role: "system".to_string(),
            content: self.system_prompt.clone(),
        },
        Message {
            role: "user".to_string(),
            content: user_message,
        },
    ],
};
```

- [ ] **Step 4: Record successful bot responses**

After the successful response is sent to chat (after the `ctx.client.say_in_reply_to` call at line ~140), add recording logic. Replace the response handling block:

```rust
let response = match result {
    Ok(Ok(text)) => {
        let truncated = truncate_response(&text, MAX_RESPONSE_LENGTH);
        // Record successful response in chat history
        if let Some(ref history) = self.chat_history {
            let mut buf = history.lock().await;
            if buf.len() == buf.capacity() {
                buf.pop_front();
            }
            buf.push_back((self.bot_username.clone(), truncated.clone()));
        }
        truncated
    }
    Ok(Err(e)) => {
        error!(error = ?e, "AI execution failed");
        "Da ist was schiefgelaufen FDM".to_string()
    }
    Err(_) => {
        error!("AI execution timed out");
        "Das hat zu lange gedauert Waiting".to_string()
    }
};
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: compiles successfully

- [ ] **Step 6: Commit**

```bash
git add src/commands/ai.rs
git commit -m "feat: add chat history support to AiCommand"
```

---

### Task 4: Update config.toml.example

**Files:**
- Modify: `config.toml.example`

- [ ] **Step 1: Add history_length to both AI backend examples**

In `config.toml.example`, update the openai example block. After the `timeout` line (line 48), add:

```toml
# history_length = 10                                # Optional, number of chat messages to keep as context (0 = disabled, max 100)
```

Update the `instruction_template` comment in both examples to mention `{chat_history}`:

```toml
# instruction_template = "{chat_history}\n{message}" # Use {message} and {chat_history} as placeholders
```

Do the same for the ollama example block (after line 58):

```toml
# instruction_template = "{chat_history}\n{message}" # Use {message} and {chat_history} as placeholders
# timeout = 30                                        # Optional, AI request timeout in seconds (default: 30)
# history_length = 10                                # Optional, number of chat messages to keep as context (0 = disabled, max 100)
```

- [ ] **Step 2: Commit**

```bash
git add config.toml.example
git commit -m "docs: add history_length to config.toml.example"
```

---

### Task 5: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add history_length to the AiConfig documentation**

In `CLAUDE.md`, find the `**[ai]** (optional)` section and add after the `timeout` entry:

```
- `history_length` - Number of recent chat messages to include as context (optional, default: 0 = disabled, max: 100). All main-channel messages are buffered; admin channel messages are excluded. The history is injected via the `{chat_history}` template placeholder.
```

Update the `instruction_template` entry to mention `{chat_history}`:

```
- `instruction_template` - Optional: Template with `{message}` and `{chat_history}` placeholders (default: `"{chat_history}\n{message}"`)
```

- [ ] **Step 2: Update the AiConfig struct documentation**

In the Code Structure section, find the `**AiConfig**` block and add:

```
- `history_length` - Number of chat messages to keep as context (default: 0)
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document history_length in CLAUDE.md"
```

---

### Task 6: Verify end-to-end

- [ ] **Step 1: Run clippy**

Run: `cargo clippy`
Expected: no warnings or errors

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 3: Build release**

Run: `cargo build --release`
Expected: builds successfully
