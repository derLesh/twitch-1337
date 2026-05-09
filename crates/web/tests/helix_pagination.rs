use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::json;
use twitch_1337_web::helix::{AccessTokenProvider, HelixClient, ReqwestHelixClient};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct StubToken;

#[async_trait]
impl AccessTokenProvider for StubToken {
    async fn current_access_token(&self) -> eyre::Result<String> {
        Ok("test-token".into())
    }
}

fn install_crypto() {
    // Workspace pins reqwest = "rustls-no-provider"; tests need a provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::test]
async fn is_moderator_uses_user_id_filter() {
    install_crypto();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/helix/moderation/moderators"))
        .and(query_param("broadcaster_id", "100"))
        .and(query_param("user_id", "12345"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "user_id": "12345", "user_login": "alice", "user_name": "Alice" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = ReqwestHelixClient::with_base(
        reqwest::Client::new(),
        SecretString::new("client-id".to_owned().into()),
        Arc::new(StubToken),
        server.uri(),
    );
    assert!(client.is_moderator("100", "12345").await.unwrap());
}

#[tokio::test]
async fn is_moderator_returns_false_for_empty_data() {
    install_crypto();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/helix/moderation/moderators"))
        .and(query_param("broadcaster_id", "100"))
        .and(query_param("user_id", "999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let client = ReqwestHelixClient::with_base(
        reqwest::Client::new(),
        SecretString::new("client-id".to_owned().into()),
        Arc::new(StubToken),
        server.uri(),
    );
    assert!(!client.is_moderator("100", "999").await.unwrap());
}
