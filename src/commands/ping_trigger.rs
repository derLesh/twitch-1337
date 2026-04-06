use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use tracing::debug;

use crate::ping::PingManager;
use super::{Command, CommandContext};

pub struct PingTriggerCommand {
    ping_manager: Arc<RwLock<PingManager>>,
    default_cooldown: u64,
}

impl PingTriggerCommand {
    pub fn new(ping_manager: Arc<RwLock<PingManager>>, default_cooldown: u64) -> Self {
        Self {
            ping_manager,
            default_cooldown,
        }
    }
}

#[async_trait]
impl Command for PingTriggerCommand {
    fn name(&self) -> &str {
        // Not used for matching -- matches() is overridden
        "!<ping>"
    }

    fn matches(&self, word: &str) -> bool {
        // word includes "!" prefix, e.g. "!dbd"
        let name = word.strip_prefix('!').unwrap_or(word);
        // Use try_read to avoid blocking the dispatcher on a write lock
        let manager = match self.ping_manager.try_read() {
            Ok(m) => m,
            Err(_) => return false,
        };
        manager.ping_exists(name)
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let trigger = ctx.privmsg.message_text.split_whitespace().next().unwrap_or("");
        let ping_name = trigger.strip_prefix('!').unwrap_or(trigger);
        let sender = &ctx.privmsg.sender.login;

        // Acquire read lock first to check conditions
        {
            let manager = self.ping_manager.read().await;

            // Only members can trigger
            if !manager.is_member(ping_name, sender) {
                return Ok(());
            }

            // Check cooldown
            if !manager.check_cooldown(ping_name, self.default_cooldown) {
                debug!(ping = ping_name, "Ping on cooldown, ignoring");
                return Ok(());
            }

            // Render template
            let Some(rendered) = manager.render_template(ping_name, sender) else {
                return Ok(());
            };

            // Send the ping message (not as reply, just to the channel)
            ctx.client
                .say(ctx.privmsg.channel_login.clone(), rendered)
                .await?;
        }

        // Acquire write lock to record trigger
        {
            let mut manager = self.ping_manager.write().await;
            manager.record_trigger(ping_name);
        }

        Ok(())
    }
}
