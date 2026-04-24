use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use super::{
    ChatCompletionRequest, LlmClient, ToolChatCompletionRequest, ToolChatCompletionResponse,
};
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

// --- Message serialization ---

/// Build the `messages` array for an Ollama tool request.
///
/// Per the Ollama native API, `tool_calls[].function.arguments` is an
/// **object** (not a JSON-encoded string), and tool results carry `tool_name`
/// instead of `tool_call_id` — Ollama keys by tool name.
fn build_ollama_messages(request: &ToolChatCompletionRequest) -> Vec<serde_json::Value> {
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

    for round in &request.prior_rounds {
        let tool_calls: Vec<serde_json::Value> = round
            .calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments,
                    },
                })
            })
            .collect();

        messages.push(serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": tool_calls,
        }));

        for tr in &round.results {
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_name": tr.tool_name,
                "content": tr.content,
            }));
        }
    }

    messages
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

        let messages = build_ollama_messages(&request);

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
                    arguments_parse_error: None,
                })
                .collect();
            return Ok(ToolChatCompletionResponse::ToolCalls {
                calls,
                reasoning_content: None,
            });
        }

        let content = api_response.message.content.unwrap_or_default();
        Ok(ToolChatCompletionResponse::Message(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{
        Message, ToolCall, ToolCallRound, ToolChatCompletionRequest, ToolResultMessage,
    };

    #[test]
    fn build_messages_two_rounds_emits_correct_sequence() {
        let request = ToolChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: "sys".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: "hi".to_string(),
                },
            ],
            tools: vec![],
            prior_rounds: vec![
                ToolCallRound {
                    calls: vec![ToolCall {
                        id: "call_0".to_string(),
                        name: "save_memory".to_string(),
                        arguments: serde_json::json!({"key": "k1", "fact": "f1"}),
                        arguments_parse_error: None,
                    }],
                    results: vec![ToolResultMessage {
                        tool_call_id: "call_0".to_string(),
                        tool_name: "save_memory".to_string(),
                        content: "Saved memory 'k1'".to_string(),
                    }],
                    reasoning_content: None,
                },
                ToolCallRound {
                    calls: vec![ToolCall {
                        id: "call_0".to_string(),
                        name: "delete_memory".to_string(),
                        arguments: serde_json::json!({"key": "k2"}),
                        arguments_parse_error: None,
                    }],
                    results: vec![ToolResultMessage {
                        tool_call_id: "call_0".to_string(),
                        tool_name: "delete_memory".to_string(),
                        content: "Deleted memory 'k2'".to_string(),
                    }],
                    reasoning_content: None,
                },
            ],
        };

        let msgs = build_ollama_messages(&request);

        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");

        assert_eq!(msgs[2]["role"], "assistant");
        let calls1 = msgs[2]["tool_calls"].as_array().expect("tool_calls array");
        assert_eq!(calls1.len(), 1);
        assert_eq!(calls1[0]["function"]["name"], "save_memory");
        // Ollama: arguments is an object, not a JSON-encoded string.
        assert_eq!(
            calls1[0]["function"]["arguments"],
            serde_json::json!({"key": "k1", "fact": "f1"})
        );

        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_name"], "save_memory");
        assert_eq!(msgs[3]["content"], "Saved memory 'k1'");

        assert_eq!(msgs[4]["role"], "assistant");
        assert_eq!(
            msgs[4]["tool_calls"][0]["function"]["name"],
            "delete_memory"
        );

        assert_eq!(msgs[5]["role"], "tool");
        assert_eq!(msgs[5]["tool_name"], "delete_memory");
        assert_eq!(msgs[5]["content"], "Deleted memory 'k2'");
    }
}
