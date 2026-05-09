mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn feedback_appends_to_file() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("alice", "!fb this is useful feedback").await;
    let ack = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        ack.contains("Feedback gespeichert"),
        "expected confirmation, got: {ack}"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let path = bot.data_dir.path().join("feedback.txt");
    let contents = tokio::fs::read_to_string(&path)
        .await
        .expect("read feedback.txt");
    assert!(
        contents.contains("alice: this is useful feedback"),
        "feedback file missing expected line: {contents}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn feedback_empty_message_shows_usage() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("alice", "!fb").await;
    let reply = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        reply.contains("Benutzung: !fb"),
        "expected usage message, got: {reply}"
    );

    let path = bot.data_dir.path().join("feedback.txt");
    assert!(
        tokio::fs::metadata(&path).await.is_err(),
        "feedback.txt should not have been created for empty message"
    );

    bot.shutdown().await;
}
