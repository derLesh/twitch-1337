pub mod ollama;
pub mod openai;

use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

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
    /// The name of the tool that was invoked. Required by Ollama (`tool_name`)
    /// and harmless for OpenAI-compatible providers.
    pub tool_name: String,
    pub content: String,
}

/// One round of tool calling: the assistant's `tool_calls` and the matching
/// `tool` role results. Strict providers require the assistant turn carrying
/// `tool_calls` to precede the results referencing its `tool_call_id`s, so
/// multi-round loops must thread both halves back into the next request.
#[derive(Debug, Clone)]
pub struct ToolCallRound {
    pub calls: Vec<ToolCall>,
    pub results: Vec<ToolResultMessage>,
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
    /// Prior tool-call rounds, threaded back in order.
    pub prior_rounds: Vec<ToolCallRound>,
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
    /// The model returned a text response (content may be unused by callers).
    Message(#[allow(dead_code)] String),
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

/// Build an [`LlmClient`] from optional AI config.
///
/// Returns `Ok(None)` when no AI config is provided or AI is disabled.
/// Returns `Ok(Some(client))` when the client is successfully built.
/// Returns `Err` only when the configuration is invalid (not when the backend is unreachable).
pub fn build_llm_client(
    ai_config: Option<&crate::config::AiConfig>,
) -> Result<Option<Arc<dyn LlmClient>>> {
    use crate::config::AiBackend;
    use secrecy::ExposeSecret as _;

    let Some(ai_cfg) = ai_config else {
        debug!("AI not configured, AI command disabled");
        return Ok(None);
    };

    let result = match ai_cfg.backend {
        AiBackend::OpenAi => {
            let api_key = ai_cfg
                .api_key
                .as_ref()
                .expect("validated: openai backend has api_key");
            openai::OpenAiClient::new(
                api_key.expose_secret(),
                &ai_cfg.model,
                ai_cfg.base_url.as_deref(),
            )
            .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
        }
        AiBackend::Ollama => ollama::OllamaClient::new(&ai_cfg.model, ai_cfg.base_url.as_deref())
            .map(|c| Arc::new(c) as Arc<dyn LlmClient>),
    };

    match result {
        Ok(client) => {
            info!(backend = ?ai_cfg.backend, model = %ai_cfg.model, "LLM client initialized");
            Ok(Some(client))
        }
        Err(e) => {
            error!(error = ?e, "Failed to initialize LLM client");
            Ok(None)
        }
    }
}
