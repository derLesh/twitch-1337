use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext, normalize_username};
use crate::twitch::handlers::tracker_1337::PersonalBest;

pub struct PbCommand {
    leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,
}

impl PbCommand {
    pub fn new(leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>) -> Self {
        Self { leaderboard }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for PbCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!pb"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let target = ctx
            .args
            .first()
            .map(|arg| normalize_username(arg))
            .unwrap_or_else(|| ctx.privmsg.sender.login.clone());
        let is_self = target == ctx.privmsg.sender.login;

        let pb = self.leaderboard.read().await.get(&target).cloned();

        let response = match (pb, is_self) {
            (Some(pb), true) => {
                format!("Dein PB ist {}ms am {}", pb.ms, pb.date.format("%d.%m.%Y"))
            }
            (Some(pb), false) => format!(
                "PB von {target}: {}ms am {}",
                pb.ms,
                pb.date.format("%d.%m.%Y")
            ),
            (None, true) => "Du hast noch keinen PB".to_string(),
            (None, false) => format!("{target} hat noch keinen PB"),
        };

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send !pb response");
        }

        Ok(())
    }
}
