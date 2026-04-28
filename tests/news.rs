mod common;

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::twitch::whisper::{FIRST_WHISPER_MAX_CHARS, WHISPER_MAX_CHARS};

#[tokio::test]
#[serial]
async fn news_command_summarizes_since_previous_user_message() {
    let bot = TestBotBuilder::new()
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
    let out = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(out.to_user_id, "67890");
    assert_eq!(out.message, "ICYMI: carol und dave haben Updates gepostet");

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
        user_msg.content.contains("old topic before alice"),
        "recent alice line should not cut off earlier context: {}",
        user_msg.content
    );
    assert!(user_msg.content.contains("alice: ich bin kurz weg"));
    assert!(
        !user_msg.content.contains("!news"),
        "included triggering command: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("Trenne mehrere Themen"),
        "duplicated format instruction in user prompt: {}",
        user_msg.content
    );
    let system_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "system")
        .expect("request has a system message");
    assert!(
        system_msg.content.contains("trenne sie mit \" | \""),
        "missing topic separator instruction: {}",
        system_msg.content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn news_command_uses_full_history_without_previous_user_message() {
    let bot = TestBotBuilder::new()
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
    let out = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(
        out.message,
        "ICYMI: der ganze Verlauf wurde zusammengefasst"
    );

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
async fn news_command_does_not_use_previous_news_response_as_boundary() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
            c.cooldowns.news = 0;
        })
        .spawn()
        .await;

    bot.send("bob", "old topic").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("icymi: old topic summary");
    bot.send("alice", "!news").await;
    let first = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(first.message, "ICYMI: old topic summary");

    bot.send("carol", "fresh topic").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("fresh topic summary");
    bot.send("dave", "!news").await;
    let out = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(out.message, "ICYMI: fresh topic summary");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 2, "expected two chat completion calls");
    let user_msg = calls[1]
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("request has a user message");

    assert!(
        user_msg.content.contains("carol: fresh topic"),
        "missing fresh topic: {}",
        user_msg.content
    );
    assert!(
        user_msg.content.contains("bob: old topic"),
        "previous public chat before news response should remain available: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("ICYMI: old topic summary"),
        "hidden previous news whisper should not be inserted into chat history: {}",
        user_msg.content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn news_command_uses_old_user_message_as_boundary_outside_recent_window() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 30;
            }
        })
        .spawn()
        .await;

    bot.send("bob", "before old alice marker").await;
    bot.send("alice", "old alice marker").await;
    for i in 0..21 {
        bot.send("carol", &format!("update {i}")).await;
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("older alice marker cut off");
    bot.send("alice", "!news").await;
    let out = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(out.message, "ICYMI: older alice marker cut off");

    let calls = bot.llm.chat_calls();
    let user_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("request has a user message");

    assert!(
        !user_msg.content.contains("before old alice marker"),
        "included content before old user boundary: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("old alice marker"),
        "included old user boundary line: {}",
        user_msg.content
    );
    assert!(user_msg.content.contains("carol: update 0"));
    assert!(user_msg.content.contains("carol: update 20"));

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

#[tokio::test]
#[serial]
async fn news_command_limits_first_whisper_to_500_chars() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
        })
        .spawn()
        .await;

    bot.send("bob", "lots happened").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("sehr ".repeat(300));
    bot.send_privmsg_as("alice", "alice-id", "!news").await;
    let out = bot.expect_whisper(Duration::from_secs(2)).await;

    assert_eq!(out.to_user_id, "alice-id");
    assert!(
        out.message.chars().count() <= FIRST_WHISPER_MAX_CHARS,
        "first whisper exceeded limit: {}",
        out.message.chars().count()
    );
    assert!(out.message.starts_with("ICYMI:"));

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn news_command_allows_longer_followup_whisper() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
            c.cooldowns.news = 0;
        })
        .spawn()
        .await;

    bot.send("bob", "first update").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    bot.llm.push_chat("first summary");
    bot.send_privmsg_as("alice", "alice-id", "!news").await;
    let first = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(first.message, "ICYMI: first summary");

    bot.send("carol", "a lot more happened").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    bot.llm.push_chat("lang ".repeat(300));
    bot.send_privmsg_as("alice", "alice-id", "!news").await;
    let second = bot.expect_whisper(Duration::from_secs(2)).await;

    let len = second.message.chars().count();
    assert!(
        len > FIRST_WHISPER_MAX_CHARS,
        "follow-up was too short: {len}"
    );
    assert!(len <= WHISPER_MAX_CHARS, "follow-up exceeded limit: {len}");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn news_command_falls_back_to_chat_when_whisper_fails() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_failing_whispers()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
        })
        .spawn()
        .await;

    bot.send("bob", "fallback topic").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("fallback summary");
    bot.send("alice", "!news").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "ICYMI: fallback summary");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn tldr_command_summarizes_available_last_24_hours() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
        })
        .spawn()
        .await;

    let now = Utc::now();
    bot.send_at(
        "bob",
        "older than one day",
        (now - ChronoDuration::hours(25)).timestamp_millis(),
    )
    .await;
    bot.send_at(
        "alice",
        "recent alice context",
        (now - ChronoDuration::hours(2)).timestamp_millis(),
    )
    .await;
    bot.send_at(
        "carol",
        "recent carol context",
        (now - ChronoDuration::minutes(30)).timestamp_millis(),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("24h tldr summary");
    bot.send_at("alice", "!tldr", now.timestamp_millis()).await;
    let out = bot.expect_whisper(Duration::from_secs(2)).await;
    assert_eq!(out.message, "ICYMI: 24h tldr summary");

    let calls = bot.llm.chat_calls();
    let user_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("request has a user message");

    assert!(
        !user_msg.content.contains("older than one day"),
        "included message older than 24h: {}",
        user_msg.content
    );
    assert!(user_msg.content.contains("alice: recent alice context"));
    assert!(user_msg.content.contains("carol: recent carol context"));
    assert!(
        user_msg.content.contains("TLDR"),
        "tldr prompt missing mode instruction: {}",
        user_msg.content
    );

    bot.shutdown().await;
}
