use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use reqwest::header::{self, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use super::{ChatCompletionRequest, LlmClient};
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

// --- Client ---

/// HTTP client for any OpenAI-compatible API (OpenRouter, OpenAI, etc.).
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
}

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

impl OpenAiClient {
    /// Creates a new OpenAI-compatible API client.
    #[instrument(skip(api_key))]
    pub fn new(api_key: &str, model: &str, base_url: Option<&str>) -> Result<Self> {
        let base_url = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');

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
        })
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    #[instrument(skip(self, request))]
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);

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

        let api_response: ApiResponse = response
            .json()
            .await
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
}
