//! Multimodal sub-agent client for `read_url`. Posts an OpenAI-compatible
//! chat completion with content parts; returns the assistant's text answer.
//!
//! Why isolated here: the multimodal request schema (content parts, image_url
//! data URLs) is provider-coupled. Keeping it out of the shared `llm` trait
//! avoids forcing every provider implementation to grow content-part variants
//! for a feature only this consumer needs.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use eyre::{Result, WrapErr as _, bail};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};

use crate::ai::content::client::Payload;

const SYSTEM_PROMPT: &str = "You analyze URLs on behalf of a Twitch chat bot. \
Answer the user's instruction strictly from the provided content. Be concise. \
If the instruction is empty, describe the contents.";

pub struct MediaClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<SecretString>,
    model: String,
    timeout: Duration,
}

impl MediaClient {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        api_key: Option<SecretString>,
        model: String,
        timeout: Duration,
    ) -> Self {
        Self {
            http,
            base_url,
            api_key,
            model,
            timeout,
        }
    }

    pub async fn analyze(
        &self,
        content_type: &str,
        payload: &Payload,
        instruction: Option<&str>,
    ) -> Result<String> {
        let body = self.build_request(content_type, payload, instruction);

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut req = self.http.post(&url).timeout(self.timeout).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key.expose_secret());
        }
        let response = req
            .send()
            .await
            .wrap_err("media chat-completions request failed")?
            .error_for_status()
            .wrap_err("media chat-completions returned error status")?;

        let value: Value = response
            .json()
            .await
            .wrap_err("media chat-completions response was not JSON")?;

        let answer = value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(str::to_string);

        match answer {
            Some(s) if !s.trim().is_empty() => Ok(s),
            _ => bail!("media response missing assistant content"),
        }
    }

    fn build_request(
        &self,
        content_type: &str,
        payload: &Payload,
        instruction: Option<&str>,
    ) -> Value {
        let prompt = instruction
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Describe the contents.");

        let media_part = match payload {
            Payload::Text(t) => json!({ "type": "text", "text": t }),
            Payload::Bytes(b) => {
                let prefix = "data:";
                let mid = ";base64,";
                let mut data_url = String::with_capacity(
                    prefix.len() + content_type.len() + mid.len() + (b.len() * 4 / 3) + 4,
                );
                data_url.push_str(prefix);
                data_url.push_str(content_type);
                data_url.push_str(mid);
                BASE64.encode_string(b, &mut data_url);
                json!({ "type": "image_url", "image_url": { "url": data_url } })
            }
        };

        json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": prompt },
                        media_part,
                    ],
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> MediaClient {
        crate::install_crypto_provider();
        MediaClient::new(
            reqwest::Client::new(),
            "https://example.invalid/v1".to_string(),
            None,
            "test-model".to_string(),
            Duration::from_secs(1),
        )
    }

    #[test]
    fn build_request_image_uses_data_url() {
        let c = client();
        let req = c.build_request(
            "image/png",
            &Payload::Bytes(vec![1, 2, 3]),
            Some("what is shown?"),
        );
        let parts = &req["messages"][1]["content"];
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is shown?");
        assert_eq!(parts[1]["type"], "image_url");
        let url = parts[1]["image_url"]["url"].as_str().expect("url");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
    }

    #[test]
    fn build_request_text_uses_inline_text_part() {
        let c = client();
        let req = c.build_request("text/html", &Payload::Text("Hello".into()), None);
        let parts = &req["messages"][1]["content"];
        assert_eq!(parts[0]["text"], "Describe the contents.");
        assert_eq!(parts[1]["type"], "text");
        assert_eq!(parts[1]["text"], "Hello");
    }

    #[test]
    fn build_request_includes_system_prompt() {
        let c = client();
        let req = c.build_request("text/plain", &Payload::Text("x".into()), None);
        assert_eq!(req["messages"][0]["role"], "system");
        assert!(
            req["messages"][0]["content"]
                .as_str()
                .expect("system content")
                .contains("Twitch chat bot"),
        );
    }

    #[tokio::test]
    async fn analyze_extracts_choices_message_content() {
        crate::install_crypto_provider();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "It is a cat." }
                    }]
                })),
            )
            .mount(&server)
            .await;

        let c = MediaClient::new(
            reqwest::Client::new(),
            format!("{}/v1", server.uri()),
            None,
            "x".into(),
            Duration::from_secs(2),
        );
        let answer = c
            .analyze("image/png", &Payload::Bytes(vec![1, 2]), Some("what?"))
            .await
            .expect("ok");
        assert_eq!(answer, "It is a cat.");
    }

    #[tokio::test]
    async fn analyze_surfaces_provider_error() {
        crate::install_crypto_provider();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let c = MediaClient::new(
            reqwest::Client::new(),
            format!("{}/v1", server.uri()),
            None,
            "x".into(),
            Duration::from_secs(2),
        );
        let err = c
            .analyze("text/plain", &Payload::Text("x".into()), None)
            .await
            .expect_err("err");
        assert!(
            err.to_string().to_lowercase().contains("error status"),
            "{err}"
        );
    }
}
