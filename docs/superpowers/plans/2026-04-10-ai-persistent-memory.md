# AI Persistent Memory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add LLM-managed persistent memory to the `!ai` command so the AI can autonomously remember, update, and forget facts about users and the channel across conversations and restarts.

**Architecture:** Post-response memory extraction — the AI responds to the user immediately (no added latency), then a fire-and-forget background task asks the LLM to extract memorable facts via tool calling. Memories are stored in `ai_memory.ron` and injected into every system prompt.

**Tech Stack:** Rust, tokio, serde/ron, reqwest, async-trait, serde_json

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/memory.rs` | Create | `Memory`, `MemoryStore`, load/save, tool execution |
| `src/llm/mod.rs` | Modify | Add tool-calling types and `chat_completion_with_tools` trait method |
| `src/llm/openai.rs` | Modify | Implement `chat_completion_with_tools` for OpenAI API |
| `src/llm/ollama.rs` | Modify | Implement `chat_completion_with_tools` for Ollama API |
| `src/commands/ai.rs` | Modify | Inject memories into system prompt, spawn extraction task |
| `src/main.rs` | Modify | Add `memory_enabled`/`max_memories` to AiConfig, load MemoryStore, wire through |
| `config.toml.example` | Modify | Document new config fields |

---

### Task 1: Memory Store (`src/memory.rs`)

**Files:**
- Create: `src/memory.rs`

- [ ] **Step 1: Create the memory data types and store**

Create `src/memory.rs`:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

const MEMORY_FILENAME: &str = "ai_memory.ron";

/// A single remembered fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub fact: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Persistent store of AI memories, serialized to RON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStore {
    pub memories: HashMap<String, Memory>,
}

impl MemoryStore {
    /// Load from disk. Returns empty store if file doesn't exist.
    pub fn load(data_dir: &Path) -> Result<(Self, PathBuf)> {
        let path = data_dir.join(MEMORY_FILENAME);
        let store = if path.exists() {
            let data =
                std::fs::read_to_string(&path).wrap_err("Failed to read ai_memory.ron")?;
            ron::from_str(&data).wrap_err("Failed to parse ai_memory.ron")?
        } else {
            info!("No ai_memory.ron found, starting with empty memory store");
            Self {
                memories: HashMap::new(),
            }
        };

        info!(count = store.memories.len(), "Loaded AI memories");
        Ok((store, path))
    }

    /// Write current state to disk using write+rename for atomicity.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp_path = path.with_extension("ron.tmp");
        let data = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .wrap_err("Failed to serialize AI memories")?;
        std::fs::write(&tmp_path, &data).wrap_err("Failed to write ai_memory.ron.tmp")?;
        std::fs::rename(&tmp_path, path)
            .wrap_err("Failed to rename ai_memory.ron.tmp to ai_memory.ron")?;
        debug!("Saved AI memories to disk");
        Ok(())
    }

    /// Format memories for injection into the system prompt.
    /// Returns None if there are no memories.
    pub fn format_for_prompt(&self) -> Option<String> {
        if self.memories.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = self
            .memories
            .iter()
            .map(|(key, mem)| format!("- {}: {}", key, mem.fact))
            .collect();
        lines.sort(); // Deterministic ordering
        Some(format!("\n\n## Known facts\n{}", lines.join("\n")))
    }

    /// Format memories for the extraction prompt (key: fact list).
    pub fn format_for_extraction(&self) -> String {
        if self.memories.is_empty() {
            return "(none)".to_string();
        }
        let mut lines: Vec<String> = self
            .memories
            .iter()
            .map(|(key, mem)| format!("- {}: {}", key, mem.fact))
            .collect();
        lines.sort();
        lines.join("\n")
    }
}
```

- [ ] **Step 2: Add the module declaration**

In `src/main.rs`, add after the existing `mod ping;` line:

```rust
mod memory;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors (unused warnings are fine)

- [ ] **Step 4: Commit**

```bash
git add src/memory.rs src/main.rs
git commit -m "feat: add MemoryStore with RON persistence"
```

---

### Task 2: Tool-calling types in `src/llm/mod.rs`

**Files:**
- Modify: `src/llm/mod.rs`

- [ ] **Step 1: Add tool-calling types and extend the trait**

Replace the entire contents of `src/llm/mod.rs` with:

```rust
pub mod ollama;
pub mod openai;

