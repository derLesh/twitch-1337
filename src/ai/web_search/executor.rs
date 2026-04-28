use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Mutex;

use crate::ai::llm::{ToolCall, ToolResultMessage};

use super::cache::TtlCache;
use super::client::{SearchClient, SearchResult};

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
        ToolResultMessage {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content,
        }
    }

    async fn execute(&self, call: &ToolCall) -> String {
        if let Some(parse_err) = &call.arguments_parse_error {
            return json!({
                "error": "invalid_arguments_json",
                "tool": call.name,
                "details": parse_err.error,
                "raw": parse_err.raw,
            })
            .to_string();
        }

        match call.name.as_str() {
            "web_search" => self.execute_web_search(call).await,
            "fetch_url" => self.execute_fetch_url(call).await,
            other => json!({
                "error": "unknown_tool",
                "tool": other,
            })
            .to_string(),
        }
    }

    async fn execute_web_search(&self, call: &ToolCall) -> String {
        let Some(query) = call.arguments.get("query").and_then(|v| v.as_str()) else {
            return json!({
                "error": "invalid_arguments",
                "details": "web_search requires string field 'query'",
            })
            .to_string();
        };
        if query.trim().is_empty() {
            return json!({
                "error": "invalid_arguments",
                "details": "query cannot be empty",
            })
            .to_string();
        }

        let requested = call
            .arguments
            .get("max_results")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(self.max_results);
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
                    "search_timeout"
                } else {
                    "search_failed"
                };
                json!({
                    "error": error_code,
                    "details": err.to_string(),
                })
                .to_string()
            }
        }
    }

    async fn execute_fetch_url(&self, call: &ToolCall) -> String {
        let Some(url) = call.arguments.get("url").and_then(|v| v.as_str()) else {
            return json!({
                "error": "invalid_arguments",
                "details": "fetch_url requires string field 'url'",
            })
            .to_string();
        };

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
                    "fetch_blocked"
                } else if msg.contains("timed out") {
                    "fetch_timeout"
                } else {
                    "fetch_failed"
                };
                json!({
                    "error": error_code,
                    "details": err.to_string(),
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
    use super::*;
    use crate::ai::llm::ToolCallArgsError;

    fn test_executor() -> WebToolExecutor {
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
            arguments_parse_error: Some(ToolCallArgsError {
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
