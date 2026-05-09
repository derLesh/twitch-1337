//! Selects and constructs an LLM backend based on bot configuration.

use std::sync::Arc;

use eyre::Result;
use llm::{LlmClient, OllamaClient, OpenAiClient};
use secrecy::ExposeSecret as _;
use tracing::{debug, error, info};

use crate::APP_USER_AGENT;
use crate::config::{AiBackend, AiConfig};

/// Build an [`LlmClient`] from optional AI config.
///
/// Returns `Ok(None)` when no AI config is provided or AI is disabled.
/// Returns `Ok(Some(client))` when the client is successfully built.
/// Returns `Err` only when the configuration is invalid (not when the backend is unreachable).
pub fn build_llm_client(ai_config: Option<&AiConfig>) -> Result<Option<Arc<dyn LlmClient>>> {
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
            OpenAiClient::new(
                api_key.expose_secret(),
                ai_cfg.base_url.as_deref(),
                APP_USER_AGENT,
            )
            .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
        }
        AiBackend::Ollama => OllamaClient::new(ai_cfg.base_url.as_deref(), APP_USER_AGENT)
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