use async_trait::async_trait;
use eyre::Result;
use serde::{Deserialize, Serialize};

/// A message in a chat completion conversation.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// A tool result message returned after executing a tool call.
#[derive(Debug, Clone)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub content: String,
}

/// Request for a chat completion.
#[derive(Debug, Clone)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
}

/// Request for a chat completion with tool support.
#[derive(Debug, Clone)]
pub struct ToolChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_results: Vec<ToolResultMessage>,
}

/// Definition of a tool the LLM can call.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A single tool call returned by the LLM.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Response from a tool-calling chat completion.
#[derive(Debug, Clone)]
pub enum ToolChatCompletionResponse {
    Message(String),
    ToolCalls(Vec<ToolCall>),
}

/// Trait for LLM backends. Implementations handle serialization
/// and response parsing internally.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a chat completion request and return the response text.
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String>;

    /// Send a chat completion request with tool definitions.
    /// Returns either a text message or a list of tool calls.
    async fn chat_completion_with_tools(
        &self,
        request: ToolChatCompletionRequest,
    ) -> Result<ToolChatCompletionResponse>;
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: Compile errors in `openai.rs` and `ollama.rs` because they don't implement the new trait method yet. That's expected — we fix them in the next tasks.

- [ ] **Step 3: Commit**

```bash
git add src/llm/mod.rs
git commit -m "feat: add tool-calling types and trait method to LlmClient"
```

---

### Task 3: OpenAI tool-calling implementation

**Files:**
- Modify: `src/llm/openai.rs`

- [ ] **Step 1: Add tool-calling serde types**

In `src/llm/openai.rs`, add these types after the existing `ApiResponse` struct (after line 37):

```rust
// --- Tool-calling serde types ---

#[derive(Debug, Serialize)]
struct ApiTool {
    r#type: String,
    function: ApiFunction,
}

#[derive(Debug, Serialize)]
struct ApiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ApiToolRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    tools: Vec<ApiTool>,
}

#[derive(Debug, Deserialize)]
struct ApiToolCall {
    id: String,
    function: ApiToolCallFunction,
}

#[derive(Debug, Deserialize)]
struct ApiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ApiToolChoice {
    message: ApiToolResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ApiToolResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ApiToolResponse {
    choices: Vec<ApiToolChoice>,
}
```

- [ ] **Step 2: Implement `chat_completion_with_tools`**

Add the method inside the existing `impl LlmClient for OpenAiClient` block, after the `chat_completion` method (after line 142):

```rust
    #[instrument(skip(self, request))]
    async fn chat_completion_with_tools(
        &self,
        request: ToolChatCompletionRequest,
    ) -> Result<ToolChatCompletionResponse> {
        let url = format!("{}/chat/completions", self.base_url);

        // Build messages array as JSON values to support mixed message types
        let mut messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            })
            .collect();

        // Append tool result messages
        for tr in &request.tool_results {
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tr.tool_call_id,
                "content": tr.content,
            }));
        }

        let tools: Vec<ApiTool> = request
            .tools
            .iter()
            .map(|t| ApiTool {
                r#type: "function".to_string(),
                function: ApiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();

        let api_request = ApiToolRequest {
            model: request.model,
            messages,
            tools,
        };

        debug!(model = %self.model, "Sending tool request to OpenAI-compatible API");

        let response = self
            .http
            .post(&url)
            .json(&api_request)
            .send()
            .await
            .wrap_err("Failed to send tool request to OpenAI-compatible API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "OpenAI-compatible API error (status {}): {}",
                status,
                error_body
            ));
        }

        let api_response: ApiToolResponse = response
            .json()
            .await
            .wrap_err("Failed to parse OpenAI-compatible API tool response")?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| eyre::eyre!("No choices in API tool response"))?;

        if let Some(tool_calls) = choice.message.tool_calls {
            if !tool_calls.is_empty() {
                let calls = tool_calls
                    .into_iter()
                    .map(|tc| {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                        super::ToolCall {
                            id: tc.id,
                            name: tc.function.name,
                            arguments,
                        }
                    })
                    .collect();
                return Ok(ToolChatCompletionResponse::ToolCalls(calls));
            }
        }

        let content = choice
            .message
            .content
            .unwrap_or_default();
        Ok(ToolChatCompletionResponse::Message(content))
    }
```

