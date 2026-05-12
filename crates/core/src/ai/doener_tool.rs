use llm::{ToolCall, ToolDefinition, ToolResultMessage};
use serde::Deserialize;
use serde_json::json;

use crate::doener::DoenerClient;

pub const DOENER_TOOL_NAME: &str = "doener_index";

#[derive(Debug, Deserialize)]
pub(crate) struct DoenerArgs {
    #[serde(default)]
    pub city: Option<String>,
}

pub fn doener_tool() -> ToolDefinition {
    ToolDefinition {
        name: DOENER_TOOL_NAME.into(),
        description: "Look up the German Döner price index from dönerindex.com. \
            Without `city`, returns the country-wide aggregate (location count, \
            avg/min/max price). With `city` (free-form), returns the top matching \
            cities and their per-city aggregate. Use this for any question about \
            Döner prices, kebab prices, or how expensive Döner is in a German city."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "Optional city name or prefix. German spelling preferred (e.g. 'Köln', 'München')."
                }
            }
        }),
    }
}

pub async fn execute_doener_index(client: &DoenerClient, call: &ToolCall) -> ToolResultMessage {
    let args = match call.parse_args::<DoenerArgs>() {
        Ok(a) => a,
        Err(e) => {
            return ToolResultMessage::for_call(
                call,
                json!({
                    "error": "invalid_arguments",
                    "details": e.to_string(),
                })
                .to_string(),
            );
        }
    };

    let payload = match args
        .city
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => match client.stats().await {
            Ok(stats) => json!({"scope": "global", "stats": stats}),
            Err(e) => {
                tracing::warn!(error = ?e, "doener_index global lookup failed");
                json!({"error": "doener_index API unavailable"})
            }
        },
        Some(q) => match client.search_cities(q).await {
            Ok(hits) => {
                let top: Vec<_> = hits.into_iter().take(5).collect();
                json!({"scope": "city", "query": q, "hits": top})
            }
            Err(e) => {
                tracing::warn!(error = ?e, query = q, "doener_index city lookup failed");
                json!({"error": "doener_index API unavailable"})
            }
        },
    };

    ToolResultMessage::for_call(call, payload.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use llm::ToolResultMessage;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::doener::DoenerClient;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: DOENER_TOOL_NAME.into(),
            arguments: args,
            arguments_parse_error: None,
        }
    }

    fn test_client(server: &MockServer) -> DoenerClient {
        crate::install_crypto_provider();
        DoenerClient::with_base_url(reqwest::Client::new(), server.uri())
    }

    fn content(msg: &ToolResultMessage) -> String {
        // ToolResultMessage.content is a public String field — see crates/llm/src/types.rs:74.
        msg.content.clone()
    }

    #[test]
    fn tool_def_has_expected_name() {
        let t = doener_tool();
        assert_eq!(t.name, "doener_index");
    }

    #[test]
    fn doener_index_is_not_a_web_tool() {
        // Regression guard: a future refactor must not gate this tool behind
        // [ai.web]. is_web_tool drives routing into ContentToolExecutor.
        assert!(!crate::ai::content::is_web_tool(DOENER_TOOL_NAME));
    }

    #[tokio::test]
    async fn no_city_returns_global_scope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"total_locations":6092,"total_cities":2202,"min_price":5.5,"max_price":9,"avg_price":6.1,"locations_no_price":5304,"locations_no_price_pct":87.1}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({}))).await;
        let body = content(&msg);
        assert!(body.contains("\"scope\":\"global\""), "got: {body}");
        assert!(body.contains("\"total_locations\":6092"), "got: {body}");
    }

    #[tokio::test]
    async fn with_city_returns_city_scope_and_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .and(wiremock::matchers::query_param("q", "Han"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"Han","count":1,"cities":[{"city":"Hannover","zip":"30459","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"}]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({"city": "Han"}))).await;
        let body = content(&msg);
        assert!(body.contains("\"scope\":\"city\""), "got: {body}");
        assert!(body.contains("Hannover"), "got: {body}");
    }

    #[tokio::test]
    async fn zero_hits_returns_empty_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"zzz","count":0,"cities":[]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({"city": "zzz"}))).await;
        let body = content(&msg);
        assert!(body.contains("\"hits\":[]"), "got: {body}");
    }

    #[tokio::test]
    async fn upstream_failure_returns_error_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({}))).await;
        let body = content(&msg);
        assert!(
            body.contains("\"error\":\"doener_index API unavailable\""),
            "got: {body}"
        );
    }

    #[tokio::test]
    async fn malformed_args_returns_invalid_arguments_json() {
        // city as integer should fail to deserialize as Option<String>.
        let server = MockServer::start().await;
        // No mock needed; the args parse error short-circuits before any HTTP.
        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({"city": 123}))).await;
        let body = content(&msg);
        assert!(
            body.contains("\"error\":\"invalid_arguments\""),
            "got: {body}"
        );
    }
}
