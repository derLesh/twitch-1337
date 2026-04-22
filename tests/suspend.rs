mod common;

use std::collections::HashMap;
use std::time::Duration;

use chrono::NaiveDate;
use common::{TestBot, TestBotBuilder};
use serial_test::serial;
use twitch_1337::PersonalBest;

/// twitch-irc's default rate limiter allows 5 msgs / 150 ms per connection.
/// When a test crosses that threshold the client opens a second connection,
/// but FakeTransport's `install()` slot has already been drained — the
/// second connect hangs. Tests that cross the boundary pause long enough
/// for the earlier sends to age out of the rate window.
const RATE_LIMIT_DRAIN_DELAY: Duration = Duration::from_millis(800);

/// Insert a single seeded PB so that `!lb` produces a deterministic reply
/// (instead of the empty-state message).
fn seeded_lb_with_alice() -> HashMap<String, PersonalBest> {
    let mut seeded: HashMap<String, PersonalBest> = HashMap::new();
    seeded.insert(
        "alice".into(),
        PersonalBest {
            ms: 234,
            date: NaiveDate::from_ymd_opt(2026, 4, 17).unwrap(),
        },
    );
    seeded
}

/// Sanity check: `!lb` yields a non-empty leaderboard reply mentioning alice.
/// Used after tests that should NOT actually have suspended the command.
async fn assert_lb_works(bot: &mut TestBot) {
    bot.send("alice", "!lb").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(out.contains("alice"), "expected !lb to reply, got: {out}");
}

#[tokio::test]
#[serial]
async fn broadcaster_can_suspend_lb_then_unsuspend() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .spawn()
        .await;

    // Broadcaster suspends !lb for 1m.
    bot.send_as_broadcaster("broadcaster", "!suspend lb 1m")
        .await;
    let confirm = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        confirm.contains("!lb") && confirm.contains("gesperrt"),
        "expected suspend confirmation, got: {confirm}"
    );
    assert!(
        confirm.contains("1m"),
        "expected duration 1m in confirmation, got: {confirm}"
    );

    // Regular user triggers !lb -> silently skipped.
    bot.send("alice", "!lb").await;
    bot.expect_silent(Duration::from_millis(500)).await;

    // Broadcaster unsuspends.
    bot.send_as_broadcaster("broadcaster", "!unsuspend lb")
        .await;
    let unsusp = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        unsusp.contains("!lb") && unsusp.contains("entsperrt"),
        "expected unsuspend confirmation, got: {unsusp}"
    );

    // Now !lb works again.
    assert_lb_works(&mut bot).await;

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn mod_can_suspend_lb() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .spawn()
        .await;

    bot.send_as_mod("modguy", "!suspend lb 1m").await;
    let confirm = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        confirm.contains("!lb") && confirm.contains("gesperrt"),
        "expected suspend confirmation from mod, got: {confirm}"
    );

    bot.send("alice", "!lb").await;
    bot.expect_silent(Duration::from_millis(500)).await;

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn hidden_admin_can_suspend_lb() {
    // irc_line::privmsg sets user-id=67890; add it as a hidden admin.
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .with_config(|c| {
            c.twitch.hidden_admins = vec!["67890".into()];
        })
        .spawn()
        .await;

    // Plain send() -> no badges, but user-id matches hidden_admins.
    bot.send("sneaky", "!suspend lb 1m").await;
    let confirm = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        confirm.contains("!lb") && confirm.contains("gesperrt"),
        "expected hidden-admin suspend confirmation, got: {confirm}"
    );

    bot.send("alice", "!lb").await;
    bot.expect_silent(Duration::from_millis(500)).await;

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn non_admin_cannot_suspend() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .spawn()
        .await;

    bot.send("random_user", "!suspend lb").await;
    let rejection = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        rejection.contains("darfst du nicht") || rejection.contains("FDM"),
        "expected rejection, got: {rejection}"
    );

    // !lb must still work since the suspend never took effect.
    assert_lb_works(&mut bot).await;

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn exempt_commands_rejected() {
    let mut bot = TestBotBuilder::new().spawn().await;

    for exempt in ["suspend", "unsuspend", "p"] {
        bot.send_as_broadcaster("broadcaster", &format!("!suspend {exempt}"))
            .await;
        let reply = bot.expect_say(Duration::from_secs(2)).await;
        assert!(
            reply.contains("kann nicht gesperrt werden"),
            "expected exempt rejection for '{exempt}', got: {reply}"
        );
    }

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn default_duration_used_when_omitted() {
    // Uses the built-in default of 600s = 10m.
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .spawn()
        .await;

    bot.send_as_broadcaster("broadcaster", "!suspend lb").await;
    let confirm = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        confirm.contains("10m"),
        "expected default duration rendered as '10m', got: {confirm}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn custom_default_from_config() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .with_config(|c| {
            c.suspend.default_duration_secs = 90;
        })
        .spawn()
        .await;

    bot.send_as_broadcaster("broadcaster", "!suspend lb").await;
    let confirm = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        confirm.contains("1m 30s"),
        "expected custom default rendered as '1m 30s', got: {confirm}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn bad_duration_format_rejected() {
    let mut bot = TestBotBuilder::new()
        .with_seeded_leaderboard(seeded_lb_with_alice())
        .spawn()
        .await;

    bot.send_as_broadcaster("broadcaster", "!suspend lb wat")
        .await;
    let reply = bot.expect_say(Duration::from_secs(2)).await;
    // Actual reply for UnknownUnit: "Unbekannte Einheit. Erlaubt: s, m, h, d FDM".
    // Check for the unit-list substring so minor wording changes don't break
    // the test.
    assert!(
        reply.contains("s, m, h, d"),
        "expected accepted-units list in rejection, got: {reply}"
    );

    // Suspend did not take effect.
    assert_lb_works(&mut bot).await;

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn unsuspend_unknown_command_replies_not_suspended() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send_as_broadcaster("broadcaster", "!unsuspend lb")
        .await;
    let reply = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        reply.contains("war nicht gesperrt"),
        "expected 'war nicht gesperrt', got: {reply}"
    );

    bot.shutdown().await;
}

