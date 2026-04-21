use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use twitch_irc::{login::LoginCredentials, message::PrivmsgMessage, transport::Transport};

use crate::ping::PingManager;

use super::{Command, CommandContext};

/// Normalize a ping name from user input to the canonical lowercase form.
fn normalize_ping_name(name: &str) -> String {
    name.to_lowercase()
}

pub struct PingAdminCommand {
    ping_manager: Arc<RwLock<PingManager>>,
    hidden_admin_ids: Vec<String>,
}

impl PingAdminCommand {
    pub fn new(ping_manager: Arc<RwLock<PingManager>>, hidden_admin_ids: Vec<String>) -> Self {
        Self {
            ping_manager,
            hidden_admin_ids,
        }
    }

    fn is_admin(&self, privmsg: &PrivmsgMessage) -> bool {
        for badge in &privmsg.badges {
            if badge.name == "broadcaster" || badge.name == "moderator" {
                return true;
            }
        }
        self.hidden_admin_ids.contains(&privmsg.sender.id)
    }
}

#[async_trait]
impl<T, L> Command<T, L> for PingAdminCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!p"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let subcommand = ctx.args.first().copied().unwrap_or("");

        match subcommand {
            "create" | "delete" | "edit" | "add" | "remove" => {
                if !self.is_admin(ctx.privmsg) {
                    ctx.client
                        .say_in_reply_to(ctx.privmsg, "Das darfst du nicht FDM".to_string())
                        .await?;
                    return Ok(());
                }
                match subcommand {
                    "create" => self.handle_create(&ctx).await,
                    "delete" => self.handle_delete(&ctx).await,
                    "edit" => self.handle_edit(&ctx).await,
                    "add" => self.handle_member_op(&ctx, "add").await,
                    "remove" => self.handle_member_op(&ctx, "remove").await,
                    _ => unreachable!(),
                }
            }
            "join" => self.handle_self_op(&ctx, "join").await,
            "leave" => self.handle_self_op(&ctx, "leave").await,
            "list" => self.handle_list(&ctx).await,
            _ => {
                ctx.client
                    .say_in_reply_to(
                        ctx.privmsg,
                        "Nutze: join, leave, list (oder create, delete, edit, add, remove als Mod)"
                            .to_string(),
                    )
                    .await?;
                Ok(())
            }
        }
    }
}

impl PingAdminCommand {
    /// !p create <name> <template...>
    async fn handle_create<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        if ctx.args.len() < 3 {
            ctx.client
                .say_in_reply_to(
                    ctx.privmsg,
                    "Nutze: !p create <name> <template>".to_string(),
                )
                .await?;
            return Ok(());
        }

        let name = normalize_ping_name(ctx.args[1]);
        let template = ctx.args[2..].join(" ");

        let mut manager = self.ping_manager.write().await;
        match manager.create_ping(
            name.clone(),
            template,
            ctx.privmsg.sender.login.clone(),
            None,
        ) {
            Ok(()) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("Ping \"{name}\" erstellt Okayge"))
                    .await?;
            }
            Err(e) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("{e} FDM"))
                    .await?;
            }
        }
        Ok(())
    }

    /// !p delete <name>
    async fn handle_delete<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        let name = match ctx.args.get(1) {
            Some(n) => normalize_ping_name(n),
            None => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, "Nutze: !p delete <name>".to_string())
                    .await?;
                return Ok(());
            }
        };

        let mut manager = self.ping_manager.write().await;
        match manager.delete_ping(&name) {
            Ok(()) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("Ping \"{name}\" gelöscht Okayge"))
                    .await?;
            }
            Err(e) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("{e} FDM"))
                    .await?;
            }
        }
        Ok(())
    }

    /// !p edit <name> <new template...>
    async fn handle_edit<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        if ctx.args.len() < 3 {
            ctx.client
                .say_in_reply_to(ctx.privmsg, "Nutze: !p edit <name> <template>".to_string())
                .await?;
            return Ok(());
        }

        let name = normalize_ping_name(ctx.args[1]);
        let template = ctx.args[2..].join(" ");

        let mut manager = self.ping_manager.write().await;
        match manager.edit_template(&name, template) {
            Ok(()) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("Ping \"{name}\" updated SeemsGood"))
                    .await?;
            }
            Err(e) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("{e} FDM"))
                    .await?;
            }
        }
        Ok(())
    }

    /// !p add/remove <name> <user>
    async fn handle_member_op<T, L>(&self, ctx: &CommandContext<'_, T, L>, op: &str) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        if ctx.args.len() < 3 {
            ctx.client
                .say_in_reply_to(ctx.privmsg, format!("Nutze: !p {op} <name> <user>"))
                .await?;
            return Ok(());
        }

        let name = normalize_ping_name(ctx.args[1]);
        let user = ctx.args[2].trim_start_matches('@').to_lowercase();

        let mut manager = self.ping_manager.write().await;
        let result = match op {
            "add" => manager.add_member(&name, &user),
            "remove" => manager.remove_member(&name, &user),
            _ => unreachable!(),
        };

        match result {
            Ok(()) => {
                let msg = match op {
                    "add" => format!("{user} zu \"{name}\" hinzugefügt Okayge"),
                    "remove" => format!("{user} aus \"{name}\" entfernt Okayge"),
                    _ => unreachable!(),
                };
                ctx.client.say_in_reply_to(ctx.privmsg, msg).await?;
            }
            Err(e) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("{e} FDM"))
                    .await?;
            }
        }
        Ok(())
    }

    /// !p join/leave <name> -- self-service membership
    async fn handle_self_op<T, L>(&self, ctx: &CommandContext<'_, T, L>, op: &str) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        let name = match ctx.args.get(1) {
            Some(n) => normalize_ping_name(n),
            None => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, format!("Nutze: !p {op} <name>"))
                    .await?;
                return Ok(());
            }
        };

        let mut manager = self.ping_manager.write().await;
        let result = match op {
            "join" => manager.add_member(&name, &ctx.privmsg.sender.login),
            "leave" => manager.remove_member(&name, &ctx.privmsg.sender.login),
            _ => unreachable!(),
        };

        match result {
            Ok(()) => {
                ctx.client
                    .say_in_reply_to(ctx.privmsg, "Hab ich gemacht Okayge".to_string())
                    .await?;
            }
            Err(e) => {
                let err_str = e.to_string();
                let msg = if err_str.contains("gibt es nicht") {
                    format!("{err_str} FDM")
                } else {
                    match op {
                        "join" => "Bist du schon FDM".to_string(),
                        "leave" => "Bist du nicht drin FDM".to_string(),
                        _ => unreachable!(),
                    }
                };
                ctx.client.say_in_reply_to(ctx.privmsg, msg).await?;
            }
        }
        Ok(())
    }

    /// !p list
    async fn handle_list<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        let manager = self.ping_manager.read().await;
        let pings = manager.list_pings_for_user(&ctx.privmsg.sender.login);

        let response = if pings.is_empty() {
            "Keine Pings".to_string()
        } else {
            pings.join(" ")
        };

        ctx.client.say_in_reply_to(ctx.privmsg, response).await?;
        Ok(())
    }
}
