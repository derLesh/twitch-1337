use twitch_1337_core as twitch_1337;
mod common;

use std::time::Duration;

use chrono::Duration as ChronoDuration;
use common::TestBotBuilder;
use secrecy::SecretString;
use serial_test::serial;
use twitch_1337::config::AviationstackConfig;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, ResponseTemplate};

fn enable_aviationstack(config: &mut twitch_1337::config::Configuration) {
    config.aviationstack = Some(AviationstackConfig {
        enabled: true,
        api_key: SecretString::new("test-key".into()),
        base_url: "https://api.aviationstack.com/v1".to_string(),
        timeout_secs: 5,
    });
}

#[tokio::test]
#[serial]
async fn track_command_acknowledges_flight() {
    let bot = TestBotBuilder::new().spawn().await;

    // Stub every live ADS-B / adsbdb route the tracker might hit.
    // Both base URLs point to the same mock server in tests.
    Mock::given(method("GET"))
        .and(path_regex(r"^/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3c6589",
                "flight": "DLH1234",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 50.0,
                "lon": 8.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    // Expected ack: "Tracke DLH1234 Okayge" (plus route info if adsbdb responded).
    // Accept any ack that references the callsign or "track".
    assert!(
        ack.contains("DLH1234") || ack.to_lowercase().contains("track"),
        "expected track ack, got: {ack}"
    );
    let aviationstack_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/flights")
        .count();
    assert_eq!(aviationstack_requests, 0);

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn track_command_enriches_flight_from_aviationstack_once() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/callsign/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3c6589",
                "flight": "DLH1234",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 50.0,
                "lon": 8.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1234"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": {
                    "iata": "LH1234",
                    "icao": "DLH1234",
                    "number": "1234"
                },
                "airline": {
                    "iata": "LH",
                    "icao": "DLH",
                    "name": "Lufthansa"
                },
                "departure": {
                    "iata": "FRA",
                    "icao": "EDDF",
                    "scheduled": "2026-04-18T09:45:00+00:00",
                    "actual": "2026-04-18T10:00:00+00:00",
                    "actual_runway": "2026-04-18T10:05:00+00:00"
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T11:00:00+00:00",
                    "actual": null
                },
                "aircraft": {
                    "icao24": "3c6589",
                    "icao": "A320"
                }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("FRA") && ack.contains("MUC"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let aviationstack_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/flights")
        .count();
    assert_eq!(aviationstack_requests, 1);

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.route, Some(("FRA".to_string(), "MUC".to_string())));
    assert_eq!(
        flight.takeoff_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-18T10:05:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
    assert!(flight.aviationstack_checked);

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn track_command_accepts_aviationstack_flight_before_adsb_appears() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "EIN336"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": {
                    "iata": "EI336",
                    "icao": "EIN336",
                    "number": "336"
                },
                "airline": {
                    "iata": "EI",
                    "icao": "EIN",
                    "name": "Aer Lingus"
                },
                "departure": {
                    "iata": "DUB",
                    "icao": "EIDW",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "estimated": "2026-04-18T12:45:00+00:00",
                    "actual": null
                },
                "aircraft": {
                    "icao24": null,
                    "icao": "A320"
                }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track EIN336").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("EIN336"), "got: {ack}");
    assert!(ack.contains("DUB") && ack.contains("BER"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("EIN336"));
    assert_eq!(flight.route, Some(("DUB".to_string(), "BER".to_string())));
    assert_eq!(flight.aircraft_type.as_deref(), Some("A320"));
    assert_eq!(flight.last_seen, None);
    assert_eq!(
        flight.scheduled_departure_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-18T10:30:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        flight.last_adsb_poll_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-18T11:00:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        flight.phase,
        twitch_1337::aviation::tracker::FlightPhase::Unknown
    );
    assert!(flight.aviationstack_checked);

    let callsign_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/callsign/EIN336")
        .count();
    assert_eq!(
        callsign_requests, 1,
        "pending flight should not be repolled immediately"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn pending_flight_polls_when_due_and_becomes_visible() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .up_to_n_times(1)
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "4ca123",
                "flight": "EIN336",
                "alt_baro": 12000,
                "gs": 280.0,
                "baro_rate": 1200,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "EIN336"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "EI336", "icao": "EIN336", "number": "336" },
                "airline": { "iata": "EI", "icao": "EIN", "name": "Aer Lingus" },
                "departure": {
                    "iata": "DUB",
                    "icao": "EIDW",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "estimated": "2026-04-18T12:45:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": null, "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track EIN336").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("EIN336"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(50)).await;
    bot.clock.advance(ChronoDuration::minutes(2));

    let visible = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        visible.contains("EIN336") && visible.contains("ADS-B sichtbar"),
        "got: {visible}"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    let requests = bot.adsb_mock.received_requests().await.unwrap();
    let callsign_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/callsign/EIN336")
        .count();
    assert_eq!(callsign_requests, 2);

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert!(flight.last_seen.is_some());
    assert_eq!(flight.hex.as_deref(), Some("4ca123"));

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn pending_flight_expires_without_extra_adsb_call() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "EIN336"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "EI336", "icao": "EIN336", "number": "336" },
                "airline": { "iata": "EI", "icao": "EIN", "name": "Aer Lingus" },
                "departure": {
                    "iata": "DUB",
                    "icao": "EIDW",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "estimated": "2026-04-18T12:45:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": null, "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track EIN336").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("EIN336"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(50)).await;
    bot.clock.advance(ChronoDuration::hours(13));

    let expired = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        expired.contains("EIN336") && expired.contains("nicht im ADS-B aufgetaucht"),
        "got: {expired}"
    );

    let requests = bot.adsb_mock.received_requests().await.unwrap();
    let callsign_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/callsign/EIN336")
        .count();
    assert_eq!(callsign_requests, 1);

    tokio::time::sleep(Duration::from_millis(100)).await;
    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    assert!(state.flights.is_empty());

    bot.shutdown().await;
}