- [ ] **Step 3: Update imports**

At the top of `src/llm/openai.rs`, update the import from `super`:

```rust
use super::{ChatCompletionRequest, LlmClient, ToolChatCompletionRequest, ToolChatCompletionResponse};
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: Still errors from `ollama.rs` (not updated yet), but no errors from `openai.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/llm/openai.rs
git commit -m "feat: implement tool-calling for OpenAI client"
```

---

### Task 4: Ollama tool-calling implementation

**Files:**
- Modify: `src/llm/ollama.rs`

- [ ] **Step 1: Add tool-calling serde types**

In `src/llm/ollama.rs`, add these types after the existing `ApiResponse` struct (after line 32):

```rust
// --- Tool-calling serde types ---

#[derive(Debug, Serialize)]
struct ApiTool {
    r#type: String,
    function: ApiFunction,
}

#[derive(Debug, Serialize)]
struct ApiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ApiToolRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    tools: Vec<ApiTool>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ApiToolCall {
    function: ApiToolCallFunction,
}

#[derive(Debug, Deserialize)]
struct ApiToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ApiToolResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ApiToolResponse {
    message: ApiToolResponseMessage,
}
```

- [ ] **Step 2: Implement `chat_completion_with_tools`**

Add the method inside the existing `impl LlmClient for OllamaClient` block, after the `chat_completion` method (after line 110):

```rust
    #[instrument(skip(self, request))]
    async fn chat_completion_with_tools(
        &self,
        request: ToolChatCompletionRequest,
    ) -> Result<ToolChatCompletionResponse> {
        let url = format!("{}/api/chat", self.base_url);

        // Build messages array as JSON values to support mixed message types
        let mut messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            })
            .collect();

        // Append tool result messages
        for tr in &request.tool_results {
            messages.push(serde_json::json!({
                "role": "tool",
                "content": tr.content,
            }));
        }

        let tools: Vec<ApiTool> = request
            .tools
            .iter()
            .map(|t| ApiTool {
                r#type: "function".to_string(),
                function: ApiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();

        let api_request = ApiToolRequest {
            model: request.model,
            messages,
            tools,
            stream: false,
        };

        debug!(model = %self.model, "Sending tool request to Ollama API");

        let response = self
            .http
            .post(&url)
            .json(&api_request)
            .send()
            .await
            .wrap_err("Failed to send tool request to Ollama API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "Ollama API error (status {}): {}",
                status,
                error_body
            ));
        }

        let api_response: ApiToolResponse = response
            .json()
            .await
            .wrap_err("Failed to parse Ollama API tool response")?;

        if let Some(tool_calls) = api_response.message.tool_calls {
            if !tool_calls.is_empty() {
                let calls = tool_calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, tc)| super::ToolCall {
                        id: format!("call_{}", i),
                        name: tc.function.name,
                        arguments: tc.function.arguments,
                    })
                    .collect();
                return Ok(ToolChatCompletionResponse::ToolCalls(calls));
            }
        }

        let content = api_response
            .message
            .content
            .unwrap_or_default();
        Ok(ToolChatCompletionResponse::Message(content))
    }
```

Note: Ollama doesn't return tool call IDs, so we generate synthetic ones (`call_0`, `call_1`, ...).

- [ ] **Step 3: Update imports**

At the top of `src/llm/ollama.rs`, update the import from `super`:

```rust
use super::{ChatCompletionRequest, LlmClient, ToolChatCompletionRequest, ToolChatCompletionResponse};
```

- [ ] **Step 4: Verify the full project compiles**

Run: `cargo check`
Expected: Clean compile, no errors.

- [ ] **Step 5: Commit**

```bash
git add src/llm/ollama.rs
git commit -m "feat: implement tool-calling for Ollama client"
```

---

### Task 5: Memory extraction logic in `src/memory.rs`

**Files:**
- Modify: `src/memory.rs`

- [ ] **Step 1: Add tool definitions and extraction prompt**

Add these constants and functions to `src/memory.rs` (the new imports will be consolidated in Step 4):

```rust
const MAX_EXTRACTION_ROUNDS: usize = 3;

