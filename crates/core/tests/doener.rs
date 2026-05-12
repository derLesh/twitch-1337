mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[serial]
async fn dpi_global_returns_summary_from_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/stats.php"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"ok":true,"total_locations":6092,"total_cities":2202,"min_price":5.5,"max_price":9,"avg_price":6.1,"locations_no_price":5304,"locations_no_price_pct":87.1}"#.as_slice(),
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
        reply.contains("Döner-Index DE:") && reply.contains("6092 Buden"),
        "expected global summary, got: {reply}"
    );

    bot.shutdown().await;
}
