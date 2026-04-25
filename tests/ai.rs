mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::llm::{ToolCall, ToolChatCompletionResponse};
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
#[serial]
async fn ai_command_returns_fake_response() {
    let bot = TestBotBuilder::new().with_ai().spawn().await;
    bot.llm.push_chat("pong");

    let mut bot = bot;
    bot.send("alice", "!ai ping").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    // say_in_reply_to prefixes ". " to prevent command injection; strip it before asserting.
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "pong");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_empty_shows_usage() {
    let mut bot = TestBotBuilder::new().with_ai().spawn().await;

    bot.send("alice", "!ai").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(out.contains("Benutzung: !ai"), "usage reply: {out}");

    // No LLM call made.
    let calls = bot.llm.chat_calls();
    assert!(calls.is_empty(), "no LLM call expected, got: {calls:?}");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_injects_chat_history() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 10;
            }
        })
        .spawn()
        .await;

    // Prime the history buffer with three non-command PRIVMSGs on the main channel.
    // The command handler records every PRIVMSG (including the later !ai) into
    // the buffer before checking for a command prefix, so a small history_length
    // would evict the earliest messages. Set length=10 to keep all four.
    bot.send("user1", "hello there").await;
    bot.send("user2", "hi back").await;
    bot.send("user3", "good morning").await;

    // Give the dispatcher time to observe and record each message.
    tokio::time::sleep(Duration::from_millis(100)).await;

    bot.llm.push_chat("acknowledged");
    bot.send("alice", "!ai what did people say?").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "acknowledged");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one chat completion call");
    let call = &calls[0];
    let user_msg = call
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("request has a user message");
    assert!(
        user_msg.content.contains("hello there"),
        "history missing user1 line: {}",
        user_msg.content
    );
    assert!(
        user_msg.content.contains("hi back"),
        "history missing user2 line: {}",
        user_msg.content
    );
    assert!(
        user_msg.content.contains("good morning"),
        "history missing user3 line: {}",
        user_msg.content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_injects_7tv_emote_glossary() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.emotes.enabled = true;
                ai.emotes.include_global = true;
            }
        })
        .spawn()
        .await;

    tokio::fs::write(
        bot.data_dir.path().join("7tv_emotes.toml"),
        r#"
[[emotes]]
name = "KEKW"
meaning = "lachen, etwas ist lustig"
usage = "bei Witzen oder Fail-Momenten"
avoid = "bei ernsten Themen"

[[emotes]]
name = "LocalEmote"
meaning = "lokaler Channel-Insider"
usage = "wenn der Chat den Insider anspricht"

[[emotes]]
name = "MissingEmote"
meaning = "steht nicht im aktuellen 7TV-Katalog"
"#,
    )
    .await
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/emote-sets/global"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "global",
            "emotes": [
                {"id": "global-kekw", "name": "KEKW"},
                {"id": "global-peepo", "name": "peepoHappy"}
            ]
        })))
        .mount(&bot.seventv_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/users/twitch/12345"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "user",
            "emote_set": {
                "id": "channel-set",
                "emotes": [
                    {"id": "channel-local", "name": "LocalEmote"},
                    {"id": "channel-kekw", "name": "KEKW"}
                ]
            }
        })))
        .mount(&bot.seventv_mock)
        .await;

    bot.llm.push_chat("passt KEKW");
    bot.send("alice", "!ai sag etwas lustiges").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "passt KEKW");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one chat completion call");
    let system_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "system")
        .expect("request has a system message");
    assert!(system_msg.content.contains("7TV emotes available"));
    assert!(system_msg.content.contains("KEKW"));
    assert!(system_msg.content.contains("meaning=lachen"));
    assert!(system_msg.content.contains("LocalEmote"));
    assert!(!system_msg.content.contains("MissingEmote"));

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_continues_when_7tv_unavailable() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.emotes.enabled = true;
            }
        })
        .spawn()
        .await;

    tokio::fs::write(
        bot.data_dir.path().join("7tv_emotes.toml"),
        r#"
[[emotes]]
name = "KEKW"
meaning = "lachen"
"#,
    )
    .await
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/emote-sets/global"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&bot.seventv_mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/twitch/12345"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&bot.seventv_mock)
        .await;

    bot.llm.push_chat("weiter ohne emote");
    bot.send("alice", "!ai ping").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "weiter ohne emote");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one chat completion call");
    let system_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == "system")
        .expect("request has a system message");
    assert!(!system_msg.content.contains("7TV emotes available"));

    bot.shutdown().await;
}

/// End-to-end smoke test for the Phase H extractor wiring: a successful
/// `!ai` exchange should spawn a fire-and-forget extraction pass that
/// routes a self-scoped `save_memory` tool call through the permission
/// matrix and persists the fact to `ai_memory.ron`. The full adversarial
/// surface (third-party writes, prompt injection, cap enforcement) lives
/// in `tests/memory_integration.rs` once Phase I lands.
#[tokio::test]
#[serial]
async fn ai_command_saves_memory_extraction() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.memory.enabled = true;
            }
        })
        .spawn()
        .await;

    // Main chat response.
    bot.llm.push_chat("nice to meet you Alice");
    // Extraction round 1: one self-scoped save_memory call. user-id 67890 is
    // the default injected by `irc_line::privmsg`, so subject_id must match
    // to pass the self-claim permission check.
    bot.llm.push_tool(ToolChatCompletionResponse::ToolCalls {
        calls: vec![ToolCall {
            id: "call_1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "scope": "user",
                "subject_id": "67890",
                "slug": "likes-coffee",
                "fact": "alice likes coffee",
            }),
            arguments_parse_error: None,
        }],
        reasoning_content: None,
    });
    // Extraction round 2: plain-text response terminates the loop.
    bot.llm
        .push_tool(ToolChatCompletionResponse::Message(String::new()));

    bot.send("alice", "!ai my name is Alice").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "nice to meet you Alice");

    // Memory extraction runs fire-and-forget after the reply. Poll briefly
    // for the RON file to contain the expected fact under the new scoped key.
    let memory_path = bot.data_dir.path().join("ai_memory.ron");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(contents) = tokio::fs::read_to_string(&memory_path).await
            && contents.contains("alice likes coffee")
            && contents.contains("user:67890:likes-coffee")
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let contents = tokio::fs::read_to_string(&memory_path)
                .await
                .unwrap_or_else(|_| "<not created>".into());
            panic!("expected memory file to contain saved fact, got: {contents}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    bot.shutdown().await;
}