const EXTRACTION_SYSTEM_PROMPT: &str = "\
You just had a conversation in a Twitch chat. Based on the exchange below, \
decide if any facts are worth remembering long-term about users, the channel, \
or the community. You can save new facts, overwrite outdated ones, or delete \
incorrect ones. Only save things that would be useful across future \
conversations. Do not save trivial or ephemeral things like greetings or \
simple questions.";

pub fn memory_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "save_memory".to_string(),
            description: "Save or update a fact. Use a short descriptive slug as the key (e.g. 'chrono-favorite-game'). If the key already exists, the fact is overwritten.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Short slug identifier for this memory"
                    },
                    "fact": {
                        "type": "string",
                        "description": "The fact to remember"
                    }
                },
                "required": ["key", "fact"]
            }),
        },
        ToolDefinition {
            name: "delete_memory".to_string(),
            description: "Delete a stored memory by its key.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key of the memory to delete"
                    }
                },
                "required": ["key"]
            }),
        },
    ]
}
```

- [ ] **Step 2: Add tool call execution**

Add this method to `MemoryStore`:

```rust
impl MemoryStore {
    // ... existing methods ...

    /// Execute a single tool call against the store. Returns a result message.
    pub fn execute_tool_call(&mut self, call: &ToolCall, max_memories: usize) -> String {
        match call.name.as_str() {
            "save_memory" => {
                let key = call.arguments.get("key").and_then(|v| v.as_str());
                let fact = call.arguments.get("fact").and_then(|v| v.as_str());
                match (key, fact) {
                    (Some(key), Some(fact)) => {
                        let now = chrono::Utc::now().to_rfc3339();
                        if self.memories.contains_key(key) {
                            // Update existing
                            let mem = self.memories.get_mut(key).unwrap();
                            mem.fact = fact.to_string();
                            mem.updated_at = now;
                            format!("Updated memory '{}'", key)
                        } else if self.memories.len() >= max_memories {
                            format!(
                                "Memory full ({}/{}) — delete a memory first",
                                self.memories.len(),
                                max_memories
                            )
                        } else {
                            // Create new
                            self.memories.insert(
                                key.to_string(),
                                Memory {
                                    fact: fact.to_string(),
                                    created_at: now.clone(),
                                    updated_at: now,
                                },
                            );
                            format!("Saved memory '{}'", key)
                        }
                    }
                    _ => "Error: save_memory requires 'key' and 'fact' parameters".to_string(),
                }
            }
            "delete_memory" => {
                let key = call.arguments.get("key").and_then(|v| v.as_str());
                match key {
                    Some(key) => {
                        if self.memories.remove(key).is_some() {
                            format!("Deleted memory '{}'", key)
                        } else {
                            format!("No memory with key '{}'", key)
                        }
                    }
                    None => "Error: delete_memory requires 'key' parameter".to_string(),
                }
            }
            other => format!("Unknown tool: {}", other),
        }
    }
}
```

- [ ] **Step 3: Add the extraction task function**

Add this free function at the bottom of `src/memory.rs`:

```rust
/// Spawn a fire-and-forget task that asks the LLM to extract memories.
/// Errors are logged and swallowed — never affects the user-facing response.
pub fn spawn_memory_extraction(
    llm_client: Arc<dyn llm::LlmClient>,
    model: String,
    store: Arc<RwLock<MemoryStore>>,
    store_path: PathBuf,
    max_memories: usize,
    username: String,
    user_message: String,
    ai_response: String,
    timeout: std::time::Duration,
) {
    tokio::spawn(async move {
        if let Err(e) = run_memory_extraction(
            &*llm_client,
            &model,
            &store,
            &store_path,
            max_memories,
            &username,
            &user_message,
            &ai_response,
            timeout,
        )
        .await
        {
            debug!("Memory extraction failed (non-critical): {:#}", e);
        }
    });
}

