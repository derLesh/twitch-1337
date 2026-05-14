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
