use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use tracing::{debug, error};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::cooldown::format_cooldown_remaining;
use crate::ping::{PingManager, TriggerDecision};

pub struct PingTriggerCommand {
    ping_manager: Arc<RwLock<PingManager>>,
    settings: crate::settings::SettingsHandle,
}

impl PingTriggerCommand {
    pub fn new(
        ping_manager: Arc<RwLock<PingManager>>,
        settings: crate::settings::SettingsHandle,
    ) -> Self {
        Self {
            ping_manager,
            settings,
        }
    }

    fn current_cooldown(&self) -> Duration {
        Duration::from_secs(self.settings.load().pings.cooldown)
    }

    fn current_public(&self) -> bool {
        self.settings.load().pings.public
    }
}

/// Extract the ping name from a trigger word.
/// Only accepts `!name` (case-insensitive).
fn parse_ping_trigger(word: &str) -> Option<String> {
    let name = word.strip_prefix('!')?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_lowercase())
}

#[async_trait]
impl<T, L> Command<T, L> for PingTriggerCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        // Not used for matching -- matches() is overridden
        "!<ping>"
    }

    fn matches(&self, word: &str) -> bool {
        let Some(name) = word.strip_prefix('!') else {
            return false;
        };

        if name.is_empty() {
            return false;
        }

        // Use try_read to avoid blocking the dispatcher on a write lock
        let manager = match self.ping_manager.try_read() {
            Ok(m) => m,
            Err(_) => return false,
        };
        // Case-insensitive check avoids the heap allocation of to_lowercase()
        manager.ping_exists_ignore_case(name)
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let Some(ping_name) = parse_ping_trigger(ctx.trigger) else {
            return Ok(());
        };
        let sender = &ctx.privmsg.sender.login;

        let decision = {
            let mut manager = self.ping_manager.write().await;
            manager.try_record_trigger(
                &ping_name,
                sender,
                self.current_cooldown(),
                self.current_public(),
            )
        };

        let rendered = match decision {
            TriggerDecision::Skip => return Ok(()),
            TriggerDecision::OnCooldown(remaining) => {
                debug!(ping = %ping_name, "Ping on cooldown");
                if let Err(e) = ctx
                    .client
                    .say_in_reply_to(
                        ctx.privmsg,
                        format!(
                            "Bitte warte noch {} Waiting",
                            format_cooldown_remaining(remaining)
                        ),
                    )
                    .await
                {
                    error!(error = ?e, "Failed to send cooldown message");
                }
                return Ok(());
            }
            TriggerDecision::Fire(rendered) => rendered,
        };

        ctx.client
            .say(ctx.privmsg.channel_login.clone(), rendered)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod settings_live_tests {
    use std::sync::Arc;

    use super::*;
    use crate::settings::Settings;

    #[test]
    fn reads_cooldown_and_public_from_handle_at_call_time() {
        let initial = Settings::compiled_defaults();
        let handle: crate::settings::SettingsHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(initial));
        let mgr = Arc::new(tokio::sync::RwLock::new(crate::ping::PingManager::empty()));
        let cmd = PingTriggerCommand::new(mgr.clone(), handle.clone());
        // Snapshot the values seen by the command before and after a swap.
        let before_cooldown = cmd.current_cooldown();
        let before_public = cmd.current_public();
        let mut next = Settings::compiled_defaults();
        next.pings.cooldown = 7;
        next.pings.public = true;
        handle.store(Arc::new(next));
        let after_cooldown = cmd.current_cooldown();
        let after_public = cmd.current_public();
        assert_ne!(before_cooldown, after_cooldown);
        assert_eq!(after_cooldown, std::time::Duration::from_secs(7));
        assert!(!before_public);
        assert!(after_public);
    }
}
