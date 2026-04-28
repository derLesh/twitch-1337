use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use reqwest::header::{self, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, trace, warn};

use super::{
    ChatCompletionRequest, LlmClient, ToolChatCompletionRequest, ToolChatCompletionResponse,
    truncate_for_echo,
};
use crate::APP_USER_AGENT;

// --- Internal serde types for OpenAI-compatible API ---

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ApiRequest {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ApiReasoning>,
}

#[derive(Debug, Serialize)]
struct ApiReasoning {
    effort: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponseMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiChoice {
    message: ApiResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    choices: Vec<ApiChoice>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ApiReasoning>,
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
    // DeepSeek/OpenRouter returns this as "reasoning"; standard field is "reasoning_content"
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiToolResponse {
    choices: Vec<ApiToolChoice>,
}

// --- Message serialization ---

/// Build the `messages` array for an OpenAI-compatible tool request.
///
/// Per the OpenAI spec, `tool_calls[].function.arguments` is a JSON-encoded
/// **string** (not an object), so we re-stringify the parsed `Value` from the
/// response.
fn build_openai_messages(request: &ToolChatCompletionRequest) -> Vec<serde_json::Value> {
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
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments.to_string(),
                    },
                })
            })
            .collect();

        let mut assistant_msg = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": tool_calls,
        });
        if let Some(rc) = &round.reasoning_content {
            assistant_msg["reasoning_content"] = serde_json::Value::String(rc.clone());
        }
        messages.push(assistant_msg);

        for tr in &round.results {
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tr.tool_call_id,
                "content": tr.content,
            }));
        }
    }

    messages
}

/// Parse the `arguments` string from an OpenAI-compatible tool call. Returns
/// the parsed `Value` on success; on failure, returns `Value::Null` plus a
/// `ToolCallArgsError` with the parse error and a truncated copy of the raw
/// payload, and emits a `warn!` tracing event. Executors use the error to
/// surface a useful tool-result to the model instead of silently dropping.
fn parse_tool_call_arguments(
    tool: &str,
    id: &str,
    raw: &str,
) -> (serde_json::Value, Option<super::ToolCallArgsError>) {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => (v, None),
        Err(e) => {
            warn!(tool, id, error = %e, raw, "invalid tool-call JSON arguments");
            let err = super::ToolCallArgsError {
                error: e.to_string(),
                raw: truncate_for_echo(raw, 512),
            };
            (serde_json::Value::Null, Some(err))
        }
    }
}

/// Extract the error message from a 2xx response body that carries an API-level
/// error (e.g. OpenRouter rate-limit: `{"error":{"message":"..."}}`).
fn extract_api_error(body: &serde_json::Value) -> Option<String> {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_owned)
}

// --- Client ---

/// HTTP client for any OpenAI-compatible API (OpenRouter, OpenAI, etc.).
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    is_openrouter: bool,
}

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

impl OpenAiClient {
    fn map_reasoning(&self, effort: Option<String>) -> (Option<String>, Option<ApiReasoning>) {
        if self.is_openrouter {
            (None, effort.map(|effort| ApiReasoning { effort }))
        } else {
            (effort, None)
        }
    }

    /// Creates a new OpenAI-compatible API client.
    #[instrument(skip(api_key))]
    pub fn new(api_key: &str, model: &str, base_url: Option<&str>) -> Result<Self> {
        let base_url = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
        let is_openrouter = base_url.contains("openrouter.ai");

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        let mut auth_value = HeaderValue::from_str(&format!("Bearer {}", api_key))
            .wrap_err("Invalid API key format")?;
        auth_value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth_value);

