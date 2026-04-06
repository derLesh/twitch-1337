use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::message::PrivmsgMessage;

use crate::AuthenticatedTwitchClient;

pub mod ai;
pub mod feedback;
pub mod flights;
pub mod flights_above;
pub mod leaderboard;
pub mod list_pings;
pub mod random_flight;
pub mod toggle_ping;
pub mod track;
pub mod untrack;

/// Context passed to every command execution.
pub struct CommandContext<'a> {
    /// The chat message that triggered the command.
    pub privmsg: &'a PrivmsgMessage,
    /// The IRC client for sending responses.
    pub client: &'a Arc<AuthenticatedTwitchClient>,
    /// Remaining words after the command name.
    pub args: Vec<&'a str>,
}

/// Trait implemented by all bot commands.
#[async_trait]
pub trait Command: Send + Sync {
    /// The command trigger including "!" prefix (e.g., "!lb").
    fn name(&self) -> &str;

    /// Whether the command is currently enabled.
    fn enabled(&self) -> bool {
        true
    }

    /// Execute the command with the given context.
    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()>;
}