#[tokio::test]
#[serial]
async fn ping_can_be_suspended() {
    let mut bot = TestBotBuilder::new().spawn().await;

    // Create ping as broadcaster.
    bot.send_as_broadcaster("broadcaster", "!p create hi yo {mentions}")
        .await;
    let _ = bot.expect_say(Duration::from_secs(2)).await;

    // Both users join so !hi can fire (default `pings.public = false`
    // requires the triggering user to be a member) and has someone to
    // mention.
    bot.send("alice", "!p join hi").await;
    let _ = bot.expect_say(Duration::from_secs(2)).await;
    bot.send("bob", "!p join hi").await;
    let _ = bot.expect_say(Duration::from_secs(2)).await;

    // alice triggers !hi -> reply mentioning bob (not sender).
    bot.send("alice", "!hi").await;
    let before = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        before.contains("@bob"),
        "expected ping trigger to mention bob, got: {before}"
    );

    tokio::time::sleep(RATE_LIMIT_DRAIN_DELAY).await;

    // Suspend the ping by name. Broadcaster-gated admin command.
    bot.send_as_broadcaster("broadcaster", "!suspend hi 1m")
        .await;
    let confirm = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        confirm.contains("!hi") && confirm.contains("gesperrt"),
        "expected suspend confirmation for !hi, got: {confirm}"
    );

    // Trigger by bob (sender) now silent (suspended).
    bot.send("bob", "!hi").await;
    bot.expect_silent(Duration::from_millis(500)).await;

    tokio::time::sleep(RATE_LIMIT_DRAIN_DELAY).await;

    // Unsuspend.
    bot.send_as_broadcaster("broadcaster", "!unsuspend hi")
        .await;
    let unsusp = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        unsusp.contains("!hi") && unsusp.contains("entsperrt"),
        "expected unsuspend confirmation for !hi, got: {unsusp}"
    );

    // Bob triggers — the pre-suspend trigger came from alice, so the 300s
    // per-ping cooldown still applies. Assert that the bot routes the
    // trigger through the handler (replying with cooldown message) instead
    // of silently dropping it, which proves the ping is no longer
    // suspended.
    tokio::time::sleep(RATE_LIMIT_DRAIN_DELAY).await;
    bot.send("bob", "!hi").await;
    let after = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        after.contains("warte") || after.contains("@alice"),
        "expected ping trigger to route after unsuspend (cooldown msg or mention), got: {after}"
    );

    bot.shutdown().await;
}
