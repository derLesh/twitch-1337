mod common;

use std::time::Duration;

use common::{TestBot, TestBotBuilder};
use serial_test::serial;

/// Create a ping named "hi" as broadcaster, then have alice and bob join.
/// Consumes their ack messages so the caller starts with a clean capture queue.
async fn setup_hi_ping(bot: &mut TestBot) {
    bot.send_as_broadcaster("broadcaster", "!p create hi yo {mentions}")
        .await;
    let _ = bot.expect_say(Duration::from_secs(2)).await;

    bot.send("alice", "!p join hi").await;
    let _ = bot.expect_say(Duration::from_secs(1)).await;

    bot.send("bob", "!p join hi").await;
    let _ = bot.expect_say(Duration::from_secs(1)).await;
}

/// Create a ping as broadcaster, have two users join, then verify the trigger
/// renders mentions correctly (template has no {sender}, so sender is included).
#[tokio::test]
#[serial]
async fn ping_trigger_renders_template_with_mentions() {
    let mut bot = TestBotBuilder::new().spawn().await;

    setup_hi_ping(&mut bot).await;

    // Alice triggers !hi. Template "yo {mentions}" has no {sender},
    // so both alice and bob appear in mentions.
    bot.send("alice", "!hi").await;
    let out = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        out.contains("@bob"),
        "expected @bob in trigger output: {out}"
    );
    assert!(
        out.contains("@alice"),
        "sender alice should be included when template has no {{sender}}: {out}"
    );

    bot.shutdown().await;
}

/// After the first trigger, a second immediate trigger should be blocked by
/// the cooldown and return a "Bitte warte noch … Waiting" reply.
#[tokio::test]
#[serial]
async fn ping_cooldown_blocks_second_trigger() {
    let mut bot = TestBotBuilder::new()
        .with_config(|c| {
            c.pings.cooldown = 60;
        })
        .spawn()
        .await;

    setup_hi_ping(&mut bot).await;

    // First trigger fires successfully.
    bot.send("alice", "!hi").await;
    let first = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        first.contains("@bob"),
        "first trigger should mention bob: {first}"
    );

    // The handler records the cooldown timestamp after sending the response;
    // yield briefly so the write lands before we fire the second trigger.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Second trigger while cooldown active.
    // Real reply: "Bitte warte noch {remaining} Waiting"
    bot.send("alice", "!hi").await;
    let second = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        second.contains("Bitte warte noch") && second.contains("Waiting"),
        "expected cooldown reply, got: {second}"
    );

    bot.shutdown().await;
}

/// A non-admin user attempting `!p create` should be rejected and the ping
/// must not be created (triggering !evil afterward stays silent).
#[tokio::test]
#[serial]
async fn ping_admin_rejects_non_admin_create() {
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("random_user", "!p create evil yo {mentions}")
        .await;
    // Real rejection: "Das darfst du nicht FDM"
    let rejection = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        rejection.contains("Das darfst du nicht"),
        "expected rejection message, got: {rejection}"
    );

    // Verify the ping was NOT created: triggering !evil stays silent.
    bot.send("random_user", "!evil").await;
    bot.expect_silent(Duration::from_millis(500)).await;

    bot.shutdown().await;
}
