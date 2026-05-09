use twitch_1337_core as twitch_1337;
mod common;

use std::time::Duration;

use common::TestBotBuilder;
use llm::{Role, ToolCall, ToolChatCompletionResponse};
use serial_test::serial;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[serial]
async fn ai_command_returns_fake_response() {
    let bot = TestBotBuilder::new().with_ai().spawn().await;
    bot.llm.push_tool_message("pong");

    let mut bot = bot;
    bot.send("alice", "!ai ping").await;
    let body = bot.expect_reply(Duration::from_secs(2)).await;
    assert_eq!(body, "pong");

    let calls = bot.llm.tool_calls();
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

    let chat_calls = bot.llm.chat_calls();
    let tool_calls = bot.llm.tool_calls();
    assert!(
        chat_calls.is_empty(),
        "no chat call expected, got: {chat_calls:?}"
    );
    assert!(
        tool_calls.is_empty(),
        "no tool call expected, got: {tool_calls:?}"
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
                ai.history_length = 10;
                ai.emotes.enabled = true;
                ai.emotes.include_global = true;
            }
        })
        .with_emote_glossary(
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
        .spawn()
        .await;

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

    bot.send("bob", "LocalEmote heute stark").await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    bot.llm.push_tool_message("passt KEKW");
    bot.send("alice", "!ai sag etwas lustiges").await;
    let body = bot.expect_reply(Duration::from_secs(2)).await;
    assert_eq!(body, "passt KEKW");

    let calls = bot.llm.tool_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");
    let system_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == Role::System)
        .expect("request has a system message");
    assert!(system_msg.content.contains("7TV emotes available"));
    assert!(system_msg.content.contains("KEKW"));
    assert!(system_msg.content.contains("meaning=lachen"));
    assert!(system_msg.content.contains("LocalEmote"));
    assert!(!system_msg.content.contains("MissingEmote"));
    let local_pos = system_msg.content.find("- LocalEmote:").unwrap();
    let kekw_pos = system_msg.content.find("- KEKW:").unwrap();
    assert!(
        local_pos < kekw_pos,
        "recent chat emote should rank before generic context match:\n{}",
        system_msg.content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_continues_when_7tv_unavailable() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            if let Some(ai) = c.ai.as_mut() {
                ai.history_length = 0;
                ai.emotes.enabled = true;
            }
        })
        .with_emote_glossary(
            r#"
[[emotes]]
name = "KEKW"
meaning = "lachen"
"#,
        )
        .spawn()
        .await;

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

    bot.llm.push_tool_message("weiter ohne emote");
    bot.send("alice", "!ai ping").await;
    let body = bot.expect_reply(Duration::from_secs(2)).await;
    assert_eq!(body, "weiter ohne emote");

    let calls = bot.llm.tool_calls();
    assert_eq!(calls.len(), 1, "expected exactly one LLM call");
    let system_msg = calls[0]
        .messages
        .iter()
        .find(|m| m.role == Role::System)
        .expect("request has a system message");
    assert!(!system_msg.content.contains("7TV emotes available"));

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_web_tool_flow_search_success() {
    let search = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("format", "json"))
        .and(query_param("q", "rust latest release"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                {
                    "title": "Rust 1.90 released",
                    "url": "https://example.com/rust-190",
                    "content": "Release notes and highlights",
                    "publishedDate": "2026-04-25",
                    "engine": "news"
                }
            ]
        })))
        .mount(&search)
        .await;

    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            let ai = c.ai.as_mut().expect("ai configured");
            ai.web.enabled = true;
            ai.web.base_url = format!("{}/search", search.uri());
            ai.web.timeout = 5;
        })
        .spawn()
        .await;

    bot.llm.push_tool(ToolChatCompletionResponse::ToolCalls {
        calls: vec![ToolCall {
            id: "call_1".into(),
            name: "web_search".into(),
            arguments: serde_json::json!({
                "query": "rust latest release",
                "max_results": 1,
            }),
            arguments_parse_error: None,
        }],
        reasoning_content: None,
    });
    bot.llm
        .push_tool_message("Rust 1.90 just shipped with new language and tooling improvements.");

    bot.send("alice", "!ai any rust news?").await;
    let body = bot.expect_reply(Duration::from_secs(2)).await;
    assert!(body.contains("Rust 1.90"), "reply: {body}");

    let calls = bot.llm.tool_calls();
    assert_eq!(calls.len(), 2, "expected tool loop with two rounds");
    let first_tools: Vec<String> = calls[0].tools.iter().map(|t| t.name.clone()).collect();
    assert!(first_tools.iter().any(|t| t == "web_search"));
    assert!(first_tools.iter().any(|t| t == "read_url"));
    let first_round = calls[1]
        .prior_rounds
        .first()
        .expect("second request includes first round");
    assert_eq!(first_round.results[0].tool_name, "web_search");
    assert!(
        first_round.results[0]
            .content
            .contains("Rust 1.90 released"),
        "tool result: {}",
        first_round.results[0].content
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ai_command_read_url_round_trip() {
    // Bypass the SSRF guard so the bot can reach local wiremock servers.
    twitch_1337::ai::content::client::ssrf_bypass_for_tests(true);

    // 1. Origin server hosting the URL the bot will fetch.
    let origin = MockServer::start().await;
    let png = vec![
        0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
    ];
    Mock::given(method("GET"))
        .and(path("/p.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(png),
        )
        .mount(&origin)
        .await;

    // 2. Media-model provider (OpenAI-compatible /v1/chat/completions).
    let media = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "A small PNG." }
            }]
        })))
        .mount(&media)
        .await;

    // SearXNG endpoint (unused but needs to exist because cfg.web.enabled = true).
    let search = MockServer::start().await;

    let media_uri = media.uri();
    let search_uri = search.uri();
    let png_url = format!("{}/p.png", origin.uri());

    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| {
            let ai = c.ai.as_mut().expect("ai configured");
            ai.base_url = Some(format!("{media_uri}/v1"));
            ai.web.enabled = true;
            ai.web.base_url = format!("{search_uri}/search");
            ai.web.timeout = 5;
        })
        .spawn()
        .await;

    // Round 1: main model calls read_url.
    bot.llm.push_tool(ToolChatCompletionResponse::ToolCalls {
        calls: vec![ToolCall {
            id: "call_1".into(),
            name: "read_url".into(),
            arguments: serde_json::json!({
                "url": png_url,
                "instruction": "describe this image",
            }),
            arguments_parse_error: None,
        }],
        reasoning_content: None,
    });
    // Round 2: main model returns final assistant text.
    bot.llm.push_tool_message("Image seen: A small PNG.");

    bot.send("alice", "!ai please look at the picture").await;
    let body = bot.expect_reply(Duration::from_secs(5)).await;
    assert!(
        body.contains("A small PNG."),
        "reply did not contain media answer: {body}"
    );

    // The second round-trip's prior tool result should carry the answer
    // returned by the media sub-agent.
    let calls = bot.llm.tool_calls();
    assert_eq!(calls.len(), 2, "expected two LLM rounds");
    let first_round = calls[1]
        .prior_rounds
        .first()
        .expect("second request includes first round");
    assert_eq!(first_round.results[0].tool_name, "read_url");
    assert!(
        first_round.results[0].content.contains("A small PNG."),
        "tool result: {}",
        first_round.results[0].content
    );

    bot.shutdown().await;

    // Reset the bypass so subsequent tests (if any run in the same process) are
    // not affected. Serial execution means this is belt-and-suspenders.
    twitch_1337::ai::content::client::ssrf_bypass_for_tests(false);
}
