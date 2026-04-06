use async_trait::async_trait;
use eyre::Result;
use tokio::sync::mpsc;
use tracing::error;

use super::{Command, CommandContext};
use crate::flight_tracker::{FlightIdentifier, TrackerCommand};

pub struct TrackCommand {
    tracker_tx: mpsc::Sender<TrackerCommand>,
}

impl TrackCommand {
    pub fn new(tracker_tx: mpsc::Sender<TrackerCommand>) -> Self {
        Self { tracker_tx }
    }
}

#[async_trait]
impl Command for TrackCommand {
    fn name(&self) -> &str {
        "!track"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let input = ctx.args.join(" ");
        if input.trim().is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, "Benutzung: !track <callsign/hex> FDM".to_string())
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        let identifier = FlightIdentifier::parse(&input);
        let cmd = TrackerCommand::Track {
            identifier,
            requested_by: ctx.privmsg.sender.login.clone(),
            reply_to: ctx.privmsg.clone(),
        };

        if let Err(e) = self.tracker_tx.send(cmd).await {
            error!(error = ?e, "Failed to send track command to flight tracker");
        }

        Ok(())
    }
}
