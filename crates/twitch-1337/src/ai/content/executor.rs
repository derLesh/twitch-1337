use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{error, warn};

use llm::{ToolCall, ToolResultMessage};

use super::cache::TtlCache;
use super::client::{SearchClient, SearchResult};
use super::media::MediaClient;
use crate::config::AiMediaConfig;

#[derive(Debug, Deserialize)]
struct WebSearchArgs {
    query: String,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ReadUrlArgs {
    /// HTTP(S) URL to fetch.
    pub url: String,
    /// Optional natural-language instruction. If omitted, the sub-agent returns
    /// a generic description.
    #[serde(default)]
    pub instruction: Option<String>,
}

pub struct ContentToolExecutor {
    client: SearchClient,
    media: Arc<MediaClient>,
    caps: AiMediaConfig,
    max_results: usize,
    search_cache: Arc<Mutex<TtlCache<Vec<SearchResult>>>>,
    read_cache: Arc<Mutex<TtlCache<ReadCacheEntry>>>,
}

#[derive(Debug, Clone)]
struct ReadCacheEntry {
    content_type: String,
    answer: String,
}

impl ContentToolExecutor {
    pub fn new(
        client: SearchClient,
        media: Arc<MediaClient>,
        caps: AiMediaConfig,
        max_results: usize,
        cache_ttl: Duration,
        cache_capacity: usize,
    ) -> Self {
        Self {
            client,
            media,
            caps,
            max_results,
            search_cache: Arc::new(Mutex::new(TtlCache::new(cache_ttl, cache_capacity))),
            read_cache: Arc::new(Mutex::new(TtlCache::new(cache_ttl, cache_capacity))),
        }
    }

    pub fn max_results(&self) -> usize {
        self.max_results
    }

    pub async fn execute_tool_call(&self, call: &ToolCall) -> ToolResultMessage {
        let content = self.execute(call).await;
        ToolResultMessage::for_call(call, content)
    }

    async fn execute(&self, call: &ToolCall) -> String {
        match call.name.as_str() {
            "web_search" => match call.parse_args::<WebSearchArgs>() {
                Ok(args) => self.execute_web_search(args).await,
                Err(e) => Self::args_error_payload(&call.name, &e),
            },
            "read_url" => match call.parse_args::<ReadUrlArgs>() {
                Ok(args) => self.execute_read_url(args).await,
                Err(e) => Self::args_error_payload(&call.name, &e),
            },
            other => json!({
                "error": "unknown_tool",
                "tool": other,
            })
            .to_string(),
        }
    }

    fn args_error_payload(tool: &str, err: &llm::ToolArgsError) -> String {
        match err {
            llm::ToolArgsError::Provider { error, raw } => json!({
                "error": "invalid_arguments_json",
                "tool": tool,
                "details": error,
                "raw": raw,
            })
            .to_string(),
            llm::ToolArgsError::Deserialize { error } => json!({
                "error": "invalid_arguments",
                "tool": tool,
                "details": error,
            })
            .to_string(),
        }
    }

    async fn execute_web_search(&self, args: WebSearchArgs) -> String {
        let query = args.query.trim();
        if query.is_empty() {
            return json!({
                "error": "invalid_arguments",
                "details": "query cannot be empty",
            })
            .to_string();
        }

        let requested = args.max_results.unwrap_or(self.max_results);
        let effective_max = requested.clamp(1, self.max_results);

        let key = format!("{}::{}", normalize_query(query), effective_max);
        if let Some(cached) = self.search_cache.lock().await.get(&key) {
            return json!({
                "cached": true,
                "results": cached,
            })
            .to_string();
        }

        match self.client.web_search(query, effective_max).await {
            Ok(results) => {
                self.search_cache.lock().await.insert(key, results.clone());
                json!({
                    "cached": false,
                    "results": results,
                })
                .to_string()
            }
            Err(err) => {
                let error_code = if err
                    .chain()
                    .any(|cause| cause.to_string().to_ascii_lowercase().contains("timed out"))
                {
                    warn!(error = ?err, query, "web_search timed out");
                    "search_timeout"
                } else {
                    error!(error = ?err, query, "web_search failed");
                    "search_failed"
                };
                json!({
                    "error": error_code,
                    "details": format!("{err:#}"),
                })
                .to_string()
            }
        }
    }

