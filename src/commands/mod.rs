use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::PrivmsgMessage, transport::Transport,
};

pub mod feedback;
pub mod leaderboard;
pub mod news;
pub mod ping_admin;
pub mod ping_trigger;
pub mod suspend;

/// German rejection reply used by admin-gated commands when the sender
/// lacks the required badge or id.
pub const ADMIN_DENIED_MSG: &str = "Das darfst du nicht FDM";

/// Returns true if the author of `privmsg` is allowed to run admin commands:
/// carries a broadcaster or moderator badge, or their user id is in
/// `hidden_admin_ids`.
pub fn is_admin(privmsg: &PrivmsgMessage, hidden_admin_ids: &[String]) -> bool {
    for badge in &privmsg.badges {
        if badge.name == "broadcaster" || badge.name == "moderator" {
            return true;
        }
    }
    hidden_admin_ids.contains(&privmsg.sender.id)
}

/// Normalize a command name from user input: strip leading `!`s and
/// ASCII-lowercase. Used both by admin suspend commands and by the
/// dispatcher's suspension lookup — they MUST agree.
pub fn normalize_command_name(raw: &str) -> String {
    raw.trim_start_matches('!').to_ascii_lowercase()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_command_name_strips_bangs_and_lowercases() {
        assert_eq!(normalize_command_name("AI"), "ai");
        assert_eq!(normalize_command_name("!AI"), "ai");
        assert_eq!(normalize_command_name("!!foo"), "foo");
        assert_eq!(normalize_command_name("track"), "track");
    }
}