async fn run_memory_extraction(
    llm_client: &dyn llm::LlmClient,
    model: &str,
    store: &RwLock<MemoryStore>,
    store_path: &Path,
    max_memories: usize,
    username: &str,
    user_message: &str,
    ai_response: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    let current_memories = {
        let store_guard = store.read().await;
        store_guard.format_for_extraction()
    };

    // Note: chat history integration is deferred until #12 (conversation context) lands.
    // When #12 is merged, pass chat history here and include it in the prompt.
    let user_content = format!(
        "Current memories:\n{}\n\nConversation:\nUser ({}): {}\nAssistant: {}",
        current_memories, username, user_message, ai_response
    );

    let tools = memory_tool_definitions();
    let mut messages = vec![
        Message {
            role: "system".to_string(),
            content: EXTRACTION_SYSTEM_PROMPT.to_string(),
        },
        Message {
            role: "user".to_string(),
            content: user_content,
        },
    ];
    let mut tool_results: Vec<ToolResultMessage> = Vec::new();

    for round in 0..MAX_EXTRACTION_ROUNDS {
        let request = ToolChatCompletionRequest {
            model: model.to_string(),
            messages: messages.clone(),
            tools: tools.clone(),
            tool_results: tool_results.clone(),
        };

        let response =
            tokio::time::timeout(timeout, llm_client.chat_completion_with_tools(request))
                .await
                .wrap_err("Memory extraction timed out")?
                .wrap_err("Memory extraction LLM call failed")?;

        match response {
            ToolChatCompletionResponse::Message(_) => {
                debug!(round, "Memory extraction finished (text response)");
                break;
            }
            ToolChatCompletionResponse::ToolCalls(calls) => {
                debug!(round, count = calls.len(), "Memory extraction: processing tool calls");
                let mut store_guard = store.write().await;
                tool_results.clear();
                for call in &calls {
                    let result = store_guard.execute_tool_call(call, max_memories);
                    info!(tool = %call.name, key = %call.arguments.get("key").and_then(|v| v.as_str()).unwrap_or("?"), result = %result, "Memory tool executed");
                    tool_results.push(ToolResultMessage {
                        tool_call_id: call.id.clone(),
                        content: result,
                    });
                }
                // Persist after each round of tool calls
                store_guard.save(store_path)?;
                // Add assistant tool_calls message to conversation for next round
                // (the LLM client handles this via tool_results in the request)
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Consolidate imports at the top of memory.rs**

The imports added in Steps 1-3 should be at the top of the file. Make sure the final import block is:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::llm::{
    self, Message, ToolCall, ToolChatCompletionRequest, ToolChatCompletionResponse,
    ToolDefinition, ToolResultMessage,
};
```

Remove any duplicate imports that ended up inline from earlier steps.

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: Clean compile. The `chrono` crate is already a dependency.

- [ ] **Step 6: Commit**

```bash
git add src/memory.rs
git commit -m "feat: add memory extraction logic with tool execution loop"
```

---

### Task 6: Configuration changes

**Files:**
- Modify: `src/main.rs`
- Modify: `config.toml.example`

- [ ] **Step 1: Add config fields to AiConfig**

In `src/main.rs`, add two new fields to the `AiConfig` struct (after the `timeout` field at line 119):

```rust
    /// Enable persistent AI memory (default: false)
    #[serde(default)]
    memory_enabled: bool,
    /// Maximum number of stored memories (default: 50)
    #[serde(default = "default_max_memories")]
    max_memories: usize,
```

Add the default function near the other AI defaults (after `default_ai_timeout`):

```rust
fn default_max_memories() -> usize {
    50
}
```

- [ ] **Step 2: Add validation**

In the `validate()` method of `Configuration`, after the existing AI validation block (after line 277), add:

```rust
        if let Some(ref ai) = self.ai
            && ai.memory_enabled
            && !(1..=200).contains(&ai.max_memories)
        {
            bail!(
                "ai.max_memories must be between 1 and 200 (got {})",
                ai.max_memories
            );
        }
```

- [ ] **Step 3: Update config.toml.example**

Add the new fields to both the OpenAI and Ollama examples in `config.toml.example`, after the existing `timeout` and `history_length` lines:

```toml
# memory_enabled = false    # Enable persistent AI memory (default: false)
# max_memories = 50         # Maximum stored facts (default: 50, max: 200)
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: Clean compile.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs config.toml.example
git commit -m "feat: add memory_enabled and max_memories config fields"
```

---

### Task 7: Wire memory into AiCommand

**Files:**
- Modify: `src/commands/ai.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add memory fields to AiCommand**

In `src/commands/ai.rs`, add these fields to the `AiCommand` struct (after line 23, after `timeout`):

```rust
    memory_store: Option<Arc<RwLock<memory::MemoryStore>>>,
    memory_store_path: Option<PathBuf>,
    max_memories: usize,
    llm_client_shared: Option<Arc<dyn LlmClient>>,
```

Update the imports at the top of `src/commands/ai.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, instrument};

use crate::cooldown::format_cooldown_remaining;
use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::memory;
use crate::{truncate_response, MAX_RESPONSE_LENGTH};

use super::{Command, CommandContext};
```

- [ ] **Step 2: Update struct and constructor**

The LLM client needs to be shareable with the background extraction task, so change `Box<dyn LlmClient>` to `Arc<dyn LlmClient>`. Replace the entire struct and constructor:

```rust
pub struct AiCommand {
    llm_client: Arc<dyn LlmClient>,
    model: String,
    cooldown: Duration,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    system_prompt: String,
    instruction_template: String,
    timeout: Duration,
    memory_store: Option<Arc<RwLock<memory::MemoryStore>>>,
    memory_store_path: Option<PathBuf>,
    max_memories: usize,
}

impl AiCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        model: String,
        system_prompt: String,
        instruction_template: String,
        timeout: Duration,
        cooldown: Duration,
        memory_store: Option<Arc<RwLock<memory::MemoryStore>>>,
        memory_store_path: Option<PathBuf>,
        max_memories: usize,
    ) -> Self {
        Self {
            llm_client,
            model,
            cooldown,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
            system_prompt,
            instruction_template,
            timeout,
            memory_store,
            memory_store_path,
            max_memories,
        }
    }
}
```

- [ ] **Step 3: Update execute() to inject memories and spawn extraction**

Replace the `execute` method body. The key changes are:
1. Build system prompt with memories injected
2. After sending response to chat, spawn memory extraction

```rust
    #[instrument(skip(self, ctx))]
    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;

        // Check cooldown
        {
            let cooldowns_guard = self.cooldowns.lock().await;
            if let Some(last_use) = cooldowns_guard.get(user) {
                let elapsed = last_use.elapsed();
                if elapsed < self.cooldown {
                    let remaining = self.cooldown - elapsed;
                    debug!(
                        user = %user,
                        remaining_secs = remaining.as_secs(),
                        "AI command on cooldown"
                    );
                    if let Err(e) = ctx
                        .client
                        .say_in_reply_to(
                            ctx.privmsg,
                            format!("Bitte warte noch {} Waiting", format_cooldown_remaining(remaining)),
                        )
                        .await
                    {
                        error!(error = ?e, "Failed to send cooldown message");
                    }
                    return Ok(());
                }
            }
        }

        let instruction = ctx.args.join(" ");

        // Check for empty instruction
        if instruction.trim().is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, "Benutzung: !ai <anweisung>".to_string())
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        debug!(user = %user, instruction = %instruction, "Processing AI command");

        // Update cooldown before making the API call
        {
            let mut cooldowns_guard = self.cooldowns.lock().await;
            cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
        }

        // Build system prompt with memories injected
        let system_prompt = if let Some(ref store) = self.memory_store {
            let store_guard = store.read().await;
            match store_guard.format_for_prompt() {
                Some(facts) => format!("{}{}", self.system_prompt, facts),
                None => self.system_prompt.clone(),
            }
        } else {
            self.system_prompt.clone()
        };

        let user_message = self.instruction_template.replace("{message}", &instruction);

        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: system_prompt,
                },
                Message {
                    role: "user".to_string(),
                    content: user_message.clone(),
                },
            ],
        };

        // Execute AI with timeout
        let result = tokio::time::timeout(
            self.timeout,
            self.llm_client.chat_completion(request),
        )
        .await;

        let response = match result {
            Ok(Ok(text)) => truncate_response(&text, MAX_RESPONSE_LENGTH),
            Ok(Err(e)) => {
                error!(error = ?e, "AI execution failed");
                "Da ist was schiefgelaufen FDM".to_string()
            }
            Err(_) => {
                error!("AI execution timed out");
                "Das hat zu lange gedauert Waiting".to_string()
            }
        };

        // Send response to chat immediately
        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response.clone()).await {
            error!(error = ?e, "Failed to send AI response");
        }

        // Spawn fire-and-forget memory extraction (only on successful AI responses)
        if self.memory_store.is_some() && !response.starts_with("Da ist was") && !response.starts_with("Das hat zu lange") {
            memory::spawn_memory_extraction(
                self.llm_client.clone(),
                self.model.clone(),
                self.memory_store.clone().unwrap(),
                self.memory_store_path.clone().unwrap(),
                self.max_memories,
                user.to_string(),
                instruction,
                response,
                self.timeout,
            );
        }

        Ok(())
    }
```

- [ ] **Step 4: Update main.rs to pass Arc and memory store**

In `src/main.rs`, in `run_generic_command_handler`, update the LLM client creation to produce `Arc<dyn llm::LlmClient>` instead of `Box<dyn llm::LlmClient>`:

Change the type annotation (around line 1614):

```rust
    let llm_client: Option<(Arc<dyn llm::LlmClient>, AiConfig)> =
```

And the two `.map` calls that create the client:

For OpenAI (around line 1627):
```rust
                    .map(|c| Arc::new(c) as Arc<dyn llm::LlmClient>)
```

For Ollama (around line 1633):
```rust
                .map(|c| Arc::new(c) as Arc<dyn llm::LlmClient>),
```

Then update the AiCommand instantiation block (around line 1670):

```rust
    if let Some((client, cfg)) = llm_client {
        // Load memory store if enabled
        let (memory_store, memory_store_path) = if cfg.memory_enabled {
            match memory::MemoryStore::load(&data_dir) {
                Ok((store, path)) => (
                    Some(Arc::new(tokio::sync::RwLock::new(store))),
                    Some(path),
                ),
                Err(e) => {
                    error!(error = ?e, "Failed to load AI memory store, memory disabled");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        commands.push(Box::new(commands::ai::AiCommand::new(
            client,
            cfg.model,
            cfg.system_prompt,
            cfg.instruction_template,
            Duration::from_secs(cfg.timeout),
            Duration::from_secs(cooldowns.ai),
            memory_store,
            memory_store_path,
            cfg.max_memories,
        )));
    }
```

Add the needed import at the top of `main.rs`:

```rust
use std::sync::Arc;
```

(Check if `Arc` is already imported — it likely is from the existing code. If so, skip this.)

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: Clean compile.

- [ ] **Step 6: Commit**

```bash
git add src/commands/ai.rs src/main.rs
git commit -m "feat: wire memory store into AiCommand with extraction"
```

---

### Task 8: Update CLAUDE.md documentation

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add memory documentation to CLAUDE.md**

Add a section about the memory system to the Architecture section in `CLAUDE.md`. Under the "State Management" heading, add:

```markdown
**AI Memory State:**
- `memory_store`: `Arc<RwLock<MemoryStore>>` - Channel-wide fact store managed by the LLM
- Persisted to `ai_memory.ron` via atomic write+rename
- Loaded at startup if `memory_enabled = true` in `[ai]` config
- Read on every `!ai` request (injected into system prompt)
- Written by fire-and-forget extraction task after each successful AI response
- Capped at `max_memories` (default 50, max 200)
```

Update the `[ai]` config section to include the new fields:

```markdown
- `memory_enabled` - Enable persistent AI memory (optional, default: false)
- `max_memories` - Maximum number of stored facts (optional, default: 50, max: 200)
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document AI persistent memory in CLAUDE.md"
```

---

### Task 9: Final verification

**Files:** (none — verification only)

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: Clean build with no errors.

- [ ] **Step 2: Clippy lint check**

Run: `cargo clippy`
Expected: No warnings or errors (except pre-existing ones).

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 4: Verify config.toml.example is valid**

Run: `cargo check` with a config that has `memory_enabled = true` and `max_memories = 50` (mental verification that the serde defaults work).

- [ ] **Step 5: Final commit if any fixes needed**

If any fixes were needed during verification:

```bash
git add -A
git commit -m "fix: address build/lint issues from final verification"
```
