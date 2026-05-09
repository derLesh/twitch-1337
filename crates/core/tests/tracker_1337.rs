mod common;

use std::time::Duration;

use chrono::{Duration as ChronoDuration, TimeZone};
use chrono_tz::Europe::Berlin;
use common::TestBotBuilder;
use common::irc_line::privmsg_at;
use serial_test::serial;

use crate::common::test_bot::TestBot;

/// ms timestamp for 2026-04-18 13:37:00 Berlin (UTC+2 in April).
/// 2026-04-18 13:37:00 Berlin = 2026-04-18 11:37:00 UTC
const TMI_TS_13_37_BERLIN: i64 = 1_776_512_220_000;

async fn yield_a_bit() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
}

/// Advance the clock through the 13:35→13:36:30 window, consume the reminder,
/// and return the bot ready for message injection in the 13:36:30–13:38 window.
async fn advance_to_1337_window(bot: &mut TestBot) {
    // 13:35 → 13:36: handler wakes from wait_until_schedule and spawns monitor.
    bot.clock.advance(ChronoDuration::seconds(60));
    yield_a_bit().await;

    // 13:36 → 13:36:30: handler wakes from sleep_until_hms(13:36:30) and says reminder.
    bot.clock.advance(ChronoDuration::seconds(30));
    yield_a_bit().await;

    let reminder = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        reminder.contains("PausersHype"),
        "expected PausersHype reminder, got: {reminder}"
    );
}

