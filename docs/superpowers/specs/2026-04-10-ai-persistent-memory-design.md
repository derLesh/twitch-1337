# AI Persistent Memory Design

**Date**: 2026-04-10
**Issue**: #13
**Depends on**: #12 (conversation context)

## Summary

Give the `!ai` command persistent, LLM-managed memory. The AI autonomously decides what facts to remember, update, or forget about users and the channel. Memories persist across bot restarts and are injected into every AI request so the model can reference them naturally.

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Memory management | LLM-driven via tool calling | Most natural UX; AI "just remembers" |
| Fallback mechanism | None | If the model doesn't support tools, memory is disabled |
| Scope | Channel-wide | Shared across all users; simplest starting point |
| Admin controls | None | AI is sole manager of its own memory |
| Memory access | Always inject | All memories in system prompt on every request; cap is small enough |
| Response latency | Post-response extraction (Approach C) | Zero added latency to user-visible response |

## Request Flow

```
User sends "!ai <message>"
  |
  +-- 1. Build system prompt with all stored memories injected
  +-- 2. Send chat completion request to LLM (normal, no tools)
  +-- 3. Get response text, send to Twitch chat immediately
  |
  +-- 4. Spawn fire-and-forget tokio task:
       +-- Build extraction prompt with: user message, AI response, chat history
       +-- Send tool-calling LLM request with save_memory / delete_memory tools
       +-- If model calls tools -> execute against MemoryStore, persist to disk
       +-- Repeat until model returns text or 3 iterations max
       +-- Log result, swallow errors
```

The user-visible response has zero added latency. Memory extraction happens in the background. Extraction failures never affect the chat experience.

## Memory Store

### Data Model

```rust
struct Memory {
    fact: String,          // e.g. "Chrono's favorite game is Factorio"
    created_at: String,    // ISO 8601 timestamp
    updated_at: String,    // ISO 8601 timestamp
}

struct MemoryStore {
    memories: HashMap<String, Memory>,  // Keyed by LLM-generated slug
}
```

The LLM generates the slug when calling `save_memory` (e.g. `chrono-favorite-game`). Using a slug rather than a numeric ID lets the model naturally overwrite a memory by reusing the same key.

### Persistence

- File: `ai_memory.ron` in `get_data_dir()`
- Format: RON (consistent with `pings.ron`, `token.ron`, `leaderboard.ron`)
- Write pattern: atomic write-then-rename via `.ron.tmp`
- Loaded at startup if `memory_enabled = true`

### Runtime Wrapper

`Arc<RwLock<MemoryStore>>` — the main request path only reads (inject memories into system prompt), the background extraction task writes.

### Capacity

Configurable `max_memories` (default 50, max 200). When at capacity and the LLM tries to save a new memory, the tool returns an error telling the LLM to delete something first.

## Tool Definitions

Two tools exposed to the LLM during the extraction call:

### `save_memory`

- Parameters: `key` (string slug), `fact` (string)
- Key exists: overwrites fact, updates `updated_at`
- Key is new, under cap: creates new entry with `created_at` and `updated_at` set to now
- Key is new, at cap: returns error "Memory full — delete a memory first"

### `delete_memory`

- Parameters: `key` (string slug)
- Key exists: removes it, returns success
- Key doesn't exist: returns error "No memory with that key"

No separate `update_memory` — `save_memory` with an existing key covers updates. Minimal tool set reduces model confusion.

The extraction prompt includes the full list of current memory keys and facts so the model can make informed decisions about overwrites and deletions.

## LLM Client Changes

### New Trait Method

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String>;

    async fn chat_completion_with_tools(
        &self,
        request: ChatCompletionRequest,
        tools: Vec<ToolDefinition>,
    ) -> Result<ChatCompletionResponse>;
}
```

### New Types

```rust
enum ChatCompletionResponse {
    Message(String),
    ToolCalls(Vec<ToolCall>),
}

struct ToolCall {
    name: String,
    arguments: serde_json::Value,  // Parsed from JSON string returned by LLM
}

struct ToolDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,  // JSON Schema
}
```

### Implementation

Both OpenAI and Ollama clients implement `chat_completion_with_tools`. The existing `chat_completion` method stays unchanged — the normal response path uses no tools.

### Extraction Loop

1. Call `chat_completion_with_tools` with extraction prompt and tool definitions
2. If `ToolCalls` — execute each against MemoryStore, collect results, send back as tool result messages in a follow-up call
3. Repeat until model returns `Message` or iteration limit (3) is hit
4. Discard final text response (only tool calls matter)

## System Prompt Injection

When memories exist, appended to the system prompt:

```
<existing system prompt>

## Known facts
- chrono-favorite-game: Chrono's favorite game is Factorio
- stream-schedule: Stream is usually on Tuesdays and Thursdays
- bot-name: The bot's name is 1337bot
```

Keys are included so the model can reference them during extraction for overwrites and deletes. When the memory store is empty, the section is omitted entirely.

### Extraction Prompt

Separate, hardcoded system prompt (not user-configurable):

```
You just had a conversation in a Twitch chat. Based on the exchange below,
decide if any facts are worth remembering long-term about users, the channel,
or the community. You can save new facts, overwrite outdated ones, or delete
incorrect ones. Only save things that would be useful across future
conversations. Do not save trivial or ephemeral things.

Current memories:
<key: fact list>

Recent chat history:
<chat history lines, if available>

Conversation:
User ({username}): {message}
Assistant: {response}
```

## Configuration

Two new fields on `AiConfig`:

```toml
[ai]
# ... existing fields ...
memory_enabled = false    # opt-in, disabled by default
max_memories = 50         # maximum stored facts (default 50, max 200)
```

Both have serde defaults so existing configs are unaffected.

### Validation

- `max_memories` must be >= 1 and <= 200
- Only validated when `memory_enabled = true`

### Behavior When Disabled

When `memory_enabled = false` (the default): no memory store loaded, no extraction calls, no injection into system prompt. The `!ai` flow is completely unchanged.
