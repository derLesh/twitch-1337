//! JSON API client for [doeneratlas.de](https://doeneratlas.de/) (`/app-api/public/…`).
//!
//! Used endpoints: `GET .../stats` (national average / headline stats) and, for manual
//! exploration, `GET .../search?q=` (city + shop hits; chat uses [`crate::commands::doener::DoenerCommand`] / `!dpi` for city look-ups instead).

use std::time::Duration;

use eyre::{Result, WrapErr as _};
use serde::Deserialize;

use crate::APP_USER_AGENT;

const DEFAULT_BASE_URL: &str = "https://doeneratlas.de";
const TIMEOUT: Duration = Duration::from_secs(10);

/// Response shape for `GET /app-api/public/stats`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AtlasPublicStats {
    pub national_average: f64,
    pub total_cities: u32,
    pub total_shops: u32,
    pub total_reports: u32,
    #[serde(default)]
    pub change_30d: Option<f64>,
    pub mode_price: u32,
}

pub struct DoeneratlasClient {
    http: reqwest::Client,
    base_url: String,
}

impl DoeneratlasClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .wrap_err("build doeneratlas HTTP client")?;
        Ok(Self {
            http,
            base_url: DEFAULT_BASE_URL.to_string(),
        })
    }

    /// Test hook: custom base URL (e.g. wiremock).
    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    /// Loads [`AtlasPublicStats`] including the Deutschland-Live-Ø (`national_average`).
    pub async fn stats(&self) -> Result<AtlasPublicStats> {
        let url = format!(
            "{}/app-api/public/stats",
            self.base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("doeneratlas stats: request failed")?
            .error_for_status()
            .wrap_err("doeneratlas stats: non-2xx")?;
        resp.json::<AtlasPublicStats>()
            .await
            .wrap_err("doeneratlas stats: parse JSON")
    }

    /// Convenience for `!döner <Betrag>` — same field the homepage hero shows.
    pub async fn national_average_eur(&self) -> Result<f64> {
        let s = self.stats().await?;
        Ok(s.national_average)
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client(server: &MockServer) -> DoeneratlasClient {
        crate::install_crypto_provider();
        DoeneratlasClient::with_base_url(reqwest::Client::new(), server.uri())
    }

    #[tokio::test]
    async fn stats_parses_canonical_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app-api/public/stats"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"national_average":8.36,"total_cities":1072,"total_shops":1897,"total_reports":3514,"change_30d":1.7,"mode_price":7}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = client(&server);
        let s = c.stats().await.expect("stats ok");
        assert!((s.national_average - 8.36).abs() < 1e-9);
        assert_eq!(s.total_cities, 1072);
    }

    #[tokio::test]
    async fn stats_error_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app-api/public/stats"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        assert!(client(&server).stats().await.is_err());
    }
}
