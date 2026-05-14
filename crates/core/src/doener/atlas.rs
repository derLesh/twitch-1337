//! JSON API client for [doeneratlas.de](https://doeneratlas.de/) (`/app-api/public/…`).
//!
//! Stadt-Ø / Min–Max für `!dpi` kommen von [`DoeneratlasClient::fetch_city_public_metrics`]
//! (`GET /app-api/public/cities?slug=…`), weil [`AtlasSearchResponse::shops`] nur eine kleine Stichprobe ist.

use std::time::Duration;

use eyre::{Result, WrapErr as _};
use serde::Deserialize;

use crate::APP_USER_AGENT;
use crate::doener::types::CityHit;

const DEFAULT_BASE_URL: &str = "https://doeneratlas.de";
const TIMEOUT: Duration = Duration::from_secs(10);

/// Stadt-Kennzahlen wie vom öffentlichen `cities`-Endpoint (`avg_price` / Min / Max / `shop_count`).
#[derive(Debug, Clone, PartialEq)]
pub struct CityPublicMetrics {
    pub listed_shops: u32,
    pub avg_price_eur: f64,
    pub min_price_eur: f64,
    pub max_price_eur: f64,
}

/// Response shape for `GET /app-api/public/stats`.
#[derive(Debug, Clone, Deserialize, PartialEq, serde::Serialize)]
pub struct AtlasPublicStats {
    pub national_average: f64,
    pub total_cities: u32,
    pub total_shops: u32,
    pub total_reports: u32,
    #[serde(default)]
    pub change_30d: Option<f64>,
    pub mode_price: u32,
}

