use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::client::LlmClient;
use crate::error::{LlmError, Result};
use crate::types::{
    ChatCompletionRequest, ToolCall, ToolChatCompletionRequest, ToolChatCompletionResponse,
};

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
                "role": m.role.to_string(),
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
}

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

impl OllamaClient {
    /// Creates a new Ollama API client.
    #[instrument]
    pub fn new(base_url: Option<&str>, user_agent: &str) -> Result<Self> {
        let base_url = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');

        let http = reqwest::Client::builder().user_agent(user_agent).build()?;

        Ok(Self {
            http,
            base_url: base_url.to_string(),
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
                    role: m.role.to_string(),
                    content: m.content,
                })
                .collect(),
            stream: false,
        };

        debug!(model = %api_request.model, "Sending request to Ollama API");

        let response = self.http.post(&url).json(&api_request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                body,
            });
        }

        let body: serde_json::Value = response.json().await?;
        let api_response: ApiResponse =
            serde_json::from_value(body).map_err(|source| LlmError::Decode {
                stage: "ollama chat response",
                source,
            })?;

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

        debug!(model = %api_request.model, "Sending tool request to Ollama API");

        let response = self.http.post(&url).json(&api_request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                body,
            });
        }

        let body: serde_json::Value = response.json().await?;
        let api_response: ApiToolResponse =
            serde_json::from_value(body).map_err(|source| LlmError::Decode {
                stage: "ollama tool response",
                source,
            })?;

        if let Some(tool_calls) = api_response.message.tool_calls
            && !tool_calls.is_empty()
        {
            let calls = tool_calls
                .into_iter()
                .enumerate()
                .map(|(i, tc)| ToolCall {
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

        let content = api_response
            .message
            .content
            .ok_or(LlmError::EmptyResponse)?;
        Ok(ToolChatCompletionResponse::Message(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Message, ToolCall, ToolCallRound, ToolChatCompletionRequest, ToolResultMessage,
    };

    #[test]
    fn empty_message_content_is_error_not_empty_string() {
        // Source-level regression: the tool path must surface an
        // EmptyResponse error rather than handing back Message("").
        // Assemble the needle at runtime so this assertion does not
        // self-match the literal it is looking for.
        let s = include_str!("ollama.rs");
        let needle = format!("{}.{}()", "content", "unwrap_or_default");
        assert!(
            !s.contains(&needle),
            "tool path must error on empty content, not return Message(\"\")"
        );
    }

    #[test]
    fn build_messages_two_rounds_emits_correct_sequence() {
        let request = ToolChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![Message::system("sys"), Message::user("hi")],
            tools: vec![],
            reasoning_effort: None,
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
            trace: crate::types::TraceIds::default(),
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
