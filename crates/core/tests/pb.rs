use twitch_1337_core as twitch_1337;
mod common;

use std::collections::HashMap;
use std::time::Duration;

use chrono::NaiveDate;
use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::PersonalBest;

fn seeded_alice() -> HashMap<String, PersonalBest> {
    let mut seeded = HashMap::new();
    seeded.insert(
        "alice".into(),
        PersonalBest {
            ms: 234,
            date: NaiveDate::from_ymd_opt(2026, 4, 17).unwrap(),
        },
    );
    seeded
}

#[tokio::test]
#[serial]
async fn pb_returns_callers_own_pb() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_alice())
        .spawn()
        .await;

    bot.send("alice", "!pb").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(out.contains("Dein PB"), "missing self phrasing: {out}");
    assert!(out.contains("234"), "missing ms: {out}");
    assert!(out.contains("17.04.2026"), "missing date: {out}");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn pb_returns_other_user_pb() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_alice())
        .spawn()
        .await;

    bot.send("bob", "!pb alice").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("PB von alice"),
        "missing other phrasing: {out}"
    );
    assert!(out.contains("234"), "missing ms: {out}");
    assert!(out.contains("17.04.2026"), "missing date: {out}");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn pb_handles_at_prefix_and_case() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_alice())
        .spawn()
        .await;

    bot.send("bob", "!pb @ALICE").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("PB von alice"),
        "missing other phrasing: {out}"
    );
    assert!(out.contains("234"), "missing ms: {out}");

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn pb_empty_state_self() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("alice", "!pb").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("Du hast noch keinen PB"),
        "expected empty self message, got: {out}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn pb_empty_state_other() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("alice", "!pb bob").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("bob hat noch keinen PB"),
        "expected empty other message, got: {out}"
    );

    bot.shutdown().await;
}
