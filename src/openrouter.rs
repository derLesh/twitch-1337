use eyre::{Result, WrapErr as _};
use reqwest::header::{self, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::APP_USER_AGENT;

/// A message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments as a JSON string (needs to be parsed)
    pub arguments: String,
}

/// Tool definition for the API (OpenAI format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

/// Function definition within a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Request body for the OpenRouter chat/completions endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// A choice in the chat completion response.
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub message: Message,
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

/// Response from the OpenRouter chat/completions endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<Choice>,
}

/// HTTP client for the OpenRouter API.
#[derive(Debug, Clone)]
pub struct OpenRouterClient {
    http: reqwest::Client,
    model: String,
}

impl OpenRouterClient {
    /// Creates a new OpenRouter API client.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    #[instrument(skip(api_key))]
    pub fn new(api_key: &str, model: &str) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        // OpenRouter uses Bearer token auth
        let mut auth_value = HeaderValue::from_str(&format!("Bearer {}", api_key))
            .wrap_err("Invalid API key format")?;
        auth_value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth_value);

        // Required OpenRouter headers
        headers.insert(
            "HTTP-Referer",
            HeaderValue::from_static("https://github.com/chronophylos/twitch-1337"),
        );
        headers.insert("X-Title", HeaderValue::from_static("twitch-1337"));

        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .default_headers(headers)
            .build()
            .wrap_err("Failed to build HTTP Client")?;

        Ok(Self {
            http,
            model: model.to_string(),
        })
    }

    /// Sends a chat completion request to OpenRouter.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or the response cannot be parsed.
    #[instrument(skip(self, request))]
    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let url = "https://openrouter.ai/api/v1/chat/completions";

        debug!(model = %self.model, "Sending request to OpenRouter API");

        let response = self
            .http
            .post(url)
            .json(&request)
            .send()
            .await
            .wrap_err("Failed to send request to OpenRouter API")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(eyre::eyre!(
                "OpenRouter API error (status {}): {}",
                status,
                error_body
            ));
        }

        response
            .json::<ChatCompletionResponse>()
            .await
            .wrap_err("Failed to parse OpenRouter API response")
    }

    /// Returns the model name.
    pub fn model(&self) -> &str {
        &self.model
    }
}