/// `GET /app-api/public/search` — cities + shops (prices per shop).
#[derive(Debug, Clone, Deserialize)]
pub struct AtlasSearchResponse {
    pub cities: Vec<AtlasCityRow>,
    pub shops: Vec<AtlasShopRow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AtlasCityRow {
    #[allow(dead_code)]
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub shop_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AtlasShopRow {
    pub city_slug: String,
    #[serde(default)]
    pub city_name: String,
    pub current_price: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AtlasCityDetail {
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub slug: String,
    pub shop_count: u32,
    #[serde(default)]
    pub avg_price: Option<String>,
    #[serde(default)]
    pub min_price: Option<String>,
    #[serde(default)]
    pub max_price: Option<String>,
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

    fn base_trimmed(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    /// Loads [`AtlasPublicStats`] including the Deutschland-Live-Ø (`national_average`).
    pub async fn stats(&self) -> Result<AtlasPublicStats> {
        let url = format!("{}/app-api/public/stats", self.base_trimmed());
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

    /// Convenience for `!döner <Betrag>`.
    pub async fn national_average_eur(&self) -> Result<f64> {
        let s = self.stats().await?;
        Ok(s.national_average)
    }

    pub async fn search(&self, q: &str) -> Result<AtlasSearchResponse> {
        let url = format!("{}/app-api/public/search", self.base_trimmed());
        let resp = self
            .http
            .get(&url)
            .query(&[("q", q)])
            .send()
            .await
            .wrap_err("doeneratlas search: request failed")?
            .error_for_status()
            .wrap_err("doeneratlas search: non-2xx")?;
        resp.json::<AtlasSearchResponse>()
            .await
            .wrap_err("doeneratlas search: parse JSON")
    }

    /// City rows from prefix search; Preisfelder sind bis zu [`Self::enrich_city_hit`] nur aus der
    /// Such-Stichprobe — oft unvollständig gegenüber [`AtlasCityRow::shop_count`].
    pub async fn search_city_hits(&self, q: &str) -> Result<Vec<CityHit>> {
        let body = self.search(q).await?;
        Ok(city_hits_from_search(&body))
    }

    /// `GET /app-api/public/cities?slug=…` — volle Stadt-Aggregate (Ø / Min / Max / Anzahl Läden).
    pub async fn fetch_city_public_metrics(&self, slug: &str) -> Result<CityPublicMetrics> {
        let url = format!("{}/app-api/public/cities", self.base_trimmed());
        let resp = self
            .http
            .get(&url)
            .query(&[("slug", slug)])
            .send()
            .await
            .wrap_err("doeneratlas cities: request failed")?
            .error_for_status()
            .wrap_err("doeneratlas cities: non-2xx")?;
        let row = resp
            .json::<AtlasCityDetail>()
            .await
            .wrap_err("doeneratlas cities: parse JSON")?;
        metrics_from_city_detail(row).wrap_err("doeneratlas cities: incomplete price fields")
    }

    /// Ersetzt Such-Stichproben-Preise durch [`Self::fetch_city_public_metrics`] wenn möglich.
    pub async fn enrich_city_hit(&self, hit: CityHit) -> CityHit {
        if hit.slug.is_empty() {
            return strip_unreliable_city_prices(hit);
        }
        match self.fetch_city_public_metrics(&hit.slug).await {
            Ok(m) => CityHit {
                city: hit.city,
                slug: hit.slug,
                location_count: m.listed_shops,
                priced_shop_sample: hit.priced_shop_sample,
                avg_price: Some(m.avg_price_eur),
                min_price: Some(m.min_price_eur),
                max_price: Some(m.max_price_eur),
            },
            Err(_) => strip_unreliable_city_prices(hit),
        }
    }
}

fn metrics_from_city_detail(row: AtlasCityDetail) -> Result<CityPublicMetrics> {
    let avg_price_eur = row
        .avg_price
        .as_deref()
        .and_then(parse_price_str)
        .ok_or_else(|| eyre::eyre!("avg_price missing or invalid"))?;
    let min_price_eur = row
        .min_price
        .as_deref()
        .and_then(parse_price_str)
        .ok_or_else(|| eyre::eyre!("min_price missing or invalid"))?;
    let max_price_eur = row
        .max_price
        .as_deref()
        .and_then(parse_price_str)
        .ok_or_else(|| eyre::eyre!("max_price missing or invalid"))?;
    Ok(CityPublicMetrics {
        listed_shops: row.shop_count,
        avg_price_eur,
        min_price_eur,
        max_price_eur,
    })
}

fn strip_unreliable_city_prices(hit: CityHit) -> CityHit {
    if hit.priced_shop_sample > 0 && hit.priced_shop_sample == hit.location_count {
        hit
    } else {
        CityHit {
            avg_price: None,
            min_price: None,
            max_price: None,
            ..hit
        }
    }
}

fn parse_price_str(raw: &str) -> Option<f64> {
    raw.trim().parse::<f64>().ok()
}

fn city_hits_from_search(body: &AtlasSearchResponse) -> Vec<CityHit> {
    body.cities
        .iter()
        .map(|c| {
            let prices: Vec<f64> = body
                .shops
                .iter()
                .filter(|s| s.city_slug == c.slug)
                .filter_map(|s| parse_price_str(&s.current_price))
                .collect();
            let priced_shop_sample = prices.len() as u32;
            let (min_price, max_price, avg_price) = if prices.is_empty() {
                (None, None, None)
            } else {
                let min = prices.iter().copied().fold(f64::INFINITY, f64::min);
                let max = prices.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let sum: f64 = prices.iter().sum();
                let avg = sum / prices.len() as f64;
                (Some(min), Some(max), Some(avg))
            };
            CityHit {
                city: c.name.clone(),
                slug: c.slug.clone(),
                location_count: c.shop_count,
                priced_shop_sample,
                min_price,
                max_price,
                avg_price,
            }
        })
        .collect()
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

    #[tokio::test]
    async fn search_city_hits_aggregates_prices_by_slug() {
        let json = br#"{"cities":[{"id":1,"name":"Darmstadt","slug":"darmstadt","state":"Hessen","shop_count":2}],"shops":[{"city_slug":"darmstadt","city_name":"Darmstadt","current_price":"4.00"},{"city_slug":"darmstadt","city_name":"Darmstadt","current_price":"6.00"}]}"#;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app-api/public/search"))
            .and(wiremock::matchers::query_param("q", "darm"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(json.as_slice(), "application/json"),
            )
            .mount(&server)
            .await;

        let hits = client(&server)
            .search_city_hits("darm")
            .await
            .expect("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].city, "Darmstadt");
        assert_eq!(hits[0].slug, "darmstadt");
        assert_eq!(hits[0].priced_shop_sample, 2);
        assert_eq!(hits[0].location_count, 2);
        assert!((hits[0].min_price.unwrap() - 4.0).abs() < 1e-9);
        assert!((hits[0].max_price.unwrap() - 6.0).abs() < 1e-9);
        assert!((hits[0].avg_price.unwrap() - 5.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn fetch_city_public_metrics_reads_cities_slug_endpoint() {
        let server = MockServer::start().await;
        let json = br#"{"name":"Berlin","slug":"berlin","shop_count":127,"avg_price":"5.49","min_price":"2.50","max_price":"37.00"}"#;
        Mock::given(method("GET"))
            .and(path("/app-api/public/cities"))
            .and(wiremock::matchers::query_param("slug", "berlin"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(json.as_slice(), "application/json"),
            )
            .mount(&server)
            .await;

        let m = client(&server)
            .fetch_city_public_metrics("berlin")
            .await
            .expect("metrics");
        assert_eq!(
            m,
            CityPublicMetrics {
                listed_shops: 127,
                avg_price_eur: 5.49,
                min_price_eur: 2.5,
                max_price_eur: 37.0,
            }
        );
    }
}
