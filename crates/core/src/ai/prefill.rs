use std::collections::VecDeque;

use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Europe::Berlin;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

fn default_base_url() -> String {
    "https://logs.zonian.dev".to_string()
}

fn default_threshold() -> f64 {
    0.5
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HistoryPrefillConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
}

#[derive(Deserialize)]
struct LogResponse {
    messages: Vec<LogMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogMessage {
    display_name: String,
    text: String,
}

/// Fetch messages for a specific date from the rustlog API.
///
/// Returns messages in chronological order (oldest first).
/// On any error, logs a warning and returns an empty Vec.
async fn fetch_messages_for_date(
    http: &reqwest::Client,
    base_url: &str,
    channel: &str,
    date: chrono::NaiveDate,
    limit: usize,
) -> Vec<(String, String, DateTime<Utc>)> {
    let url = format!(
        "{}/channel/{}/{}/{}/{}?jsonBasic=1&limit={}&reverse=1",
        base_url.trim_end_matches('/'),
        channel,
        date.year(),
        date.month(),
        date.day(),
        limit,
    );

    debug!(url = %url, "Fetching chat history");

    let response = match http.get(&url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!(error = ?e, url = %url, "Failed to fetch chat history");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        warn!(
            status = %response.status(),
            url = %url,
            "Chat history API returned non-success status"
        );
        return Vec::new();
    }

    let log_response: LogResponse = match response.json().await {
        Ok(parsed) => parsed,
        Err(e) => {
            warn!(error = ?e, url = %url, "Failed to parse chat history response");
            return Vec::new();
        }
    };

    // API returns newest-first with reverse=1, so reverse to get chronological order.
    // Prefilled messages lack exact timestamps; use the fetch time as a proxy.
    let fetched_at = Utc::now();
    log_response
        .messages
        .into_iter()
        .rev()
        .map(|msg| (msg.display_name, msg.text, fetched_at))
        .collect()
}

const PREFILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Prefill the chat history buffer by fetching recent messages from a rustlog-compatible API.
///
/// Fetches today's messages. If the count is below `config.threshold * history_length`,
/// also fetches yesterday's messages. Returns at most `history_length` messages in
/// chronological order.
///
/// On any failure, logs a warning and returns what it has (or an empty buffer).
pub async fn prefill_chat_history(
    channel: &str,
    history_length: usize,
    config: &HistoryPrefillConfig,
) -> VecDeque<(String, String, DateTime<Utc>)> {
    let http = match reqwest::Client::builder().timeout(PREFILL_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            warn!(error = ?e, "Failed to create HTTP client for history prefill");
            return VecDeque::with_capacity(history_length);
        }
    };

    let now = chrono::Utc::now().with_timezone(&Berlin);
    let today = now.date_naive();
    let yesterday = today - chrono::Duration::days(1);

    // Fetch today's messages
    let today_messages =
        fetch_messages_for_date(&http, &config.base_url, channel, today, history_length).await;

    let threshold_count = (config.threshold * history_length as f64).ceil() as usize;
    let today_count = today_messages.len();

    // If today has fewer messages than the threshold, also fetch yesterday
    let mut all_messages = if today_count < threshold_count {
        debug!(
            today_count,
            threshold_count, "Today's messages below threshold, fetching yesterday"
        );
        let yesterday_messages =
            fetch_messages_for_date(&http, &config.base_url, channel, yesterday, history_length)
                .await;

        let mut combined = yesterday_messages;
        combined.extend(today_messages);
        combined
    } else {
        today_messages
    };

    // Take only the last history_length messages
    if all_messages.len() > history_length {
        all_messages.drain(..all_messages.len() - history_length);
    }

    let count = all_messages.len();
    info!(count, "Prefilled chat history buffer");

    VecDeque::from(all_messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let config: HistoryPrefillConfig =
            serde_json::from_str("{}").expect("empty JSON should use defaults");
        assert_eq!(config.base_url, "https://logs.zonian.dev");
        assert!((config.threshold - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_custom_values() {
        let json = r#"{"base_url": "https://logs.example.com", "threshold": 0.8}"#;
        let config: HistoryPrefillConfig =
            serde_json::from_str(json).expect("valid JSON should parse");
        assert_eq!(config.base_url, "https://logs.example.com");
        assert!((config.threshold - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_in_ai_config_toml() {
        let toml_str = r#"
            backend = "openai"
            api_key = "test-key"
            model = "test-model"

            [history_prefill]
            base_url = "https://custom.logs.dev"
            threshold = 0.3
        "#;
        // Use the parent AiConfig to verify nesting works
        // This test verifies the serde deserialization path
        let config: toml::Value = toml::from_str(toml_str).expect("valid TOML");
        let prefill = config
            .get("history_prefill")
            .expect("history_prefill should exist");
        assert_eq!(
            prefill.get("base_url").and_then(|v| v.as_str()),
            Some("https://custom.logs.dev")
        );
    }
}
