use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::RwLock;
use twitch_irc::message::PrivmsgMessage;

use crate::ping::PingManager;

use super::{Command, CommandContext};

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
impl Command for PingAdminCommand {
    fn name(&self) -> &str {
        "!p"
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let subcommand = ctx.args.first().copied().unwrap_or("");

        match subcommand {
            "create" | "delete" | "add" | "remove" => {
                if !self.is_admin(ctx.privmsg) {
                    ctx.client
                        .say_in_reply_to(ctx.privmsg, "Das darfst du nicht FDM".to_string())
                        .await?;
                    return Ok(());
                }
                match subcommand {
                    "create" => self.handle_create(&ctx).await,
                    "delete" => self.handle_delete(&ctx).await,
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
                        "Nutze: join, leave, list (oder create, delete, add, remove als Mod)"
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
    async fn handle_create(&self, ctx: &CommandContext<'_>) -> Result<()> {
        if ctx.args.len() < 3 {
            ctx.client
                .say_in_reply_to(ctx.privmsg, "Nutze: !p create <name> <template>".to_string())
                .await?;
            return Ok(());
        }

        let name = ctx.args[1].to_lowercase();
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
    async fn handle_delete(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let name = match ctx.args.get(1) {
            Some(n) => n.to_lowercase(),
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

    /// !p add/remove <name> <user>
    async fn handle_member_op(&self, ctx: &CommandContext<'_>, op: &str) -> Result<()> {
        if ctx.args.len() < 3 {
            ctx.client
                .say_in_reply_to(
                    ctx.privmsg,
                    format!("Nutze: !p {op} <name> <user>"),
                )
                .await?;
            return Ok(());
        }

        let name = ctx.args[1].to_lowercase();
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
    async fn handle_self_op(&self, ctx: &CommandContext<'_>, op: &str) -> Result<()> {
        let name = match ctx.args.get(1) {
            Some(n) => n.to_lowercase(),
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
                ctx.client
                    .say_in_reply_to(ctx.privmsg, msg)
                    .await?;
            }
        }
        Ok(())
    }

    /// !p list
    async fn handle_list(&self, ctx: &CommandContext<'_>) -> Result<()> {
        let manager = self.ping_manager.read().await;
        let pings = manager.list_pings_for_user(&ctx.privmsg.sender.login);

        let response = if pings.is_empty() {
            "Keine Pings".to_string()
        } else {
            pings.join(" ")
        };

        ctx.client
            .say_in_reply_to(ctx.privmsg, response)
            .await?;
        Ok(())
    }
}
