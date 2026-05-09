//! Integration tests for the optional `twitch.ai_channel`: only `!ai` is
//! reachable there, all other commands and the 1337 tracker ignore it,
//! chat history skips it, and the primary-channel path is unchanged.

mod common;

use std::time::Duration;

use chrono::TimeZone;
use chrono_tz::Europe::Berlin;
use common::TestBotBuilder;

const AI_CHAN: &str = "ai_chan";

#[tokio::test]
async fn ai_command_works_in_ai_channel() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.llm.push_tool_message("stubbed reply");
    bot.send_to(AI_CHAN, "viewer", "!ai hello").await;

    let (channel, body) = bot.expect_say_full(Duration::from_secs(2)).await;
    assert_eq!(channel, AI_CHAN, "ai reply must land in ai_channel");
    assert!(body.contains("stubbed reply"), "got: {body}");

    bot.shutdown().await;
}

#[tokio::test]
async fn lb_is_ignored_in_ai_channel() {
    let mut bot = TestBotBuilder::new()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.send_to(AI_CHAN, "viewer", "!lb").await;
    bot.expect_silent(Duration::from_millis(300)).await;

    bot.shutdown().await;
}

#[tokio::test]
async fn ping_is_ignored_in_ai_channel() {
    let mut bot = TestBotBuilder::new()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.send_to(AI_CHAN, "viewer", "!p list").await;
    bot.expect_silent(Duration::from_millis(300)).await;

    bot.shutdown().await;
}

#[tokio::test]
async fn track_is_ignored_in_ai_channel() {
    let mut bot = TestBotBuilder::new()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.send_to(AI_CHAN, "viewer", "!track DLH400").await;
    bot.expect_silent(Duration::from_millis(300)).await;

    bot.shutdown().await;
}

#[tokio::test]
async fn aviation_lookup_is_ignored_in_ai_channel() {
    let mut bot = TestBotBuilder::new()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.send_to(AI_CHAN, "viewer", "!up EDDF").await;
    bot.expect_silent(Duration::from_millis(300)).await;

    bot.shutdown().await;
}

#[tokio::test]
async fn feedback_is_ignored_in_ai_channel() {
    let mut bot = TestBotBuilder::new()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.send_to(AI_CHAN, "viewer", "!fb please add X").await;
    bot.expect_silent(Duration::from_millis(300)).await;

    bot.shutdown().await;
}

#[tokio::test]
async fn tracker_1337_ignores_ai_channel_messages() {
    // 13:37 Berlin → UTC instant; format as a `tmi-sent-ts` (ms since epoch)
    // matching what Twitch puts on incoming PRIVMSGs.
    let at_1337 = Berlin
        .with_ymd_and_hms(2026, 4, 28, 13, 37, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let ts_ms: i64 = at_1337.timestamp_millis();

    let bot = TestBotBuilder::new()
        .at(at_1337)
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.send_to_at(AI_CHAN, "viewer", "1337", ts_ms).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    let lb_path = bot.data_dir.path().join("leaderboard.ron");
    if lb_path.exists() {
        let contents = std::fs::read_to_string(&lb_path).expect("read leaderboard");
        assert!(
            !contents.contains("viewer"),
            "ai_channel 1337 must not appear in leaderboard: {contents}"
        );
    }

    bot.shutdown().await;
}

#[tokio::test]
async fn ai_command_still_works_in_primary_channel() {
    let mut bot = TestBotBuilder::new()
        .with_ai()
        .with_config(|c| c.twitch.ai_channel = Some(AI_CHAN.into()))
        .spawn()
        .await;

    bot.llm.push_tool_message("primary reply");
    bot.send("viewer", "!ai hello").await;

    let (channel, body) = bot.expect_say_full(Duration::from_secs(2)).await;
    assert_eq!(channel, "test_chan");
    assert!(body.contains("primary reply"));

    bot.shutdown().await;
}
