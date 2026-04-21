use std::path::PathBuf;

use async_trait::async_trait;
use eyre::Result;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::time::Duration;
use tracing::{error, info};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};

const FEEDBACK_FILENAME: &str = "feedback.txt";

pub struct FeedbackCommand {
    data_dir: PathBuf,
    cooldown: PerUserCooldown,
}

impl FeedbackCommand {
    pub fn new(data_dir: PathBuf, cooldown: Duration) -> Self {
        Self {
            data_dir,
            cooldown: PerUserCooldown::new(cooldown),
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for FeedbackCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!fb"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        let message: String = ctx.args.join(" ");

        // Check for empty message
        if message.trim().is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, "Benutzung: !fb <nachricht>".to_string())
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        // Check cooldown
        if let Some(remaining) = self.cooldown.check(user).await {
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

        self.cooldown.record(user).await;

        // Write feedback to file
        let now = chrono::Utc::now()
            .with_timezone(&chrono_tz::Europe::Berlin)
            .format("%Y-%m-%dT%H:%M:%S");
        let line = format!("{now} {user}: {message}\n");

        let path = self.data_dir.join(FEEDBACK_FILENAME);
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(line.as_bytes()).await {
                    error!(error = ?e, "Failed to write feedback to file");
                    if let Err(e) = ctx
                        .client
                        .say_in_reply_to(ctx.privmsg, "Da ist was schiefgelaufen FDM".to_string())
                        .await
                    {
                        error!(error = ?e, "Failed to send error message");
                    }
                    return Ok(());
                }
            }
            Err(e) => {
                error!(error = ?e, "Failed to open feedback file");
                if let Err(e) = ctx
                    .client
                    .say_in_reply_to(ctx.privmsg, "Da ist was schiefgelaufen FDM".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send error message");
                }
                return Ok(());
            }
        }

        info!(user = %user, "Feedback saved");

        if let Err(e) = ctx
            .client
            .say_in_reply_to(ctx.privmsg, "Feedback gespeichert Okayge".to_string())
            .await
        {
            error!(error = ?e, "Failed to send confirmation message");
        }

        Ok(())
    }
}
