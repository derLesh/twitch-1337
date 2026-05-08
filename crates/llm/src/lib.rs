//! Provider-agnostic LLM client for twitch-1337.

pub mod agent;
pub mod error;
pub mod ollama;
pub mod openai;

mod client;
mod types;
mod util;

pub use agent::{AgentOpts, AgentOutcome, ToolExecutor, run_agent};
pub use client::LlmClient;
pub use error::{LlmError, Result};
pub use ollama::OllamaClient;
pub use openai::OpenAiClient;
pub use types::{
    ChatCompletionRequest, Message, Role, ToolArgsError, ToolCall, ToolCallRound,
    ToolChatCompletionRequest, ToolChatCompletionResponse, ToolDefinition, ToolResultMessage,
    TraceIds,
};
