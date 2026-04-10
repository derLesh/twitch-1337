use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use super::{ChatCompletionRequest, LlmClient, ToolChatCompletionRequest, ToolChatCompletionResponse};
use crate::APP_USER_AGENT;

// --- Internal serde types for Ollama native API ---

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ApiRequest {
    model: String,
    messages: Vec<ApiMessage>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ApiResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    message: ApiResponseMessage,
}

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

// --- Client ---

/// HTTP client for Ollama's native API.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
}

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

impl OllamaClient {
    /// Creates a new Ollama API client.
    #[instrument]
    pub fn new(model: &str, base_url: Option<&str>) -> Result<Self> {
        let base_url = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');

        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .build()
            .wrap_err("Failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: base_url.to_string(),
            model: model.to_string(),
        })
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    #[instrument(skip(self, request))]
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String> {
        let url = format!("{}/api/chat", self.base_url);

        let api_request = ApiRequest {
            model: request.model,
            messages: request
                .messages
                .into_iter()
                .map(|m| ApiMessage {
                    role: m.role,
                    content: m.content,
                })
                .collect(),
            stream: false,
        };

        debug!(model = %self.model, "Sending request to Ollama API");

        let response = self
            .http
            .post(&url)
            .json(&api_request)
            .send()
            .await
            .wrap_err("Failed to send request to Ollama API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "Ollama API error (status {}): {}",
                status,
                error_body
            ));
        }

        let api_response: ApiResponse = response
            .json()
            .await
            .wrap_err("Failed to parse Ollama API response")?;

        Ok(api_response.message.content)
    }

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

        if let Some(tool_calls) = api_response.message.tool_calls
            && !tool_calls.is_empty()
        {
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

        let content = api_response.message.content.unwrap_or_default();
        Ok(ToolChatCompletionResponse::Message(content))
    }
}
