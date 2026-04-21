use async_trait::async_trait;
use eyre::Result;
use tokio::sync::mpsc;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::flight_tracker::TrackerCommand;

pub struct FlightsCommand {
    tracker_tx: mpsc::Sender<TrackerCommand>,
}

impl FlightsCommand {
    pub fn new(tracker_tx: mpsc::Sender<TrackerCommand>) -> Self {
        Self { tracker_tx }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for FlightsCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!flights"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let cmd = TrackerCommand::Status {
            identifier: None,
            reply_to: ctx.privmsg.clone(),
        };

        if let Err(e) = self.tracker_tx.send(cmd).await {
            error!(error = ?e, "Failed to send flights command to flight tracker");
        }

        Ok(())
    }
}

pub struct FlightCommand {
    tracker_tx: mpsc::Sender<TrackerCommand>,
}

impl FlightCommand {
    pub fn new(tracker_tx: mpsc::Sender<TrackerCommand>) -> Self {
        Self { tracker_tx }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for FlightCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!flight"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let input = ctx.args.join(" ");
        if input.trim().is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    "Benutzung: !flight <callsign/hex> FDM".to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        let cmd = TrackerCommand::Status {
            identifier: Some(input.trim().to_string()),
            reply_to: ctx.privmsg.clone(),
        };

        if let Err(e) = self.tracker_tx.send(cmd).await {
            error!(error = ?e, "Failed to send flight command to flight tracker");
        }

        Ok(())
    }
}
