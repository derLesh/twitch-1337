//! HTTP client for ADS-B aggregators (adsb.lol + fallbacks), adsbdb,
//! Nominatim, and Aviationstack.

use std::time::Duration;

use eyre::{Result, WrapErr as _};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::config::AviationstackConfig;
use crate::util::APP_USER_AGENT;

use super::location::{
    ResolvedLocation, airline_table, is_iata_flight_number, is_icao_flight_number,
};
use super::tracker::FlightIdentifier;
use super::types::{
    AdsbAircraftResponse, AdsbDbAirlineResponse, AdsbDbResponse, AviationstackFlightMetadata,
    AviationstackFlightsResponse, FlightRoute, NearbyAircraft,
};

const ADSBDB_BASE_URL: &str = "https://api.adsbdb.com/v0";
const ADSBLOL_BASE_URL: &str = "https://api.adsb.lol/v2";
const AIRPLANES_LIVE_BASE_URL: &str = "https://api.airplanes.live/v2";
const ADSBFI_BASE_URL: &str = "https://opendata.adsb.fi/api/v2";
const ADSBONE_BASE_URL: &str = "https://api.adsb.one/v2";
const NOMINATIM_BASE_URL: &str = "https://nominatim.openstreetmap.org";
const AIRLINE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const ADSB_AGGREGATOR_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(super) struct AdsbAggregator {
    name: &'static str,
    base_url: String,
    path_style: AdsbPathStyle,
}

impl AdsbAggregator {
    pub(super) fn readsb_v2(name: &'static str, base_url: String) -> Self {
        Self {
            name,
            base_url,
            path_style: AdsbPathStyle::ReadsbV2,
        }
    }

    pub(super) fn adsb_fi(base_url: String) -> Self {
        Self {
            name: "adsb.fi",
            base_url,
            path_style: AdsbPathStyle::AdsbFiOpenData,
        }
    }
}

#[derive(Clone, Copy)]
enum AdsbPathStyle {
    ReadsbV2,
    AdsbFiOpenData,
}

#[derive(Clone, Copy)]
pub(super) enum AdsbEndpoint<'a> {
    Point { lat: f64, lon: f64, radius_nm: u16 },
    Hex(&'a str),
    Callsign(&'a str),
}

impl AdsbEndpoint<'_> {
    fn label(&self) -> &'static str {
        match self {
            Self::Point { .. } => "point",
            Self::Hex(_) => "hex",
            Self::Callsign(_) => "callsign",
        }
    }

    pub(super) fn url(&self, aggregator: &AdsbAggregator) -> String {
        let base = aggregator.base_url.trim_end_matches('/');
        match (aggregator.path_style, self) {
            (
                AdsbPathStyle::ReadsbV2,
                Self::Point {
                    lat,
                    lon,
                    radius_nm,
                },
            ) => format!("{base}/point/{lat}/{lon}/{radius_nm}"),
            (AdsbPathStyle::ReadsbV2, Self::Hex(hex)) => format!("{base}/hex/{hex}"),
            (AdsbPathStyle::ReadsbV2, Self::Callsign(callsign)) => {
                format!("{base}/callsign/{callsign}")
            }
            (
                AdsbPathStyle::AdsbFiOpenData,
                Self::Point {
                    lat,
                    lon,
                    radius_nm,
                },
            ) => format!("{base}/lat/{lat}/lon/{lon}/dist/{radius_nm}"),
            (AdsbPathStyle::AdsbFiOpenData, Self::Hex(hex)) => {
                format!("{base}/hex/{hex}")
            }
            (AdsbPathStyle::AdsbFiOpenData, Self::Callsign(callsign)) => {
                format!("{base}/callsign/{callsign}")
            }
        }
    }
}

enum AdsbFetchError {
    Retryable(eyre::Report),
    Fatal(eyre::Report),
}

#[derive(Debug, Deserialize)]
struct NominatimResult {
    lat: String,
    lon: String,
    display_name: String,
}

