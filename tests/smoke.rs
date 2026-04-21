mod common;

use std::time::Duration;

use common::fake_transport;
use serial_test::serial;
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::{ClientConfig, TwitchIRCClient};

#[tokio::test]
#[serial]
async fn test_bot_spawns_and_shuts_down_cleanly() {
    let bot = common::TestBotBuilder::new().spawn().await;
    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn fake_transport_handshake_succeeds() {
    let mut handle = fake_transport::install().await;

    let mut cfg = ClientConfig::new_simple(StaticLoginCredentials::new(
        "bot".to_owned(),
        Some("test-token".to_owned()),
    ));
    // Prevent the client from trying a second connection if something fails.
    cfg.connection_rate_limiter = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    let (_incoming, client) =
        TwitchIRCClient::<fake_transport::FakeTransport, StaticLoginCredentials>::new(cfg);

    client.join("test_chan".to_owned()).expect("join");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut captured = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), handle.capture.recv()).await {
            Ok(Some(line)) => {
                captured.push(line);
                if captured.len() >= 4 {
                    break;
                }
            }
            _ => break,
        }
    }
    drop(handle);
    drop(client);

    let joined = captured.join("\n");
    assert!(joined.contains("CAP REQ"), "captured: {joined}");
    assert!(joined.contains("PASS"), "captured: {joined}");
    assert!(joined.contains("NICK"), "captured: {joined}");
    assert!(joined.contains("JOIN"), "captured: {joined}");
}
