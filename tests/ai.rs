mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::llm::{ToolCall, ToolChatCompletionResponse};

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
async fn ai_command_saves_memory_extraction() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.memory_enabled = true;
            }
        })
        .spawn()
        .await;

    // Main response
    bot.llm.push_chat("nice to meet you Alice");
    // Fire-and-forget memory extraction call uses chat_completion_with_tools.
    // Return a save_memory tool call with key "alice_name", fact "alice likes coffee".
    bot.llm
        .push_tool(ToolChatCompletionResponse::ToolCalls(vec![ToolCall {
            id: "call_1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "key": "alice_name",
                "fact": "alice likes coffee"
            }),
            arguments_parse_error: None,
        }]));
    // Memory extraction loop continues until the LLM stops returning tool calls.
    // Second round: return a plain message to terminate the loop.
    bot.llm
        .push_tool(ToolChatCompletionResponse::Message(String::new()));

    bot.send("alice", "!ai my name is Alice").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "nice to meet you Alice");

    // Memory extraction runs fire-and-forget after the reply. Poll briefly for
    // the RON file to contain the expected fact.
    let memory_path = bot.data_dir.path().join("ai_memory.ron");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(contents) = tokio::fs::read_to_string(&memory_path).await
            && contents.contains("alice likes coffee")
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
