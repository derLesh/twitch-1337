use async_trait::async_trait;
use eyre::Result;
use tokio::sync::mpsc;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::aviation::TrackerCommand;
use crate::commands::{Command, CommandContext};

pub struct UntrackCommand {
    tracker_tx: mpsc::Sender<TrackerCommand>,
}

impl UntrackCommand {
    pub fn new(tracker_tx: mpsc::Sender<TrackerCommand>) -> Self {
        Self { tracker_tx }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for UntrackCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!untrack"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let input = ctx.args.join(" ");
        if input.trim().is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    "Benutzung: !untrack <callsign/hex> FDM".to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        let is_mod = ctx
            .privmsg
            .badges
            .iter()
            .any(|b| b.name == "moderator" || b.name == "broadcaster");

        let cmd = TrackerCommand::Untrack {
            identifier: input.trim().to_string(),
            requested_by: ctx.privmsg.sender.login.clone(),
            is_mod,
            reply_to: ctx.privmsg.clone(),
        };

        if let Err(e) = self.tracker_tx.send(cmd).await {
            error!(error = ?e, "Failed to send untrack command to flight tracker");
        }

        Ok(())
    }
}
