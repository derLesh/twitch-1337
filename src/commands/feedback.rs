use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{error, info};

use super::{Command, CommandContext};

const FEEDBACK_COOLDOWN: Duration = Duration::from_secs(300);
const FEEDBACK_FILENAME: &str = "feedback.txt";

pub struct FeedbackCommand {
    data_dir: PathBuf,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl FeedbackCommand {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for FeedbackCommand {
    fn name(&self) -> &str {
        "!fb"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        let message: String = ctx.args.join(" ");

        // Check for empty message
        if message.trim().is_empty() {
            if let Err(e) = ctx.client
                .say_in_reply_to(ctx.privmsg, "Benutzung: !fb <nachricht>".to_string())
                .await
            {
                error!(error = ?e, "Failed to send usage message");
            }
            return Ok(());
        }

        // Check cooldown
        {
            let cooldowns_guard = self.cooldowns.lock().await;
            if let Some(last_use) = cooldowns_guard.get(user) {
                let elapsed = last_use.elapsed();
                if elapsed < FEEDBACK_COOLDOWN {
                    let remaining = (FEEDBACK_COOLDOWN - elapsed).as_secs();
                    if let Err(e) = ctx.client
                        .say_in_reply_to(
                            ctx.privmsg,
                            format!("Bitte warte noch {remaining}s Waiting"),
                        )
                        .await
                    {
                        error!(error = ?e, "Failed to send cooldown message");
                    }
                    return Ok(());
                }
            }
        }

        // Update cooldown
        {
            let mut cooldowns_guard = self.cooldowns.lock().await;
            cooldowns_guard.insert(user.to_string(), std::time::Instant::now());
        }

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
                    if let Err(e) = ctx.client
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
                if let Err(e) = ctx.client
                    .say_in_reply_to(ctx.privmsg, "Da ist was schiefgelaufen FDM".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send error message");
                }
                return Ok(());
            }
        }

        info!(user = %user, "Feedback saved");

        if let Err(e) = ctx.client
            .say_in_reply_to(ctx.privmsg, "Feedback gespeichert Okayge".to_string())
            .await
        {
            error!(error = ?e, "Failed to send confirmation message");
        }

        Ok(())
    }
}
