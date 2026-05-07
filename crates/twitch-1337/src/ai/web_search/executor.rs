use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{error, warn};

use llm::{ToolCall, ToolResultMessage};

use super::cache::TtlCache;
use super::client::{SearchClient, SearchResult};

#[derive(Debug, Deserialize)]
struct WebSearchArgs {
    query: String,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct FetchUrlArgs {
    /// HTTP(S) URL to fetch.
    pub url: String,
}

const FETCH_RESULT_MAX_CHARS: usize = 4_000;

pub struct WebToolExecutor {
    client: SearchClient,
    max_results: usize,
    search_cache: Arc<Mutex<TtlCache<Vec<SearchResult>>>>,
    fetch_cache: Arc<Mutex<TtlCache<String>>>,
}

impl WebToolExecutor {
    pub fn new(
        client: SearchClient,
        max_results: usize,
        cache_ttl: Duration,
        cache_capacity: usize,
    ) -> Self {
        Self {
            client,
            max_results,
            search_cache: Arc::new(Mutex::new(TtlCache::new(cache_ttl, cache_capacity))),
            fetch_cache: Arc::new(Mutex::new(TtlCache::new(cache_ttl, cache_capacity))),
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
            "fetch_url" => match call.parse_args::<FetchUrlArgs>() {
                Ok(args) => self.execute_fetch_url(args).await,
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

    async fn execute_fetch_url(&self, args: FetchUrlArgs) -> String {
        let url = args.url.as_str();
        let key = normalize_url(url);
        if let Some(cached) = self.fetch_cache.lock().await.get(&key) {
            return json!({
                "cached": true,
                "url": url,
                "content": cached,
            })
            .to_string();
        }

        match self.client.fetch_url(url).await {
            Ok(content) => {
                let shortened = truncate_chars(&content, FETCH_RESULT_MAX_CHARS);
                self.fetch_cache.lock().await.insert(key, shortened.clone());
                json!({
                    "cached": false,
                    "url": url,
                    "content": shortened,
                })
                .to_string()
            }
            Err(err) => {
                let msg = err.to_string().to_ascii_lowercase();
                let error_code = if msg.contains("blocked") {
                    warn!(error = ?err, url, "fetch_url blocked");
                    "fetch_blocked"
                } else if msg.contains("timed out") {
                    warn!(error = ?err, url, "fetch_url timed out");
                    "fetch_timeout"
                } else {
                    error!(error = ?err, url, "fetch_url failed");
                    "fetch_failed"
                };
                json!({
                    "error": error_code,
                    "details": format!("{err:#}"),
                })
                .to_string()
            }
        }
    }
}

fn normalize_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_url(url: &str) -> String {
    url.trim().to_ascii_lowercase()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let len = value.chars().count();
    if len <= max_chars {
        return value.to_string();
    }
    let cutoff = value
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    format!("{}...", &value[..cutoff])
}

#[cfg(test)]
mod tests {
    use llm::ToolArgsError;

    use super::*;

    fn test_executor() -> WebToolExecutor {
        crate::install_crypto_provider();
        let client = SearchClient::new_with_client(
            "http://127.0.0.1:65535/search".to_string(),
            Duration::from_secs(1),
            reqwest::Client::new(),
        );
        WebToolExecutor::new(client, 5, Duration::from_secs(300), 32)
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
    async fn surfaces_arguments_parse_error() {
        let executor = test_executor();
        let call = ToolCall {
            id: "c1".into(),
            name: "web_search".into(),
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
}
