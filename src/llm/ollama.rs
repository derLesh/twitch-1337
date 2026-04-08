use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use super::{ChatCompletionRequest, LlmClient};
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
}
