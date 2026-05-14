//! `!döner` / `!doener` — Euro-Betrag in „Anzahl Döner“ umrechnen (Deutschland-Ø nach [Döneratlas](https://doeneratlas.de/)).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::{error, warn};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::doener::DoeneratlasClient;

use super::{Command, CommandContext};

fn parse_euro_amount(raw: &str) -> Option<f64> {
    let mut t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t = t.strip_suffix('€').unwrap_or(t).trim();

    match (t.contains(','), t.contains('.')) {
        (true, false) => t.replace(',', ".").parse().ok(),
        (false, true) => t.parse().ok(),
        (true, true) => {
            let last_comma = t.rfind(',').unwrap_or(0);
            let last_dot = t.rfind('.').unwrap_or(0);
            if last_comma > last_dot {
                let no_dots: String = t.chars().filter(|&c| c != '.').collect();
                no_dots.replace(',', ".").parse().ok()
            } else {
                let no_commas: String = t.chars().filter(|&c| c != ',').collect();
                no_commas.parse().ok()
            }
        }
        (false, false) => t.parse().ok(),
    }
}

fn format_eur_chat(n: f64) -> String {
    let rounded = (n * 100.0).round() / 100.0;
    if (rounded - rounded.round()).abs() < f64::EPSILON {
        format!("{}€", rounded as i64)
    } else {
        let s = format!("{rounded:.2}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        s.replace('.', ",") + "€"
    }
}

fn format_doener_count(n: f64) -> String {
    let rounded_tenth = (n * 10.0).round() / 10.0;
    if (rounded_tenth - rounded_tenth.round()).abs() < 0.051 {
        format!("{}", rounded_tenth.round() as i64)
    } else {
        format!("{:.1}", rounded_tenth).replace('.', ",")
    }
}

pub struct DoenerCalcCommand {
    client: Arc<DoeneratlasClient>,
    cooldown: PerUserCooldown,
}

impl DoenerCalcCommand {
    pub fn new(client: Arc<DoeneratlasClient>, cooldown: Duration) -> Self {
        Self {
            client,
            cooldown: PerUserCooldown::new(cooldown),
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for DoenerCalcCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!döner"
    }

    fn matches(&self, word: &str) -> bool {
        let Some(stripped) = word.strip_prefix('!') else {
            return false;
        };
        let lowered = stripped.to_lowercase();
        lowered == "döner" || lowered == "doener"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        if let Some(rem) = self.cooldown.check(user).await {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    format!(
                        "Bitte warte noch {} Waiting",
                        format_cooldown_remaining(rem)
                    ),
                )
                .await
            {
                error!(error = ?e, "Failed to send !döner cooldown");
            }
            return Ok(());
        }

        if ctx.args.is_empty() {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    "Nutze: !döner <Euro-Betrag> — Umrechnung nutzt den Döneratlas-Deutschland-Ø. Stadtpreise: !dpi <Suche> FDM".to_string(),
                )
                .await
            {
                error!(error = ?e, "Failed to send !döner usage");
            }
            return Ok(());
        }

        let amount_raw = ctx.args.join(" ");
        let response = match parse_euro_amount(&amount_raw) {
            None => "Das ist kein gültiger Euro-Betrag FDM".to_string(),
            Some(amount_eur) if amount_eur <= 0.0 => {
                "Der Betrag muss größer als 0 sein FDM".to_string()
            }
            Some(amount_eur) => match self.client.national_average_eur().await {
                Ok(avg) if avg > 0.0 => {
                    let n = amount_eur / avg;
                    format!(
                        "Das wären {} Döner (Ø {} Deutschland, Döneratlas)",
                        format_doener_count(n),
                        format_eur_chat(avg),
                    )
                }
                Ok(_) => {
                    warn!("doeneratlas returned non-positive national average");
                    "Konnte den Deutschland-Ø auf Döneratlas nicht lesen FDM".to_string()
                }
                Err(e) => {
                    warn!(error = ?e, "doeneratlas stats fetch failed");
                    "Döneratlas gerade nicht erreichbar FDM".to_string()
                }
            },
        };

        self.cooldown.record(user).await;

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send !döner response");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_amounts() {
        assert!((parse_euro_amount("25").unwrap() - 25.0).abs() < f64::EPSILON);
        assert!((parse_euro_amount("25,5").unwrap() - 25.5).abs() < 1e-9);
        assert!((parse_euro_amount("25.5").unwrap() - 25.5).abs() < f64::EPSILON);
        assert!((parse_euro_amount("1.234,56").unwrap() - 1234.56).abs() < 0.001);
    }
}
