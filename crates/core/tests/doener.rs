mod common;

use std::time::Duration;

use common::TestBotBuilder;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn dpi_global_returns_summary_from_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app-api/public/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"national_average":8.36,"total_cities":1072,"total_shops":1897,"total_reports":3514,"change_30d":1.7,"mode_price":7}"#.as_slice(),
            "application/json",
        ))
        .mount(&server)
        .await;

    let mut bot = TestBotBuilder::new()
        .with_doener_base_url(server.uri())
        .spawn()
        .await;

    bot.send("alice", "!dpi").await;
    let reply = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        reply.contains("Döneratlas DE:") && reply.contains("1072 Städte"),
        "expected global summary, got: {reply}"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn dpi_unicode_exact_match_prefers_city_details() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app-api/public/search"))
        .and(wiremock::matchers::query_param("q", "MÜNCHEN"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"cities":[{"id":1,"name":"München","slug":"muenchen","shop_count":42},{"id":2,"name":"Müncheberg","slug":"muencheberg","shop_count":2}],"shops":[{"city_slug":"muenchen","city_name":"München","current_price":"7.00"},{"city_slug":"muencheberg","city_name":"Müncheberg","current_price":"5.00"}]}"#.as_bytes(),
            "application/json",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/app-api/public/cities"))
        .and(wiremock::matchers::query_param("slug", "muenchen"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"name":"München","slug":"muenchen","shop_count":42,"avg_price":"8.10","min_price":"6.50","max_price":"10.00"}"#.as_bytes(),
            "application/json",
        ))
        .mount(&server)
        .await;

    let mut bot = TestBotBuilder::new()
        .with_doener_base_url(server.uri())
        .spawn()
        .await;

    bot.send("alice", "!dpi MÜNCHEN").await;
    let reply = bot.expect_reply(Duration::from_secs(2)).await;
    assert!(
        reply.starts_with("München: 42 Buden")
            && reply.contains("8.10")
            && reply.contains("6.50")
            && reply.contains("10.00"),
        "expected enriched exact city reply, got: {reply}"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn doener_calc_accepts_ascii_alias_and_formats_reply() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app-api/public/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"national_average":8.36,"total_cities":1072,"total_shops":1897,"total_reports":3514,"change_30d":1.7,"mode_price":7}"#.as_slice(),
            "application/json",
        ))
        .mount(&server)
        .await;

    let mut bot = TestBotBuilder::new()
        .with_doener_base_url(server.uri())
        .spawn()
        .await;

    bot.send("alice", "!doener 16,72").await;
    let reply = bot.expect_reply(Duration::from_secs(2)).await;
    assert!(
        reply.contains("Das wären 2 Döner") && reply.contains("Ø 8,36€ Deutschland"),
        "expected calculated döner count, got: {reply}"
    );

    bot.shutdown().await;
}
