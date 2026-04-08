# LLM Backend Abstraction Design

Replace the hardcoded OpenRouter integration with a configurable LLM backend system supporting both OpenAI-compatible APIs and Ollama's native API.

## Goal

The `!ai` command should work identically regardless of which backend is configured. Users in chat see no difference. The bot operator picks the backend in `config.toml`.

## Configuration

The `[openrouter]` config section is replaced by `[ai]`:

```toml
# OpenAI-compatible backend (OpenRouter, OpenAI, Together, etc.)
[ai]
backend = "openai"
api_key = "sk-or-..."
base_url = "https://openrouter.ai/api/v1"  # optional, defaults to OpenRouter
model = "google/gemini-2.0-flash-exp:free"
system_prompt = "..."  # optional, has default
instruction_template = "{message}"  # optional, has default

# Ollama backend
[ai]
backend = "ollama"
base_url = "http://docker.homelab:11434"  # optional, defaults to http://localhost:11434
model = "gemma3:4b"
system_prompt = "..."  # optional, same default
instruction_template = "{message}"  # optional, same default
```

- `backend`: required, either `"openai"` or `"ollama"`
- `api_key`: required for `openai`, absent for `ollama`
- `base_url`: optional for both, sensible defaults
- `system_prompt`, `instruction_template`: optional, shared defaults (same as current)
- Entire `[ai]` section is optional. If absent, `!ai` command is disabled.
- Startup validation: fail if `backend = "openai"` and `api_key` is missing.

## Trait & Shared Types

New module `src/llm/mod.rs`:

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String>;
}

pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
}

pub struct Message {
    pub role: String,
    pub content: String,
}
```

The trait returns `String` directly (extracted response text), not the full API response. Consumers don't need to dig into response structure.

Unused fields from the current types (`tools`, `tool_calls`, `tool_call_id`) are dropped.

## Backend: OpenAI-compatible (`src/llm/openai.rs`)

The current `OpenRouterClient` renamed and generalized:

- Constructor takes `base_url`, `api_key`, `model`
- `base_url` defaults to `https://openrouter.ai/api/v1`
- POSTs to `{base_url}/chat/completions`
- Bearer token authentication
- Keeps OpenRouter-specific headers (`HTTP-Referer`, `X-Title`) — harmless for other providers, required for OpenRouter
- Implements `LlmClient` trait
- Response parsing: extracts `choices[0].message.content` internally

## Backend: Ollama Native (`src/llm/ollama.rs`)

New client using Ollama's native API:

- Constructor takes `base_url`, `model`
- `base_url` defaults to `http://localhost:11434`
- POSTs to `{base_url}/api/chat`
- No authentication
- Sets `stream: false` in request body
- Implements `LlmClient` trait
- Room for Ollama-specific methods later (e.g., `pull_model()`) outside the trait

## Wiring (`main.rs`)

1. `config.openrouter: Option<OpenRouterConfig>` becomes `config.ai: Option<AiConfig>`
2. Based on `ai_config.backend`, construct either `OpenAiClient` or `OllamaClient`, returned as `Box<dyn LlmClient>`
3. `AiCommand::new()` takes `Box<dyn LlmClient>` instead of `OpenRouterClient`
4. `system_prompt` and `instruction_template` still passed from config to `AiCommand`
5. If `[ai]` is absent, `!ai` command isn't registered (same as today)

No changes to the command dispatcher, broadcast architecture, or other handlers.

## File Changes

- **New**: `src/llm/mod.rs`, `src/llm/openai.rs`, `src/llm/ollama.rs`
- **Modified**: `src/main.rs` (config structs + wiring), `src/commands/ai.rs` (use `Box<dyn LlmClient>`), `config.toml.example`, `CLAUDE.md`
- **Deleted**: `src/openrouter.rs`
