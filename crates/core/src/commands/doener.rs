use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::time::Duration;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::doener::format::{
    api_down_message, format_city, format_did_you_mean, format_global, format_not_found,
};
use crate::doener::{CityHit, DoenerClient};

pub struct DoenerCommand {
    client: Arc<DoenerClient>,
    cooldown: PerUserCooldown,
}

impl DoenerCommand {
    pub fn new(client: Arc<DoenerClient>, cooldown: Duration) -> Self {
        Self {
            client,
            cooldown: PerUserCooldown::new(cooldown),
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for DoenerCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!dpi"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        if let Some(remaining) = self.cooldown.check(user).await {
            send(
                &ctx,
                format!(
                    "Bitte warte noch {} Waiting",
                    format_cooldown_remaining(remaining)
                ),
            )
            .await;
            return Ok(());
        }
        self.cooldown.record(user).await;

        let query = ctx.args.join(" ");
        let query = query.trim();

        let response = if query.is_empty() {
            match self.client.stats().await {
                Ok(s) => format_global(&s),
                Err(e) => {
                    error!(error = ?e, "doener stats lookup failed");
                    api_down_message().to_string()
                }
            }
        } else {
            match self.client.search_cities(query).await {
                Ok(hits) => decide_city_response(query, hits),
                Err(e) => {
                    error!(error = ?e, query, "doener city lookup failed");
                    api_down_message().to_string()
                }
            }
        };

        send(&ctx, response).await;
        Ok(())
    }
}

fn decide_city_response(query: &str, hits: Vec<CityHit>) -> String {
    if hits.is_empty() {
        return format_not_found(query);
    }
    if hits.len() == 1 {
        return format_city(&hits[0]);
    }
    if hits[0].city.eq_ignore_ascii_case(query) {
        return format_city(&hits[0]);
    }
    format_did_you_mean(&hits)
}

async fn send<T, L>(ctx: &CommandContext<'_, T, L>, line: String)
where
    T: Transport,
    L: LoginCredentials,
{
    if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, line).await {
        error!(error = ?e, "Failed to send !dpi response");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doener::CityHit;

    fn hit(city: &str, n: u32, with_prices: bool) -> CityHit {
        CityHit {
            city: city.into(),
            location_count: n,
            min_price: with_prices.then_some(6.0),
            max_price: with_prices.then_some(6.0),
            avg_price: with_prices.then_some(6.0),
        }
    }

    #[test]
    fn city_response_empty_hits_is_not_found() {
        let out = decide_city_response("xyz", vec![]);
        assert_eq!(out, "FeelsDankMan keine Stadt für 'xyz' gefunden.");
    }

    #[test]
    fn city_response_single_hit_uses_format_city() {
        let out = decide_city_response("Hannover", vec![hit("Hannover", 51, true)]);
        assert!(out.starts_with("Hannover: 51 Buden"));
    }

    #[test]
    fn city_response_exact_match_short_circuits() {
        // "Berlin" upstream returns Berlin + Berlingerode. Treat as single hit.
        let hits = vec![hit("Berlin", 324, true), hit("Berlingerode", 1, false)];
        let out = decide_city_response("berlin", hits);
        assert!(out.starts_with("Berlin: 324 Buden"));
    }

    #[test]
    fn city_response_multi_hit_no_exact_match_is_did_you_mean() {
        let hits = vec![
            hit("Hannover", 51, true),
            hit("Hanau", 3, true),
            hit("Handewitt", 1, false),
        ];
        let out = decide_city_response("Han", hits);
        assert_eq!(out, "Meintest du: Hannover (51), Hanau (3), Handewitt (1)?");
    }
}
