use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use tracing::error;

use crate::PersonalBest;
use super::{Command, CommandContext};

pub struct LeaderboardCommand {
    leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,
}

impl LeaderboardCommand {
    pub fn new(leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>) -> Self {
        Self { leaderboard }
    }
}

#[async_trait]
impl Command for LeaderboardCommand {
    fn name(&self) -> &str {
        "!lb"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let leaderboard = self.leaderboard.read().await;

        let response = if let Some((username, pb)) = leaderboard
            .iter()
            .min_by_key(|(_, pb)| pb.ms)
        {
            let date = pb.date.format("%d.%m.%Y");
            format!(
                "Der schnellste 1337 ist {username} mit {}ms am {date}",
                pb.ms
            )
        } else {
            "Noch keine Einträge vorhanden".to_string()
        };

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send leaderboard response");
        }

        Ok(())
    }
}
