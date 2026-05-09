use std::time::Duration;

use serial_test::serial;

mod common;

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn web_healthz_responds_when_enabled() {
    let bot = common::TestBotBuilder::new()
        .with_web("127.0.0.1:18080")
        .spawn()
        .await;

    bot.set_irc_connected(true);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let res = reqwest::Client::new()
        .get("http://127.0.0.1:18080/healthz")
        .send()
        .await
        .expect("connect");
    assert_eq!(res.status(), 200);

    bot.shutdown().await;
}