    async fn execute_read_url(&self, args: ReadUrlArgs) -> String {
        let instruction = args
            .instruction
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let cache_key = format!(
            "{}::{}",
            normalize_url(&args.url),
            instruction.as_deref().unwrap_or("").to_ascii_lowercase()
        );
        if let Some(cached) = self.read_cache.lock().await.get(&cache_key) {
            return json!({
                "cached": true,
                "url": args.url,
                "content_type": cached.content_type,
                "answer": cached.answer,
            })
            .to_string();
        }

        let fetched = match self.client.fetch_for_read(&args.url, &self.caps).await {
            Ok(f) => f,
            Err(err) => return Self::map_fetch_err(&err, &args.url),
        };

        let answer = match self
            .media
            .analyze(
                &fetched.content_type,
                &fetched.payload,
                instruction.as_deref(),
            )
            .await
        {
            Ok(a) => a,
            Err(err) => return Self::map_media_err(&err, &args.url),
        };

        self.read_cache.lock().await.insert(
            cache_key,
            ReadCacheEntry {
                content_type: fetched.content_type.clone(),
                answer: answer.clone(),
            },
        );

        json!({
            "cached": false,
            "url": args.url,
            "content_type": fetched.content_type,
            "answer": answer,
        })
        .to_string()
    }

    fn map_fetch_err(err: &eyre::Report, url: &str) -> String {
        let msg = err.to_string().to_ascii_lowercase();
        let code = if msg.contains("blocked") {
            warn!(error = ?err, url, "read_url blocked");
            "fetch_blocked"
        } else if msg.contains("too large") {
            warn!(error = ?err, url, "read_url payload too large");
            "payload_too_large"
        } else if msg.contains("unsupported") {
            warn!(error = ?err, url, "read_url unsupported content type");
            "unsupported_content_type"
        } else if msg.contains("timed out") {
            warn!(error = ?err, url, "read_url timed out");
            "fetch_timeout"
        } else {
            error!(error = ?err, url, "read_url fetch failed");
            "fetch_failed"
        };
        json!({
            "error": code,
            "details": format!("{err:#}"),
        })
        .to_string()
    }

    fn map_media_err(err: &eyre::Report, url: &str) -> String {
        let msg = err.to_string().to_ascii_lowercase();
        let code = if msg.contains("timed out") {
            warn!(error = ?err, url, "read_url media timeout");
            "analysis_timeout"
        } else {
            error!(error = ?err, url, "read_url media failed");
            "analysis_failed"
        };
        json!({
            "error": code,
            "details": format!("{err:#}"),
        })
        .to_string()
    }
}

fn normalize_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_url(url: &str) -> String {
    url.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use llm::ToolArgsError;
    use std::time::Duration;

    use super::*;

    fn test_executor() -> ContentToolExecutor {
        crate::install_crypto_provider();
        let client = SearchClient::new_with_client(
            "http://127.0.0.1:65535/search".to_string(),
            Duration::from_secs(1),
            reqwest::Client::new(),
        );
        let media = Arc::new(MediaClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:65535/v1".to_string(),
            None,
            "test-model".into(),
            Duration::from_secs(1),
        ));
        ContentToolExecutor::new(
            client,
            media,
            AiMediaConfig::default(),
            5,
            Duration::from_secs(300),
            32,
        )
    }

    #[tokio::test]
    async fn rejects_unknown_tool_name() {
        let executor = test_executor();
        let call = ToolCall {
            id: "c1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({}),
            arguments_parse_error: None,
        };
        let result = executor.execute_tool_call(&call).await;
        assert!(
            result.content.contains("\"unknown_tool\""),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_url_surfaces_arguments_parse_error() {
        let executor = test_executor();
        let call = ToolCall {
            id: "c1".into(),
            name: "read_url".into(),
            arguments: serde_json::Value::Null,
            arguments_parse_error: Some(ToolArgsError::Provider {
                error: "expected value".into(),
                raw: "{bad".into(),
            }),
        };
        let result = executor.execute_tool_call(&call).await;
        assert!(
            result.content.contains("\"invalid_arguments_json\""),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_url_returns_fetch_failed_for_unreachable_host() {
        let executor = test_executor();
        let call = ToolCall {
            id: "c1".into(),
            name: "read_url".into(),
            arguments: serde_json::json!({ "url": "http://127.0.0.1:1/nope" }),
            arguments_parse_error: None,
        };
        let result = executor.execute_tool_call(&call).await;
        assert!(
            result.content.contains("\"fetch_blocked\"")
                || result.content.contains("\"fetch_failed\""),
            "{}",
            result.content
        );
    }
}
