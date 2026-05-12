//! Minimal Twitch helix client (broadcaster id, moderator list, user lookup).
//!
//! Mirrors the AviationClient pattern in core for testability — boxed behind
//! a trait so route tests can inject fakes. Moderator membership is checked
//! with the helix `user_id` filter so a single round-trip resolves it
//! regardless of how many mods the channel has.

use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;

#[async_trait]
pub trait HelixClient: Send + Sync {
    async fn fetch_user_by_id(&self, user_id: &str) -> Result<Option<HelixUser>>;
    async fn fetch_user_by_login(&self, login: &str) -> Result<Option<HelixUser>>;
    /// Single helix call filtered by `user_id`; returns true iff the user is in the moderator list.
    async fn is_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<bool>;
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelixUser {
    pub id: String,
    pub login: String,
    pub display_name: String,
}

#[async_trait]
pub trait AccessTokenProvider: Send + Sync {
    async fn current_access_token(&self) -> Result<String>;
}

pub struct ReqwestHelixClient {
    pub http: reqwest::Client,
    pub client_id: SecretString,
    pub access_token_provider: Arc<dyn AccessTokenProvider>,
    /// `https://api.twitch.tv` in production. Tests inject a wiremock URI.
    pub helix_base: String,
}

impl ReqwestHelixClient {
    pub fn new(
        http: reqwest::Client,
        client_id: SecretString,
        provider: Arc<dyn AccessTokenProvider>,
    ) -> Self {
        Self::with_base(http, client_id, provider, "https://api.twitch.tv".into())
    }

    pub fn with_base(
        http: reqwest::Client,
        client_id: SecretString,
        provider: Arc<dyn AccessTokenProvider>,
        base: String,
    ) -> Self {
        Self {
            http,
            client_id,
            access_token_provider: provider,
            helix_base: base,
        }
    }

    async fn fetch_user(&self, query: &[(&str, &str)]) -> Result<Option<HelixUser>> {
        #[derive(Deserialize)]
        struct UserResp {
            data: Vec<HelixUser>,
        }
        let token = self.access_token_provider.current_access_token().await?;
        let resp: UserResp = helix_get(
            &self.http,
            &self.helix_base,
            "/helix/users",
            query,
            &token,
            self.client_id.expose_secret(),
            "helix users",
        )
        .await?;
        Ok(resp.data.into_iter().next())
    }
}

/// Shared GET → bearer + Client-Id → JSON-decode. All Twitch helix calls
/// in this module share the same shape; this isolates it so each caller
/// only writes the path, query, and response type.
async fn helix_get<T: DeserializeOwned>(
    http: &reqwest::Client,
    helix_base: &str,
    path: &str,
    query: &[(&str, &str)],
    bearer_token: &str,
    client_id: &str,
    context: &'static str,
) -> Result<T> {
    let mut url = url::Url::parse(&format!("{helix_base}{path}"))?;
    for (k, v) in query {
        url.query_pairs_mut().append_pair(k, v);
    }
    let resp = http
        .get(url)
        .bearer_auth(bearer_token)
        .header("Client-Id", client_id)
        .send()
        .await?
        .error_for_status()
        .wrap_err(context)?
        .json::<T>()
        .await?;
    Ok(resp)
}

#[async_trait]
impl HelixClient for ReqwestHelixClient {
    async fn fetch_user_by_id(&self, user_id: &str) -> Result<Option<HelixUser>> {
        self.fetch_user(&[("id", user_id)]).await
    }

    async fn fetch_user_by_login(&self, login: &str) -> Result<Option<HelixUser>> {
        self.fetch_user(&[("login", login)]).await
    }

    async fn is_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<bool> {
        let token = self.access_token_provider.current_access_token().await?;
        helix_moderator_check(
            &self.http,
            &self.helix_base,
            self.client_id.expose_secret(),
            &token,
            broadcaster_id,
            user_id,
            "helix moderators (bot token)",
        )
        .await
    }
}

/// Single helix moderator-list call filtered by `user_id`. Returns true iff
/// `user_id` is a moderator of `broadcaster_id`. Used by
/// `ReqwestHelixClient::is_moderator` (bot token); the OAuth callback path
/// uses [`user_moderates_channel`] instead because Twitch requires the
/// token's user to *be* the broadcaster on this endpoint.
pub async fn helix_moderator_check(
    http: &reqwest::Client,
    helix_base: &str,
    client_id: &str,
    bearer_token: &str,
    broadcaster_id: &str,
    user_id: &str,
    context: &'static str,
) -> Result<bool> {
    #[derive(Deserialize)]
    struct ModResp {
        data: Vec<serde::de::IgnoredAny>,
    }
    let resp: ModResp = helix_get(
        http,
        helix_base,
        "/helix/moderation/moderators",
        &[("broadcaster_id", broadcaster_id), ("user_id", user_id)],
        bearer_token,
        client_id,
        context,
    )
    .await?;
    Ok(!resp.data.is_empty())
}

/// Used during OAuth callback because `helix/moderation/moderators`
/// requires the bearer's user id to match `broadcaster_id` (i.e. only
/// the broadcaster can list their own mods), so a logging-in mod cannot
/// query their own status that way.
///
/// Pagination: `first=100` (Twitch's max) without follow-up — at that
/// many moderated channels the user is almost certainly already an
/// admin somewhere; deny on overflow is acceptable for this use case.
pub async fn user_moderates_channel(
    http: &reqwest::Client,
    helix_base: &str,
    client_id: &str,
    user_access_token: &str,
    user_id: &str,
    broadcaster_id: &str,
    context: &'static str,
) -> Result<bool> {
    #[derive(Deserialize)]
    struct Channel {
        broadcaster_id: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<Channel>,
    }
    let resp: Resp = helix_get(
        http,
        helix_base,
        "/helix/moderation/channels",
        &[("user_id", user_id), ("first", "100")],
        user_access_token,
        client_id,
        context,
    )
    .await?;
    Ok(resp.data.iter().any(|c| c.broadcaster_id == broadcaster_id))
}
