use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::PrivmsgMessage, transport::Transport,
};

pub mod ai;
pub mod feedback;
pub mod flights;
pub mod flights_above;
pub mod leaderboard;
pub mod ping_admin;
pub mod ping_trigger;
pub mod random_flight;
pub mod track;
pub mod untrack;

/// Context passed to every command execution.
pub struct CommandContext<'a, T: Transport, L: LoginCredentials> {
    /// The chat message that triggered the command.
    pub privmsg: &'a PrivmsgMessage,
    /// The IRC client for sending responses.
    pub client: &'a Arc<TwitchIRCClient<T, L>>,
    /// The first word of the message that matched the command.
    pub trigger: &'a str,
    /// Remaining words after the command name.
    pub args: Vec<&'a str>,
}

/// Trait implemented by all bot commands.
#[async_trait]
pub trait Command<T, L>: Send + Sync
where
    T: Transport,
    L: LoginCredentials,
{
    /// The command trigger including "!" prefix (e.g., "!lb").
    fn name(&self) -> &str;

    /// Whether the command is currently enabled.
    fn enabled(&self) -> bool {
        true
    }

    /// Whether this command matches the given trigger word.
    /// Default: exact match on `name()`.
    fn matches(&self, word: &str) -> bool {
        self.name() == word
    }

    /// Execute the command with the given context.
    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()>;
}
