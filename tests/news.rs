mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn news_command_summarizes_since_previous_user_message() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
        })
        .spawn()
        .await;

    bot.send("bob", "old topic before alice").await;
    bot.send("alice", "ich bin kurz weg").await;
    bot.send("carol", "first relevant update").await;
    bot.send("dave", "second relevant update").await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("carol und dave haben Updates gepostet");
    bot.send("alice", "!news").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "carol und dave haben Updates gepostet");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one chat completion call");
    let user_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("request has a user message");

    assert!(
        user_msg.content.contains("carol: first relevant update"),
        "missing relevant carol line: {}",
        user_msg.content
    );
    assert!(
        user_msg.content.contains("dave: second relevant update"),
        "missing relevant dave line: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("old topic before alice"),
        "included message before alice's previous line: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("!news"),
        "included triggering command: {}",
        user_msg.content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn news_command_uses_full_history_without_previous_user_message() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
        })
        .spawn()
        .await;

    bot.send("bob", "channel started talking").await;
    bot.send("carol", "another update").await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("der ganze Verlauf wurde zusammengefasst");
    bot.send("alice", "!news").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "der ganze Verlauf wurde zusammengefasst");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one chat completion call");
    let user_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("request has a user message");

    assert!(
        user_msg.content.contains("bob: channel started talking"),
        "missing first history line: {}",
        user_msg.content
    );
    assert!(
        user_msg.content.contains("carol: another update"),
        "missing second history line: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("!news"),
        "included triggering command: {}",
        user_msg.content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn news_command_without_history_does_not_call_llm() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
            }
        })
        .spawn()
        .await;

    bot.send("bob", "this will not be recorded").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.send("alice", "!news").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("noch keine Chat-Historie"),
        "unexpected empty-history reply: {out}"
    );

    let calls = bot.llm.chat_calls();
    assert!(calls.is_empty(), "no LLM call expected, got: {calls:?}");

    bot.shutdown().await;
}
