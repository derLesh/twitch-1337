mod common;

use std::time::Duration;

use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::ai::llm::ToolChatCompletionResponse;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[serial]
async fn ai_reply_includes_parent_message() {
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
    bot.send_reply("alice", "!ai stimmt das", "bob", "Die Erde ist flach")
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
async fn grok_without_reply_behaves_like_ai_alias() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
            }
        })
        .spawn()
        .await;
    bot.llm.push_chat("alias ok");

    let mut bot = bot;
    bot.send("alice", "@grok sag hallo").await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "alias ok");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");
    let user_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("request has a user message");
    assert!(user_message.content.contains("sag hallo"));
    assert!(bot.llm.tool_calls().is_empty());

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn grok_reply_with_leading_mention_triggers_alias() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
            }
        })
        .spawn()
        .await;
    bot.llm.push_chat("Nein, Chat, das ist Quatsch.");

    let mut bot = bot;
    bot.send_reply(
        "alice",
        "@bob @grok stimmt das",
        "bob",
        "Die Erde ist flach",
    )
    .await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "Nein, Chat, das ist Quatsch.");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");
    let user_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("request has a user message");
    assert!(user_message.content.contains("stimmt das"));
    assert!(user_message.content.contains("Replied-to author: bob"));
    assert!(
        user_message
            .content
            .contains("Replied-to message: Die Erde ist flach")
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn grok_empty_reply_with_leading_mention_uses_default_instruction() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
            }
        })
        .spawn()
        .await;
    bot.llm.push_chat("Kurz: nein.");

    let mut bot = bot;
    bot.send_reply("alice", "@bob @grok", "bob", "Berlin liegt auf dem Mond")
        .await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "Kurz: nein.");

    let calls = bot.llm.chat_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");
    let user_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("request has a user message");
    assert!(user_message.content.contains("Prüfe die Reply-Nachricht"));
    assert!(
        user_message
            .content
            .contains("Replied-to message: Berlin liegt auf dem Mond")
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn grok_strips_visible_reasoning_prefix_from_response() {
    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
            }
        })
        .spawn()
        .await;
    bot.llm.push_chat("thought test_channel|Hallo Chat");

    let mut bot = bot;
    bot.send("alice", "@grok sag hallo").await;

    let out = bot.expect_say(Duration::from_secs(2)).await;
    let body = out.strip_prefix(". ").unwrap_or(&out);
    assert_eq!(body, "Hallo Chat");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn grok_uses_web_tools_when_enabled() {
    let search = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                {
                    "title": "Berlin weather",
                    "url": "https://example.com/berlin-weather",
                    "content": "Berlin hat heute keine 40 Grad",
                    "engine": "test"
                }
            ]
        })))
        .mount(&search)
        .await;

    let bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.web.enabled = true;
                ai.web.base_url = format!("{}/search", search.uri());
                ai.web.timeout = 5;
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
    assert_eq!(calls[0].prior_rounds.len(), 1);
    let forced_result = &calls[0].prior_rounds[0].results[0];
    assert_eq!(forced_result.tool_name, "web_search");
    assert!(
        forced_result
            .content
            .contains("Berlin hat heute keine 40 Grad"),
        "forced web result: {}",
        forced_result.content
    );
    let user_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("request has a user message");
    assert!(user_message.content.contains("Berlin hat heute 40 Grad"));
    assert!(user_message.content.contains("stimmt das aktuell"));
    assert!(!calls[0].tools.is_empty(), "web tools should be provided");
    let system_message = calls[0]
        .messages
        .iter()
        .find(|message| message.role == "system")
        .expect("request has a system message");
    assert!(system_message.content.contains("test prompt"));
    assert!(system_message.content.contains("Grok-inspired"));
    assert!(system_message.content.contains("do not claim access to X"));
    assert!(system_message.content.contains("do not invent X posts"));
    assert!(!system_message.content.contains("Du bist NICHT Grok"));

    bot.shutdown().await;
}
