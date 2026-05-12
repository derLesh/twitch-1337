use std::time::Duration;

use eyre::{Result, WrapErr};

use crate::APP_USER_AGENT;
use crate::doener::types::{CitiesResponse, CityHit, GlobalStats};

const BASE_URL: &str = "https://xn--dnerindex-07a.com";
const TIMEOUT: Duration = Duration::from_secs(5);

pub struct DoenerClient {
    http: reqwest::Client,
    base_url: String,
}

impl DoenerClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .wrap_err("build doener HTTP client")?;
        Ok(Self {
            http,
            base_url: BASE_URL.to_string(),
        })
    }

    /// Test hook: inject an existing `reqwest::Client` (commonly with a short
    /// timeout) and a custom base URL pointing at a wiremock server.
    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    pub async fn stats(&self) -> Result<GlobalStats> {
        let url = format!("{}/api/stats.php", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("doener stats: request failed")?
            .error_for_status()
            .wrap_err("doener stats: non-2xx")?;
        resp.json::<GlobalStats>()
            .await
            .wrap_err("doener stats: parse JSON")
    }

    pub async fn search_cities(&self, q: &str) -> Result<Vec<CityHit>> {
        let url = format!("{}/api/cities.php", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", q)])
            .send()
            .await
            .wrap_err("doener cities: request failed")?
            .error_for_status()
            .wrap_err("doener cities: non-2xx")?;
        let body = resp
            .json::<CitiesResponse>()
            .await
            .wrap_err("doener cities: parse JSON")?;
        Ok(body.cities)
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn test_client(server: &MockServer) -> DoenerClient {
        crate::install_crypto_provider();
        DoenerClient::with_base_url(reqwest::Client::new(), server.uri())
    }

    fn test_client_with_timeout(server: &MockServer, timeout: std::time::Duration) -> DoenerClient {
        crate::install_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("build timeout test client");
        DoenerClient::with_base_url(http, server.uri())
    }

    #[tokio::test]
    async fn stats_parses_canonical_response() {
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
        let stats = client.stats().await.expect("stats ok");
        assert_eq!(stats.total_locations, 6092);
        assert_eq!(stats.total_cities, 2202);
    }

    #[tokio::test]
    async fn stats_returns_err_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = test_client(&server);
        assert!(client.stats().await.is_err());
    }

    #[tokio::test]
    async fn stats_returns_err_on_malformed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = test_client(&server);
        assert!(client.stats().await.is_err());
    }

    #[tokio::test]
    async fn stats_returns_err_on_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(b"{}".as_slice(), "application/json")
                    .set_delay(std::time::Duration::from_secs(2)),
            )
            .mount(&server)
            .await;

        let client = test_client_with_timeout(&server, std::time::Duration::from_millis(150));
        assert!(client.stats().await.is_err());
    }

    #[tokio::test]
    async fn search_cities_returns_hits_in_upstream_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .and(wiremock::matchers::query_param("q", "Han"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"Han","count":3,"cities":[
                    {"city":"Hannover","zip":"30459","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"},
                    {"city":"Hanau","zip":"63456","location_count":3,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"},
                    {"city":"Handewitt","zip":"24983","location_count":1,"min_price":null,"max_price":null,"avg_price":null}
                ]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let hits = client.search_cities("Han").await.expect("cities ok");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].city, "Hannover");
        assert_eq!(hits[2].avg_price, None);
    }

    #[tokio::test]
    async fn search_cities_empty_array_is_ok_empty() {
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
        let hits = client.search_cities("zzz").await.expect("ok");
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_cities_returns_err_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = test_client(&server);
        assert!(client.search_cities("x").await.is_err());
    }
}
