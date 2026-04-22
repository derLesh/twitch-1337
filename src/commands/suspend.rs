//! Admin commands for transiently suspending other bot commands.
//!
//! `!suspend <command> [duration]` silences another command for the given
//! duration (defaults to [`SuspendConfig::default_duration_secs`]).
//! `!unsuspend <command>` lifts an active suspension.
//!
//! Both commands are gated to broadcaster/moderator badges or user ids listed
//! in `twitch.hidden_admins`. The commands `suspend`, `unsuspend`, and `p`
//! cannot be suspended (enforced in [`SuspendCommand`]).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{ADMIN_DENIED_MSG, Command, CommandContext, is_admin, normalize_command_name};
use crate::cooldown::format_cooldown_remaining;
use crate::suspend::{ParseDurationError, SuspensionManager, parse_duration};

/// Command names that must never be suspendable. Kept lowercase; the key
/// passed from user input is normalized the same way before comparison.
const EXEMPT_COMMANDS: &[&str] = &["suspend", "unsuspend", "p"];

/// Map a [`ParseDurationError`] to a user-facing German message.
fn duration_error_message(err: &ParseDurationError) -> String {
    match err {
        ParseDurationError::Empty => "Dauer fehlt. Nutze z.B. 30s, 10m, 2h, 1d FDM".to_string(),
        ParseDurationError::InvalidNumber => {
            "Ungültige Zahl. Nutze z.B. 30s, 10m, 2h, 1d FDM".to_string()
        }
        ParseDurationError::UnknownUnit => {
            "Unbekannte Einheit. Erlaubt: s, m, h, d FDM".to_string()
        }
        ParseDurationError::Zero => "Dauer muss größer als 0 sein FDM".to_string(),
        ParseDurationError::TooLong => "Dauer zu lang (max 7 Tage) FDM".to_string(),
    }
}

pub struct SuspendCommand {
    manager: Arc<SuspensionManager>,
    hidden_admin_ids: Vec<String>,
    default_duration: Duration,
}

impl SuspendCommand {
    pub fn new(
        manager: Arc<SuspensionManager>,
        hidden_admin_ids: Vec<String>,
        default_duration: Duration,
    ) -> Self {
        Self {
            manager,
            hidden_admin_ids,
            default_duration,
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for SuspendCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!suspend"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        if !is_admin(ctx.privmsg, &self.hidden_admin_ids) {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, ADMIN_DENIED_MSG.to_string())
                .await
            {
                error!(error = ?e, "Failed to send admin-gate reply");
            }
            return Ok(());
        }

        let raw_cmd = match ctx.args.first() {
            Some(c) => *c,
            None => {
                if let Err(e) = ctx
                    .client
                    .say_in_reply_to(ctx.privmsg, "Nutze: !suspend <command> [dauer]".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send usage reply");
                }
                return Ok(());
            }
        };

        let cmd = normalize_command_name(raw_cmd);

        if EXEMPT_COMMANDS.contains(&cmd.as_str()) {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    "Das kann nicht gesperrt werden FDM".to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send exempt reply");
            }
            return Ok(());
        }

        let duration = match ctx.args.get(1) {
            None => self.default_duration,
            Some(s) => match parse_duration(s) {
                Ok(d) => d,
                Err(err) => {
                    if let Err(e) = ctx
                        .client
                        .say_in_reply_to(ctx.privmsg, duration_error_message(&err))
                        .await
                    {
                        error!(error = ?e, "Failed to send duration-error reply");
                    }
                    return Ok(());
                }
            },
        };

        self.manager.suspend(&cmd, duration).await;

        if let Err(e) = ctx
            .client
            .say_in_reply_to(
                ctx.privmsg,
                format!(
                    "!{cmd} gesperrt für {}",
                    format_cooldown_remaining(duration)
                ),
            )
            .await
        {
            error!(error = ?e, "Failed to send suspend confirmation");
        }

        Ok(())
    }
}

pub struct UnsuspendCommand {
    manager: Arc<SuspensionManager>,
    hidden_admin_ids: Vec<String>,
}

impl UnsuspendCommand {
    pub fn new(manager: Arc<SuspensionManager>, hidden_admin_ids: Vec<String>) -> Self {
        Self {
            manager,
            hidden_admin_ids,
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for UnsuspendCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!unsuspend"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        if !is_admin(ctx.privmsg, &self.hidden_admin_ids) {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, ADMIN_DENIED_MSG.to_string())
                .await
            {
                error!(error = ?e, "Failed to send admin-gate reply");
            }
            return Ok(());
        }

        let raw_cmd = match ctx.args.first() {
            Some(c) => *c,
            None => {
                if let Err(e) = ctx
                    .client
                    .say_in_reply_to(ctx.privmsg, "Nutze: !unsuspend <command>".to_string())
                    .await
                {
                    error!(error = ?e, "Failed to send usage reply");
                }
                return Ok(());
            }
        };

        let cmd = normalize_command_name(raw_cmd);

        let reply = if self.manager.unsuspend(&cmd).await {
            format!("!{cmd} entsperrt Okayge")
        } else {
            format!("!{cmd} war nicht gesperrt FDM")
        };

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, reply).await {
            error!(error = ?e, "Failed to send unsuspend reply");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exempt_list_covers_required_commands() {
        assert!(EXEMPT_COMMANDS.contains(&"suspend"));
        assert!(EXEMPT_COMMANDS.contains(&"unsuspend"));
        assert!(EXEMPT_COMMANDS.contains(&"p"));
    }

    #[test]
    fn duration_error_messages_end_in_fdm() {
        for err in [
            ParseDurationError::Empty,
            ParseDurationError::InvalidNumber,
            ParseDurationError::UnknownUnit,
            ParseDurationError::Zero,
            ParseDurationError::TooLong,
        ] {
            let msg = duration_error_message(&err);
            assert!(msg.ends_with("FDM"), "expected FDM suffix, got: {msg}");
        }
    }
}
