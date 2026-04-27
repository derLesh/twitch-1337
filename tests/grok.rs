mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::llm::ToolChatCompletionResponse;

#[tokio::test]
#[serial]
async fn grok_reply_uses_ai_model_and_parent_message() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
            }
        })
        .spawn()
        .await;
    bot.llm.push_chat("Nein, das ist Quatsch.");

    let mut bot = bot;
    bot.send_reply("alice", "@grok stimmt das", "bob", "Die Erde ist flach")
        .await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "Nein, das ist Quatsch.");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");
    assert_eq!(calls[0].model, "test-model");
    let user_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("request has a user message");
    assert!(user_message.content.contains("stimmt das"));
    assert!(user_message.content.contains("Die Erde ist flach"));
    assert!(user_message.content.contains("bob"));
    assert!(bot.llm.tool_calls().is_empty());

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn grok_without_reply_shows_usage_without_llm_call() {
    let mut bot = TestBotBuilder::new().with_ai().spawn().await;

    bot.send("alice", "@grok stimmt das").await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("Antworte auf eine Nachricht mit @grok"),
        "usage reply: {out}"
    );
    assert!(bot.llm.chat_calls().is_empty());
    assert!(bot.llm.tool_calls().is_empty());

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn grok_uses_web_tools_when_enabled() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.web.enabled = true;
            }
        })
        .spawn()
        .await;
    bot.llm.push_tool(ToolChatCompletionResponse::Message(
        "Web sagt: nope.".into(),
    ));

    let mut bot = bot;
    bot.send_reply(
        "alice",
        "@grok stimmt das aktuell",
        "bob",
        "Berlin hat heute 40 Grad",
    )
    .await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "Web sagt: nope.");

    assert!(bot.llm.chat_calls().is_empty());
    let calls = bot.llm.tool_calls();
    assert_eq!(calls.len(), 1, "expected exactly one tool-capable LLM call");
    assert_eq!(calls[0].model, "test-model");
    let user_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("request has a user message");
    assert!(user_message.content.contains("Berlin hat heute 40 Grad"));
    assert!(user_message.content.contains("stimmt das aktuell"));
    assert!(!calls[0].tools.is_empty(), "web tools should be provided");

    bot.shutdown().await;
}
