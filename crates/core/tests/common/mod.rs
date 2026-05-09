#![allow(dead_code)]

pub mod fake_clock;
pub mod fake_llm;
pub mod fake_transport;
pub mod irc_line;
pub mod test_bot;

#[allow(unused_imports)]
pub use test_bot::{TestBot, TestBotBuilder};

/// Assert that the next PRIVMSG from the bot contains `text`. Returns the full
/// line so callers can do further assertions.
#[allow(dead_code)]
pub async fn wait_for_say(bot: &mut TestBot, text: &str, timeout: std::time::Duration) -> String {
    let line = bot.expect_say(timeout).await;
    assert!(
        line.contains(text),
        "expected substring {text:?} in {line:?}"
    );
    line
}