        // OpenRouter headers — harmless for other providers, required for OpenRouter
        headers.insert(
            "HTTP-Referer",
            HeaderValue::from_static("https://github.com/chronophylos/twitch-1337"),
        );
        headers.insert("X-Title", HeaderValue::from_static("twitch-1337"));

        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .default_headers(headers)
            .build()
            .wrap_err("Failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: base_url.to_string(),
            model: model.to_string(),
            is_openrouter,
        })
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    #[instrument(skip(self, request))]
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);

        let ChatCompletionRequest {
            model,
            messages,
            reasoning_effort,
        } = request;
        let (reasoning_effort, reasoning) = self.map_reasoning(reasoning_effort);

        let api_request = ApiRequest {
            model,
            messages: messages
                .into_iter()
                .map(|m| ApiMessage {
                    role: m.role,
                    content: m.content,
                })
                .collect(),
            reasoning_effort,
            reasoning,
        };

        debug!(model = %self.model, "Sending request to OpenAI-compatible API");

        let response = self
            .http
            .post(&url)
            .json(&api_request)
            .send()
            .await
            .wrap_err("Failed to send request to OpenAI-compatible API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "OpenAI-compatible API error (status {}): {}",
                status,
                error_body
            ));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .wrap_err("Failed to parse OpenAI-compatible API response")?;
        if let Some(msg) = extract_api_error(&body) {
            return Err(eyre::eyre!("OpenAI-compatible API error: {}", msg));
        }
        let api_response: ApiResponse = serde_json::from_value(body)
            .wrap_err("Failed to parse OpenAI-compatible API response")?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| eyre::eyre!("No choices in API response"))?;

        choice
            .message
            .content
            .ok_or_else(|| eyre::eyre!("No text response from API"))
    }

    #[instrument(skip(self, request))]
    async fn chat_completion_with_tools(
        &self,
        request: ToolChatCompletionRequest,
    ) -> Result<ToolChatCompletionResponse> {
        let url = format!("{}/chat/completions", self.base_url);

        let ToolChatCompletionRequest {
            model,
            messages,
            tools,
            reasoning_effort,
            prior_rounds,
        } = request;
        let (reasoning_effort, reasoning) = self.map_reasoning(reasoning_effort);

        let request = ToolChatCompletionRequest {
            model,
            messages,
            tools,
            reasoning_effort: None,
            prior_rounds,
        };

        let messages = build_openai_messages(&request);

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
            reasoning_effort,
            reasoning,
        };

        if let Ok(req_json) = serde_json::to_string(&api_request) {
            trace!(request_body = %req_json, "Sending tool request to OpenAI-compatible API");
        } else {
            debug!(model = %self.model, "Sending tool request to OpenAI-compatible API");
        }

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

        let body: serde_json::Value = response
            .json()
            .await
            .wrap_err("Failed to parse OpenAI-compatible API tool response")?;
        trace!(response_body = %body, "OpenAI-compatible API raw tool response");
        if let Some(msg) = extract_api_error(&body) {
            return Err(eyre::eyre!("OpenAI-compatible API error: {}", msg));
        }
        let api_response: ApiToolResponse = serde_json::from_value(body)
            .wrap_err("Failed to parse OpenAI-compatible API tool response")?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| eyre::eyre!("No choices in API tool response"))?;

        debug!(
            content = ?choice.message.content,
            reasoning_content = ?choice.message.reasoning_content,
            has_tool_calls = choice.message.tool_calls.is_some(),
            "Parsed assistant message from tool response"
        );

        if let Some(tool_calls) = choice.message.tool_calls
            && !tool_calls.is_empty()
        {
            let calls = tool_calls
                .into_iter()
                .map(|tc| {
                    let (arguments, arguments_parse_error) = parse_tool_call_arguments(
                        &tc.function.name,
                        &tc.id,
                        &tc.function.arguments,
                    );
                    super::ToolCall {
                        id: tc.id,
                        name: tc.function.name,
                        arguments,
                        arguments_parse_error,
                    }
                })
                .collect();
            return Ok(ToolChatCompletionResponse::ToolCalls {
                calls,
                reasoning_content: choice.message.reasoning_content,
            });
        }

        let content = choice.message.content.unwrap_or_default();
        Ok(ToolChatCompletionResponse::Message(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::llm::{
        Message, ToolCall, ToolCallRound, ToolChatCompletionRequest, ToolResultMessage,
    };

    fn test_client(is_openrouter: bool) -> OpenAiClient {
        OpenAiClient {
            http: reqwest::Client::new(),
            base_url: "https://example.test".to_string(),
            model: "test-model".to_string(),
            is_openrouter,
        }
    }

    fn req_with_rounds(rounds: Vec<ToolCallRound>) -> ToolChatCompletionRequest {
        ToolChatCompletionRequest {
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
            reasoning_effort: None,
            prior_rounds: rounds,
        }
    }

    #[test]
    fn build_messages_empty_rounds_passes_through_base_messages() {
        let msgs = build_openai_messages(&req_with_rounds(vec![]));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn build_messages_two_rounds_emits_correct_sequence() {
        let round1 = ToolCallRound {
            calls: vec![ToolCall {
                id: "X".to_string(),
                name: "save_memory".to_string(),
                arguments: serde_json::json!({"key": "k1", "fact": "f1"}),
                arguments_parse_error: None,
            }],
            results: vec![ToolResultMessage {
                tool_call_id: "X".to_string(),
                tool_name: "save_memory".to_string(),
                content: "Saved memory 'k1'".to_string(),
            }],
            reasoning_content: None,
        };
        let round2 = ToolCallRound {
            calls: vec![ToolCall {
                id: "Y".to_string(),
                name: "delete_memory".to_string(),
                arguments: serde_json::json!({"key": "k2"}),
                arguments_parse_error: None,
            }],
            results: vec![ToolResultMessage {
                tool_call_id: "Y".to_string(),
                tool_name: "delete_memory".to_string(),
                content: "Deleted memory 'k2'".to_string(),
            }],
            reasoning_content: None,
        };

        let msgs = build_openai_messages(&req_with_rounds(vec![round1, round2]));

        // Expected layout:
        // [0] system, [1] user,
        // [2] assistant(tool_calls=[X]), [3] tool(tool_call_id=X),
        // [4] assistant(tool_calls=[Y]), [5] tool(tool_call_id=Y)
        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");

        assert_eq!(msgs[2]["role"], "assistant");
        assert!(msgs[2]["content"].is_null());
        let calls1 = msgs[2]["tool_calls"].as_array().expect("tool_calls array");
        assert_eq!(calls1.len(), 1);
        assert_eq!(calls1[0]["id"], "X");
        assert_eq!(calls1[0]["type"], "function");
        assert_eq!(calls1[0]["function"]["name"], "save_memory");
        // Arguments must be a JSON-encoded *string* per OpenAI spec.
        let args1_str = calls1[0]["function"]["arguments"]
            .as_str()
            .expect("arguments must be a string");
        let args1_parsed: serde_json::Value = serde_json::from_str(args1_str).unwrap();
        assert_eq!(args1_parsed, serde_json::json!({"key": "k1", "fact": "f1"}));

        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "X");
        assert_eq!(msgs[3]["content"], "Saved memory 'k1'");

        assert_eq!(msgs[4]["role"], "assistant");
        let calls2 = msgs[4]["tool_calls"].as_array().expect("tool_calls array");
        assert_eq!(calls2[0]["id"], "Y");

        assert_eq!(msgs[5]["role"], "tool");
        assert_eq!(msgs[5]["tool_call_id"], "Y");
        assert_eq!(msgs[5]["content"], "Deleted memory 'k2'");
    }

    #[test]
    fn build_messages_reasoning_content_included_in_assistant_turn() {
        let round = ToolCallRound {
            calls: vec![ToolCall {
                id: "Z".to_string(),
                name: "save_memory".to_string(),
                arguments: serde_json::json!({"key": "k"}),
                arguments_parse_error: None,
            }],
            results: vec![ToolResultMessage {
                tool_call_id: "Z".to_string(),
                tool_name: "save_memory".to_string(),
                content: "ok".to_string(),
            }],
            reasoning_content: Some("I should save this fact.".to_string()),
        };

        let msgs = build_openai_messages(&req_with_rounds(vec![round]));

        // [0] system, [1] user, [2] assistant, [3] tool
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(
            msgs[2]["reasoning_content"], "I should save this fact.",
            "reasoning_content must be echoed back in the assistant turn"
        );
    }

    #[test]
    fn build_messages_no_reasoning_content_omits_field() {
        let round = ToolCallRound {
            calls: vec![ToolCall {
                id: "Z".to_string(),
                name: "save_memory".to_string(),
                arguments: serde_json::json!({"key": "k"}),
                arguments_parse_error: None,
            }],
            results: vec![ToolResultMessage {
                tool_call_id: "Z".to_string(),
                tool_name: "save_memory".to_string(),
                content: "ok".to_string(),
            }],
            reasoning_content: None,
        };

        let msgs = build_openai_messages(&req_with_rounds(vec![round]));

        assert!(
            msgs[2].get("reasoning_content").is_none(),
            "reasoning_content must not be present when None"
        );
    }

    #[test]
    fn parse_tool_call_arguments_valid_json_returns_value() {
        let (args, err) =
            parse_tool_call_arguments("save_memory", "X", r#"{"key":"k","fact":"f"}"#);
        assert!(err.is_none());
        assert_eq!(args, serde_json::json!({"key": "k", "fact": "f"}));
    }

    #[test]
    fn parse_tool_call_arguments_malformed_json_returns_error() {
        let raw = r#"{"key":"k" "fact":"f"}"#; // missing comma
        let (args, err) = parse_tool_call_arguments("save_memory", "X", raw);
        assert_eq!(args, serde_json::Value::Null);
        let err = err.expect("parse error must be set");
        assert!(!err.error.is_empty());
        assert_eq!(err.raw, raw);
    }

    #[test]
    fn extract_api_error_returns_message_for_error_body() {
        let body = serde_json::json!({"error": {"message": "rate limit exceeded", "type": "rate_limit_error"}});
        assert_eq!(
            extract_api_error(&body).as_deref(),
            Some("rate limit exceeded")
        );
    }

    #[test]
    fn extract_api_error_returns_none_for_choices_body() {
        let body = serde_json::json!({"choices": [{"message": {"content": "hi"}}]});
        assert!(extract_api_error(&body).is_none());
    }

    #[test]
    fn parse_tool_call_arguments_truncates_oversized_raw() {
        let raw = "x".repeat(1024);
        let (_, err) = parse_tool_call_arguments("save_memory", "X", &raw);
        let err = err.expect("parse error must be set");
        assert!(err.raw.starts_with(&"x".repeat(512)));
        assert!(err.raw.contains("more chars"));
        assert!(err.raw.chars().count() < raw.chars().count());
    }

    #[test]
    fn map_reasoning_openrouter_uses_nested_reasoning_object() {
        let client = test_client(true);
        let (reasoning_effort, reasoning) = client.map_reasoning(Some("high".to_string()));
        assert!(reasoning_effort.is_none());
        let reasoning = reasoning.expect("reasoning object must be set");
        assert_eq!(reasoning.effort, "high");
    }

    #[test]
    fn map_reasoning_non_openrouter_uses_reasoning_effort_field() {
        let client = test_client(false);
        let (reasoning_effort, reasoning) = client.map_reasoning(Some("xhigh".to_string()));
        assert_eq!(reasoning_effort.as_deref(), Some("xhigh"));
        assert!(reasoning.is_none());
    }
}
