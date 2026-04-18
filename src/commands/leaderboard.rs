use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::error;

use super::{Command, CommandContext};

/// A user's personal best time for the 1337 challenge.
///
/// Tracks the fastest sub-1-second message time and the date it was achieved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalBest {
    /// Milliseconds after 13:37:00.000 (0-999)
    pub ms: u64,
    /// The date (Europe/Berlin) when this record was set
    pub date: chrono::NaiveDate,
}

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

        let response = if let Some((username, pb)) = leaderboard.iter().min_by_key(|(_, pb)| pb.ms)
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