#[tokio::test]
#[serial]
async fn tracker_1337_posts_reminder_and_stats() {
    let mut bot = TestBotBuilder::new()
        .at(Berlin
            .with_ymd_and_hms(2026, 4, 18, 13, 35, 0)
            .unwrap()
            .with_timezone(&chrono::Utc))
        .spawn()
        .await;

    advance_to_1337_window(&mut bot).await;

    // Inject with tmi-sent-ts=13:37 Berlin so the monitor's hour/minute guard passes.
    let msg_alice = privmsg_at(&bot.channel, "alice", "1337", TMI_TS_13_37_BERLIN);
    let msg_charlie = privmsg_at(&bot.channel, "charlie", "DANKIES", TMI_TS_13_37_BERLIN);
    bot.transport
        .inject
        .send(msg_alice)
        .await
        .expect("inject alice");
    bot.transport
        .inject
        .send(msg_charlie)
        .await
        .expect("inject charlie");
    yield_a_bit().await;

    // 13:36:30 → 13:38:00: handler aborts monitor and posts stats.
    bot.clock.advance(ChronoDuration::seconds(90));
    yield_a_bit().await;

    let stats = bot.expect_say(Duration::from_secs(3)).await;
    // 2 users, neither "gargoyletec" → generate_stats_message produces "2 … gnocci …"
    assert!(
        stats.contains('2') || stats.to_lowercase().contains("gnocci"),
        "expected 2-user stats, got: {stats}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn tracker_1337_updates_leaderboard_with_sub_second_time() {
    let mut bot = TestBotBuilder::new()
        .at(Berlin
            .with_ymd_and_hms(2026, 4, 18, 13, 35, 0)
            .unwrap()
            .with_timezone(&chrono::Utc))
        .spawn()
        .await;

    advance_to_1337_window(&mut bot).await;

    // Inject "1337" with tmi-sent-ts at 13:37:00.234 Berlin. The monitor
    // computes ms_since_minute = seconds*1000 + subsec_millis; alice should
    // be recorded as 234ms and persisted to leaderboard.ron.
    let ts_alice_234ms = TMI_TS_13_37_BERLIN + 234;
    let msg_alice = privmsg_at(&bot.channel, "alice", "1337", ts_alice_234ms);
    bot.transport
        .inject
        .send(msg_alice)
        .await
        .expect("inject alice");
    yield_a_bit().await;

    // Advance to 13:38:00 so the handler computes stats + persists leaderboard.
    bot.clock.advance(ChronoDuration::seconds(90));
    yield_a_bit().await;

    let _stats = bot.expect_say(Duration::from_secs(3)).await;

    // leaderboard.ron written under DATA_DIR.
    let lb_path = bot.data_dir.path().join("leaderboard.ron");
    let contents = tokio::fs::read_to_string(&lb_path)
        .await
        .expect("read leaderboard.ron");
    assert!(
        contents.contains("alice"),
        "leaderboard missing alice: {contents}"
    );
    assert!(
        contents.contains("ms:234") || contents.contains("ms: 234"),
        "leaderboard missing sub-second time 234: {contents}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn tracker_1337_posts_slowest_only_for_final_ten_seconds() {
    let mut bot = TestBotBuilder::new()
        .at(Berlin
            .with_ymd_and_hms(2026, 4, 18, 13, 35, 0)
            .unwrap()
            .with_timezone(&chrono::Utc))
        .spawn()
        .await;

    advance_to_1337_window(&mut bot).await;

    let ts_alice_49999ms = TMI_TS_13_37_BERLIN + 49_999;
    let ts_charlie_58700ms = TMI_TS_13_37_BERLIN + 58_700;
    let msg_alice = privmsg_at(&bot.channel, "alice", "1337", ts_alice_49999ms);
    let msg_charlie = privmsg_at(&bot.channel, "charlie", "DANKIES", ts_charlie_58700ms);
    bot.transport
        .inject
        .send(msg_alice)
        .await
        .expect("inject alice");
    bot.transport
        .inject
        .send(msg_charlie)
        .await
        .expect("inject charlie");
    yield_a_bit().await;

    bot.clock.advance(ChronoDuration::seconds(90));
    yield_a_bit().await;

    let stats = bot.expect_say(Duration::from_secs(3)).await;
    assert!(
        stats.contains("Am langsamsten war charlie mit 58s"),
        "expected slowest seconds in stats, got: {stats}"
    );
    assert!(
        !stats.contains("58700ms"),
        "slowest time should be shown in seconds, got: {stats}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn tracker_1337_does_not_post_slowest_before_final_ten_seconds() {
    let mut bot = TestBotBuilder::new()
        .at(Berlin
            .with_ymd_and_hms(2026, 4, 18, 13, 35, 0)
            .unwrap()
            .with_timezone(&chrono::Utc))
        .spawn()
        .await;

    advance_to_1337_window(&mut bot).await;

    let ts_alice_1200ms = TMI_TS_13_37_BERLIN + 1_200;
    let ts_charlie_49999ms = TMI_TS_13_37_BERLIN + 49_999;
    let msg_alice = privmsg_at(&bot.channel, "alice", "1337", ts_alice_1200ms);
    let msg_charlie = privmsg_at(&bot.channel, "charlie", "DANKIES", ts_charlie_49999ms);
    bot.transport
        .inject
        .send(msg_alice)
        .await
        .expect("inject alice");
    bot.transport
        .inject
        .send(msg_charlie)
        .await
        .expect("inject charlie");
    yield_a_bit().await;

    bot.clock.advance(ChronoDuration::seconds(90));
    yield_a_bit().await;

    let stats = bot.expect_say(Duration::from_secs(3)).await;
    assert!(
        !stats.contains("Am langsamsten"),
        "did not expect slowest before final ten seconds, got: {stats}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn tracker_1337_zero_users_posts_erm_or_fuh() {
    let mut bot = TestBotBuilder::new()
        .at(Berlin
            .with_ymd_and_hms(2026, 4, 18, 13, 35, 0)
            .unwrap()
            .with_timezone(&chrono::Utc))
        .spawn()
        .await;

    advance_to_1337_window(&mut bot).await;

    // 13:36:30 → 13:38:00 with no messages injected.
    bot.clock.advance(ChronoDuration::seconds(90));
    yield_a_bit().await;

    let stats = bot.expect_say(Duration::from_secs(3)).await;
    // twitch-irc say() prepends ". " to prevent slash-command execution.
    let stats_body = stats.trim_start_matches(". ").trim();
    assert!(
        stats_body == "Erm" || stats_body == "fuh",
        "expected zero-user stats (Erm/fuh), got: {stats:?}"
    );

    bot.shutdown().await;
}
