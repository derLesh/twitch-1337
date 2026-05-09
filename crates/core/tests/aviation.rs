mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
#[serial]
async fn up_command_lists_aircraft_above_plz() {
    let bot = TestBotBuilder::new().spawn().await;

    // Stub the ADS-B point-radius endpoint (path: /point/{lat}/{lon}/{radius}).
    // The test AviationClient uses adsb_mock.uri() as the live ADS-B base URL (no /v2 prefix).
    Mock::given(method("GET"))
        .and(path_regex(r"^/point/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [
                {
                    "hex": "3c6589",
                    "flight": "DLH1234",
                    "alt_baro": 35000,
                    "lat": 52.52,
                    "lon": 13.40,
                    "gs": 450.0,
                    "squawk": "1000"
                }
            ],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    // Stub the adsbdb callsign route endpoint so DLH1234 resolves to a route.
    // The test fixture sets adsbdb_base_url = adsb_mock.uri() as well.
    Mock::given(method("GET"))
        .and(path_regex(r"^/callsign/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "response": {
                "flightroute": {
                    "origin": { "iata_code": "FRA" },
                    "destination": { "iata_code": "TXL" }
                }
            }
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!up 10115").await;
    let out = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        out.contains("DLH1234") || out.contains("DLH"),
        "expected DLH1234 in up output: {out}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn up_command_includes_aircraft_without_route() {
    let bot = TestBotBuilder::new().spawn().await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/point/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [
                {
                    "hex": "abcdef",
                    "flight": "PRIV01",
                    "t": "C172",
                    "alt_baro": 3500,
                    "lat": 52.52,
                    "lon": 13.40,
                    "gs": 110.0,
                    "squawk": "1200"
                }
            ],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/callsign/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "response": "unknown callsign"
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!up 10115").await;
    let out = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        out.contains("PRIV01"),
        "expected PRIV01 in up output: {out}"
    );
    assert!(out.contains("C172"), "expected C172 in up output: {out}");
    assert!(
        !out.contains("→"),
        "no route arrow expected when adsbdb returns no route: {out}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn up_command_uses_registration_when_callsign_missing() {
    let bot = TestBotBuilder::new().spawn().await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/point/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [
                {
                    "hex": "abcdef",
                    "r": "D-EABC",
                    "t": "C172",
                    "alt_baro": 3500,
                    "lat": 52.52,
                    "lon": 13.40,
                    "gs": 110.0
                }
            ],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!up 10115").await;
    let out = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        out.contains("D-EABC"),
        "expected registration D-EABC in up output: {out}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn up_command_falls_back_to_hex() {
    let bot = TestBotBuilder::new().spawn().await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/point/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [
                {
                    "hex": "abcdef",
                    "alt_baro": 2500,
                    "lat": 52.52,
                    "lon": 13.40,
                    "gs": 90.0
                }
            ],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!up 10115").await;
    let out = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        out.contains("abcdef"),
        "expected hex abcdef in up output: {out}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn up_command_sorts_by_distance() {
    let bot = TestBotBuilder::new().spawn().await;

    // Two aircraft, both inside the cone above 10115 (lat 52.5208, lon 13.4094).
    // FAR1 sits ~9 NM away, NEAR1 sits right above the point.
    Mock::given(method("GET"))
        .and(path_regex(r"^/point/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [
                {
                    "hex": "111111",
                    "flight": "FAR1",
                    "t": "A320",
                    "alt_baro": 35000,
                    "lat": 52.5208,
                    "lon": 13.5594,
                    "gs": 450.0
                },
                {
                    "hex": "222222",
                    "flight": "NEAR1",
                    "t": "A320",
                    "alt_baro": 35000,
                    "lat": 52.5208,
                    "lon": 13.4094,
                    "gs": 450.0
                }
            ],
            "ctime": 0,
            "now": 0,
            "total": 2
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/callsign/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "response": "unknown callsign"
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!up 10115").await;
    let out = bot.expect_say(Duration::from_secs(5)).await;
    let near_pos = out.find("NEAR1").expect("NEAR1 in output");
    let far_pos = out.find("FAR1").expect("FAR1 in output");
    assert!(
        near_pos < far_pos,
        "expected NEAR1 before FAR1 in distance-sorted output: {out}"
    );

    bot.shutdown().await;
}
