mod common;

use std::collections::HashMap;
use std::time::Duration;

use chrono::NaiveDate;
use common::TestBotBuilder;
use serial_test::serial;
use twitch_1337::PersonalBest;

#[tokio::test]
#[serial]
async fn leaderboard_returns_fastest_pb() {
    let mut seeded: HashMap<String, PersonalBest> = HashMap::new();
    seeded.insert(
        "alice".into(),
        PersonalBest {
            ms: 234,
            date: NaiveDate::from_ymd_opt(2026, 4, 17).unwrap(),
        },
    );
    seeded.insert(
        "bob".into(),
        PersonalBest {
            ms: 567,
            date: NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
        },
    );

    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded)
        .spawn()
        .await;

    bot.send("alice", "!lb").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(out.contains("alice"), "leaderboard missing alice: {out}");
    assert!(out.contains("234"), "leaderboard missing alice's ms: {out}");
    assert!(
        out.contains("17.04.2026"),
        "leaderboard missing alice's date: {out}"
    );
    assert!(
        !out.contains("bob"),
        "only fastest PB should be shown, bob present: {out}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn leaderboard_empty_state_message() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("alice", "!lb").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("Noch keine Einträge vorhanden"),
        "expected empty-state message, got: {out}"
    );

    bot.shutdown().await;
}
