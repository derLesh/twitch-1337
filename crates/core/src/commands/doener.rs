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
use crate::doener::{CityHit, DoeneratlasClient};

pub struct DoenerCommand {
    client: Arc<DoeneratlasClient>,
    cooldown: PerUserCooldown,
}

impl DoenerCommand {
    pub fn new(client: Arc<DoeneratlasClient>, cooldown: Duration) -> Self {
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
                    error!(error = ?e, "doeneratlas stats lookup failed");
                    api_down_message().to_string()
                }
            }
        } else {
            match self.client.search_city_hits(query).await {
                Ok(hits) => resolve_city_reply(self.client.as_ref(), query, hits).await,
                Err(e) => {
                    error!(error = ?e, query, "doeneratlas city search failed");
                    api_down_message().to_string()
                }
            }
        };

        send(&ctx, response).await;
        Ok(())
    }
}

async fn resolve_city_reply(
    client: &DoeneratlasClient,
    query: &str,
    mut hits: Vec<CityHit>,
) -> String {
    if hits.is_empty() {
        return format_not_found(query);
    }
    if hits.len() == 1 {
        let hit = client.enrich_city_hit(hits.remove(0)).await;
        return format_city(&hit);
    }
    if hits[0].city.eq_ignore_ascii_case(query) {
        let hit = client.enrich_city_hit(hits.remove(0)).await;
        return format_city(&hit);
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
    use crate::doener::{CityHit, DoeneratlasClient};

    fn hit(city: &str, n: u32, with_prices: bool) -> CityHit {
        CityHit {
            city: city.into(),
            slug: String::new(),
            location_count: n,
            priced_shop_sample: if with_prices { n } else { 0 },
            min_price: with_prices.then_some(6.0),
            max_price: with_prices.then_some(6.0),
            avg_price: with_prices.then_some(6.0),
        }
    }

    #[tokio::test]
    async fn city_response_empty_hits_is_not_found() {
        crate::install_crypto_provider();
        let server = wiremock::MockServer::start().await;
        let client = DoeneratlasClient::with_base_url(reqwest::Client::new(), server.uri());
        let out = resolve_city_reply(&client, "xyz", vec![]).await;
        assert_eq!(out, "FeelsDankMan keine Stadt für 'xyz' gefunden.");
    }

    #[tokio::test]
    async fn city_response_single_hit_enriches_from_cities_endpoint() {
        crate::install_crypto_provider();
        let server = wiremock::MockServer::start().await;
        let json = br#"{"name":"Hannover","slug":"hannover","shop_count":20,"avg_price":"4.81","min_price":"2.50","max_price":"8.00"}"#;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app-api/public/cities"))
            .and(wiremock::matchers::query_param("slug", "hannover"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(json.as_slice(), "application/json"),
            )
            .mount(&server)
            .await;

        let client = DoeneratlasClient::with_base_url(reqwest::Client::new(), server.uri());
        let hit = CityHit {
            city: "Hannover".into(),
            slug: "hannover".into(),
            location_count: 51,
            priced_shop_sample: 3,
            min_price: Some(3.0),
            max_price: Some(9.0),
            avg_price: Some(6.0),
        };
        let out = resolve_city_reply(&client, "Han", vec![hit]).await;
        assert!(
            out.starts_with("Hannover: 20 Buden")
                && out.contains("4.81")
                && out.contains("2.50")
                && out.contains("8.00"),
            "unexpected reply: {out}"
        );
    }

    #[tokio::test]
    async fn city_response_exact_match_enriches_first_hit_only() {
        crate::install_crypto_provider();
        let server = wiremock::MockServer::start().await;
        let json = br#"{"name":"Berlin","slug":"berlin","shop_count":127,"avg_price":"5.49","min_price":"2.50","max_price":"37.00"}"#;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app-api/public/cities"))
            .and(wiremock::matchers::query_param("slug", "berlin"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(json.as_slice(), "application/json"),
            )
            .mount(&server)
            .await;

        let client = DoeneratlasClient::with_base_url(reqwest::Client::new(), server.uri());
        let hits = vec![
            CityHit {
                city: "Berlin".into(),
                slug: "berlin".into(),
                location_count: 127,
                priced_shop_sample: 8,
                min_price: Some(3.3),
                max_price: Some(8.0),
                avg_price: Some(4.95),
            },
            hit("Berlingerode", 1, false),
        ];
        let out = resolve_city_reply(&client, "berlin", hits).await;
        assert!(
            out.starts_with("Berlin: 127 Buden")
                && out.contains("5.49")
                && out.contains("2.50")
                && out.contains("37.00"),
            "unexpected reply: {out}"
        );
    }

    #[tokio::test]
    async fn city_response_multi_hit_no_exact_match_is_did_you_mean() {
        crate::install_crypto_provider();
        let server = wiremock::MockServer::start().await;
        let client = DoeneratlasClient::with_base_url(reqwest::Client::new(), server.uri());
        let hits = vec![
            hit("Hannover", 51, true),
            hit("Hanau", 3, true),
            hit("Handewitt", 1, false),
        ];
        let out = resolve_city_reply(&client, "Han", hits).await;
        assert_eq!(out, "Meintest du: Hannover (51), Hanau (3), Handewitt (1)?");
    }
}