#[derive(Clone)]
pub struct AviationClient {
    http: reqwest::Client,
    adsb_aggregators: Vec<AdsbAggregator>,
    adsb_aggregator_timeout: Duration,
    adsbdb_base_url: String,
    nominatim_base_url: String,
    aviationstack: Option<AviationstackConfig>,
}

impl AviationClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .wrap_err("Failed to build aviation HTTP client")?;
        Ok(Self::new_with_adsb_aggregators(
            Self::default_adsb_aggregators(),
            ADSBDB_BASE_URL.to_owned(),
            NOMINATIM_BASE_URL.to_owned(),
            http,
        ))
    }

    pub fn new_with_base_url(
        adsb_base_url: String,
        adsbdb_base_url: String,
        nominatim_base_url: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self::new_with_adsb_aggregators(
            vec![AdsbAggregator::readsb_v2("adsb.lol", adsb_base_url)],
            adsbdb_base_url,
            nominatim_base_url,
            http_client,
        )
    }

    pub(super) fn new_with_adsb_aggregators(
        adsb_aggregators: Vec<AdsbAggregator>,
        adsbdb_base_url: String,
        nominatim_base_url: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            http: http_client,
            adsb_aggregators,
            adsb_aggregator_timeout: ADSB_AGGREGATOR_TIMEOUT,
            adsbdb_base_url,
            nominatim_base_url,
            aviationstack: None,
        }
    }

    fn default_adsb_aggregators() -> Vec<AdsbAggregator> {
        vec![
            AdsbAggregator::readsb_v2("adsb.lol", ADSBLOL_BASE_URL.to_owned()),
            AdsbAggregator::readsb_v2("airplanes.live", AIRPLANES_LIVE_BASE_URL.to_owned()),
            AdsbAggregator::adsb_fi(ADSBFI_BASE_URL.to_owned()),
            AdsbAggregator::readsb_v2("ADSB.One", ADSBONE_BASE_URL.to_owned()),
        ]
    }

    pub fn with_aviationstack_config(mut self, aviationstack: Option<AviationstackConfig>) -> Self {
        self.aviationstack = aviationstack.filter(|cfg| cfg.enabled);
        self
    }

    #[cfg(test)]
    fn with_adsb_aggregator_timeout(mut self, timeout: Duration) -> Self {
        self.adsb_aggregator_timeout = timeout;
        self
    }

    pub fn aviationstack_enabled(&self) -> bool {
        self.aviationstack.is_some()
    }

    async fn fetch_adsb_response(
        &self,
        endpoint: AdsbEndpoint<'_>,
    ) -> Result<AdsbAircraftResponse> {
        let mut last_retryable_error = None;

        for aggregator in &self.adsb_aggregators {
            let url = endpoint.url(aggregator);
            debug!(
                provider = aggregator.name,
                endpoint = endpoint.label(),
                url = %url,
                "Fetching aircraft from ADS-B aggregator"
            );

            match self.fetch_adsb_response_once(aggregator, &url).await {
                Ok(resp) => {
                    debug!(
                        provider = aggregator.name,
                        count = resp.aircraft.len(),
                        "Received aircraft from ADS-B aggregator"
                    );
                    return Ok(resp);
                }
                Err(AdsbFetchError::Retryable(error)) => {
                    warn!(
                        provider = aggregator.name,
                        error = ?error,
                        "ADS-B aggregator failed; trying fallback"
                    );
                    last_retryable_error = Some(error);
                }
                Err(AdsbFetchError::Fatal(error)) => return Err(error),
            }
        }

        match last_retryable_error {
            Some(error) => Err(error.wrap_err("All ADS-B aggregators failed")),
            None => Err(eyre::eyre!("No ADS-B aggregators configured")),
        }
    }

    async fn fetch_adsb_response_once(
        &self,
        aggregator: &AdsbAggregator,
        url: &str,
    ) -> std::result::Result<AdsbAircraftResponse, AdsbFetchError> {
        let resp = self
            .http
            .get(url)
            .timeout(self.adsb_aggregator_timeout)
            .send()
            .await
            .map_err(|e| {
                let error = eyre::eyre!("Failed to send request to {}: {e}", aggregator.name);
                if e.is_timeout() || e.is_connect() {
                    AdsbFetchError::Retryable(error)
                } else {
                    AdsbFetchError::Fatal(error)
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(AdsbFetchError::Retryable(eyre::eyre!(
                "{} returned {status}",
                aggregator.name
            )));
        }

        resp.json().await.map_err(|e| {
            AdsbFetchError::Retryable(eyre::eyre!(
                "Failed to parse {} response: {e}",
                aggregator.name
            ))
        })
    }

    pub(super) async fn get_aircraft_nearby(
        &self,
        lat: f64,
        lon: f64,
        radius_nm: u16,
    ) -> Result<Vec<NearbyAircraft>> {
        let resp = self
            .fetch_adsb_response(AdsbEndpoint::Point {
                lat,
                lon,
                radius_nm,
            })
            .await?;
        Ok(resp.aircraft)
    }

    pub async fn get_aircraft_by_hex(&self, hex: &str) -> Result<Option<NearbyAircraft>> {
        debug!(hex = %hex, "Fetching aircraft by hex from ADS-B aggregators");

        let resp = self.fetch_adsb_response(AdsbEndpoint::Hex(hex)).await?;

        Ok(resp.aircraft.into_iter().next())
    }

    pub async fn get_aircraft_by_callsign(&self, callsign: &str) -> Result<Option<NearbyAircraft>> {
        debug!(callsign = %callsign, "Fetching aircraft by callsign from ADS-B aggregators");

        let resp = self
            .fetch_adsb_response(AdsbEndpoint::Callsign(callsign))
            .await?;

        Ok(resp.aircraft.into_iter().next())
    }

    pub async fn get_flight_route(&self, callsign: &str) -> Result<Option<FlightRoute>> {
        let url = format!("{}/callsign/{callsign}", self.adsbdb_base_url);
        debug!(callsign = %callsign, "Fetching flight route from adsbdb");

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb response")?;

        Ok(body.response.flightroute)
    }

    pub async fn get_aviationstack_flight_metadata(
        &self,
        identifier: &FlightIdentifier,
        callsign: Option<&str>,
    ) -> Result<Option<AviationstackFlightMetadata>> {
        let Some(config) = &self.aviationstack else {
            return Ok(None);
        };
        let Some((query_key, query_value)) = aviationstack_query(identifier, callsign) else {
            debug!(identifier = %identifier, "Skipping aviationstack lookup: no callsign query");
            return Ok(None);
        };

        let url = format!("{}/flights", config.base_url.trim_end_matches('/'));
        debug!(
            query_key,
            query_value = %query_value,
            "Fetching flight metadata from aviationstack"
        );

        let timeout = Duration::from_secs(config.timeout_secs);
        let resp: AviationstackFlightsResponse = self
            .http
            .get(&url)
            .query(&[
                ("access_key", config.api_key.expose_secret()),
                (query_key, query_value.as_str()),
                ("limit", "1"),
            ])
            .timeout(timeout)
            .send()
            .await
            .wrap_err("Failed to send request to aviationstack")?
            .error_for_status()
            .wrap_err("aviationstack returned error status")?
            .json()
            .await
            .wrap_err("Failed to parse aviationstack response")?;

        Ok(resp
            .data
            .into_iter()
            .next()
            .map(AviationstackFlightMetadata::from))
    }

    /// Resolve a potential IATA flight number to an ICAO callsign.
    pub async fn resolve_callsign(&self, input: &str) -> String {
        if !is_iata_flight_number(input) {
            return input.to_string();
        }

        let (airline_iata, flight_num) = input.split_at(2);

        // Try static CSV lookup first
        if let Some(&icao) = airline_table().get(airline_iata) {
            debug!(iata = %airline_iata, icao = %icao, "Resolved airline code via CSV");
            return format!("{icao}{flight_num}");
        }

        // Fallback: query adsbdb airline API
        debug!(iata = %airline_iata, "Airline not in CSV, trying adsbdb API");
        match tokio::time::timeout(
            AIRLINE_LOOKUP_TIMEOUT,
            self.lookup_airline_icao(airline_iata),
        )
        .await
        {
            Ok(Ok(Some(icao))) => {
                warn!(
                    iata = %airline_iata,
                    icao = %icao,
                    "Resolved airline via adsbdb API — consider adding to airlines.csv"
                );
                format!("{icao}{flight_num}")
            }
            Ok(Ok(None)) => {
                debug!(iata = %airline_iata, "Airline not found in adsbdb");
                input.to_string()
            }
            Ok(Err(e)) => {
                warn!(error = ?e, iata = %airline_iata, "adsbdb airline lookup failed");
                input.to_string()
            }
            Err(_) => {
                warn!(iata = %airline_iata, "adsbdb airline lookup timed out");
                input.to_string()
            }
        }
    }

    /// Query adsbdb for an airline's ICAO code by IATA code.
    async fn lookup_airline_icao(&self, iata: &str) -> Result<Option<String>> {
        let url = format!("{}/airline/{iata}", self.adsbdb_base_url);
        debug!(url = %url, "Fetching airline from adsbdb");

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbAirlineResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb airline response")?;

        Ok(body.response.into_iter().next().map(|a| a.icao))
    }

    pub(super) async fn geocode_nominatim(&self, query: &str) -> Result<Option<ResolvedLocation>> {
        let url = format!("{}/search", self.nominatim_base_url);
        debug!(query = %query, "Geocoding via Nominatim");

        let resp = self
            .http
            .get(&url)
            .query(&[("q", query), ("format", "json"), ("limit", "1")])
            .send()
            .await
            .wrap_err("Failed to send request to Nominatim")?
            .error_for_status()
            .wrap_err("Nominatim returned error status")?;

        let results: Vec<NominatimResult> = resp
            .json()
            .await
            .wrap_err("Failed to parse Nominatim response")?;

        let Some(first) = results.into_iter().next() else {
            debug!(query = %query, "Nominatim returned no results");
            return Ok(None);
        };

        let lat: f64 = first.lat.parse().wrap_err("Invalid lat from Nominatim")?;
        let lon: f64 = first.lon.parse().wrap_err("Invalid lon from Nominatim")?;

        // Trim display_name to first comma-separated segment
        let display_name = first
            .display_name
            .split(',')
            .next()
            .unwrap_or(&first.display_name)
            .trim()
            .to_string();

        debug!(query = %query, lat = %lat, lon = %lon, display = %display_name, "Nominatim resolved");
        Ok(Some(ResolvedLocation {
            lat,
            lon,
            display_name,
        }))
    }
}

pub(super) fn aviationstack_query(
    identifier: &FlightIdentifier,
    callsign: Option<&str>,
) -> Option<(&'static str, String)> {
    let candidate = match identifier {
        FlightIdentifier::Callsign(value) => value.as_str(),
        FlightIdentifier::Hex(_) => callsign?,
    }
    .trim();

    if candidate.is_empty() {
        return None;
    }

    let candidate = candidate.to_uppercase();
    if is_iata_flight_number(&candidate) {
        Some(("flight_iata", candidate))
    } else if is_icao_flight_number(&candidate)
        || !matches!(identifier, FlightIdentifier::Hex(_))
        || callsign.is_some()
    {
        Some(("flight_icao", candidate))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn aviation_client() -> AviationClient {
        crate::install_crypto_provider();
        AviationClient::new().unwrap()
    }

    fn test_client_with_aggregators(adsb_aggregators: Vec<AdsbAggregator>) -> AviationClient {
        crate::install_crypto_provider();
        AviationClient::new_with_adsb_aggregators(
            adsb_aggregators,
            "http://adsbdb.test".to_string(),
            "http://nominatim.test".to_string(),
            reqwest::Client::new(),
        )
    }

    fn aircraft_response(flight: &str) -> serde_json::Value {
        serde_json::json!({
            "ac": [{
                "hex": "3c6589",
                "flight": flight,
                "alt_baro": 35000,
                "lat": 50.0,
                "lon": 8.5
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })
    }

    #[tokio::test]
    async fn resolve_callsign_translates_iata() {
        let client = aviation_client();
        assert_eq!(client.resolve_callsign("TP247").await, "TAP247");
        assert_eq!(client.resolve_callsign("LH5765").await, "DLH5765");
    }

    #[tokio::test]
    async fn resolve_callsign_passes_through_icao() {
        let client = aviation_client();
        assert_eq!(client.resolve_callsign("TAP247").await, "TAP247");
        assert_eq!(client.resolve_callsign("DLH5765").await, "DLH5765");
    }

    #[tokio::test]
    async fn resolve_callsign_passes_through_hex() {
        let client = aviation_client();
        assert_eq!(client.resolve_callsign("4CA87D").await, "4CA87D");
    }

    #[tokio::test]
    async fn adsb_falls_back_from_5xx_to_next_aggregator() {
        let (primary, backup) = tokio::join!(MockServer::start(), MockServer::start());

        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&primary)
            .await;

        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("DLH1234")))
            .mount(&backup)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("primary", primary.uri()),
            AdsbAggregator::readsb_v2("backup", backup.uri()),
        ]);

        let aircraft = client
            .get_aircraft_by_hex("3c6589")
            .await
            .expect("lookup succeeds through fallback")
            .expect("aircraft returned");

        assert_eq!(aircraft.flight.as_deref(), Some("DLH1234"));
        assert_eq!(primary.received_requests().await.unwrap().len(), 1);
        assert_eq!(backup.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn adsb_falls_back_from_timeout_to_next_aggregator() {
        let (primary, backup) = tokio::join!(MockServer::start(), MockServer::start());

        Mock::given(method("GET"))
            .and(path("/callsign/DLH1234"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(500))
                    .set_body_json(aircraft_response("DLH1234")),
            )
            .mount(&primary)
            .await;

        Mock::given(method("GET"))
            .and(path("/callsign/DLH1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("DLH1234")))
            .mount(&backup)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("primary", primary.uri()),
            AdsbAggregator::readsb_v2("backup", backup.uri()),
        ])
        .with_adsb_aggregator_timeout(Duration::from_millis(50));

        let aircraft = client
            .get_aircraft_by_callsign("DLH1234")
            .await
            .expect("lookup succeeds through fallback")
            .expect("aircraft returned");

        assert_eq!(aircraft.flight.as_deref(), Some("DLH1234"));
        assert_eq!(primary.received_requests().await.unwrap().len(), 1);
        assert_eq!(backup.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn adsb_fi_point_url_uses_open_data_path() {
        let aggregator = AdsbAggregator::adsb_fi("https://opendata.adsb.fi/api/v2/".to_string());
        let url = AdsbEndpoint::Point {
            lat: 1.5,
            lon: 2.25,
            radius_nm: 15,
        }
        .url(&aggregator);

        assert_eq!(
            url,
            "https://opendata.adsb.fi/api/v2/lat/1.5/lon/2.25/dist/15"
        );
    }

    #[test]
    fn adsb_response_accepts_aircraft_alias() {
        let response: AdsbAircraftResponse = serde_json::from_value(serde_json::json!({
            "aircraft": [{
                "hex": "3c6589",
                "flight": "DLH1234",
                "alt_baro": 35000
            }]
        }))
        .unwrap();

        assert_eq!(response.aircraft.len(), 1);
        assert_eq!(response.aircraft[0].flight.as_deref(), Some("DLH1234"));
    }
}
