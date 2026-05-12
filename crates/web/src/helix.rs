//! Minimal Twitch helix client (broadcaster id, moderator list, user lookup).
//!
//! Mirrors the AviationClient pattern in core for testability — boxed behind
//! a trait so route tests can inject fakes. Moderator membership is checked
//! with the helix `user_id` filter so a single round-trip resolves it
//! regardless of how many mods the channel has.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use eyre::{Result, WrapErr as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use crate::clock::Clock;

#[async_trait]
pub trait HelixClient: Send + Sync {
    async fn fetch_user_by_id(&self, user_id: &str) -> Result<Option<HelixUser>>;
    async fn fetch_user_by_login(&self, login: &str) -> Result<Option<HelixUser>>;
    /// Single helix call filtered by `user_id`; returns true iff the user is in the moderator list.
    async fn is_moderator(&self, broadcaster_id: &str, user_id: &str) -> Result<bool>;
    /// Batched lookup. [`ReqwestHelixClient`] overrides to a single helix
    /// call (up to 100 ids per request).
    async fn fetch_users_by_ids(&self, ids: &[&str]) -> Result<Vec<HelixUser>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(u) = self.fetch_user_by_id(id).await? {
                out.push(u);
            }
        }
        Ok(out)
    }
}

/// TTL cache for `profile_image_url` lookups keyed by Twitch user id.
///
/// Negative results (helix returned no avatar for an id) are cached
/// alongside positive ones so a missing user doesn't refetch every page
/// load. `lookup` returns the cached subset plus the ids that need a
/// fresh helix call; the caller is expected to feed the helix response
/// back into `insert`.
pub struct AvatarCache {
    entries: Mutex<HashMap<String, AvatarEntry>>,
    ttl: chrono::Duration,
    /// Hard cap on cached entries. When `insert` or `prime` would push
    /// the map past this, expired entries are pruned first and the
    /// oldest survivor is dropped if still over. Prevents unbounded
    /// growth from churned-through user ids over the bot's lifetime.
    max_entries: usize,
}

struct AvatarEntry {
    url: Option<String>,
    fetched_at: DateTime<Utc>,
}

pub struct AvatarLookup {
    pub cached: HashMap<String, String>,
    pub missing: Vec<String>,
}

/// Default upper bound on cached entries — generous enough for any
/// reasonable channel's `memories/users/` directory.
const DEFAULT_AVATAR_CACHE_CAP: usize = 4096;

impl AvatarCache {
    pub fn new(ttl: Duration) -> Self {
        Self::with_capacity(ttl, DEFAULT_AVATAR_CACHE_CAP)
    }

    pub fn with_capacity(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl: chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(1)),
            max_entries: max_entries.max(1),
        }
    }

    /// Drop expired entries; if still at-or-over cap, evict the
    /// oldest-fetched survivor. Called from the insert paths.
    fn evict_if_needed(
        entries: &mut HashMap<String, AvatarEntry>,
        now: DateTime<Utc>,
        ttl: chrono::Duration,
        cap: usize,
    ) {
        if entries.len() < cap {
            return;
        }
        entries.retain(|_, e| now.signed_duration_since(e.fetched_at) < ttl);
        while entries.len() >= cap {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.fetched_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
    }

    pub async fn lookup(&self, ids: &[&str], clock: &dyn Clock) -> AvatarLookup {
        let now = clock.now();
        let entries = self.entries.lock().await;
        let mut cached = HashMap::new();
        let mut missing = Vec::new();
        for id in ids {
            match entries.get(*id) {
                Some(e) if now.signed_duration_since(e.fetched_at) < self.ttl => {
                    if let Some(url) = &e.url {
                        cached.insert((*id).to_owned(), url.clone());
                    }
                }
                _ => missing.push((*id).to_owned()),
            }
        }
        AvatarLookup { cached, missing }
    }

    /// Records a single avatar entry. Used by the OAuth callback so the
    /// caller's own avatar lands in the cache without an extra batch call.
    pub async fn prime(&self, user_id: &str, url: Option<&str>, clock: &dyn Clock) {
        let now = clock.now();
        let mut entries = self.entries.lock().await;
        Self::evict_if_needed(&mut entries, now, self.ttl, self.max_entries);
        entries.insert(
            user_id.to_owned(),
            AvatarEntry {
                url: url.map(str::to_owned),
                fetched_at: now,
            },
        );
    }

    /// Records helix response. `queried` is the full set of ids that were
    /// requested; ids absent from `users` are stored as negative entries
    /// so repeated lookups for empty/unknown users don't refetch.
    pub async fn insert(&self, queried: &[String], users: &[HelixUser], clock: &dyn Clock) {
        let now = clock.now();
        let by_id: HashMap<&str, Option<&str>> = users
            .iter()
            .map(|u| (u.id.as_str(), u.profile_image_url.as_deref()))
            .collect();
        let mut entries = self.entries.lock().await;
        Self::evict_if_needed(&mut entries, now, self.ttl, self.max_entries);
        for id in queried {
            let url = by_id.get(id.as_str()).copied().flatten().map(str::to_owned);
            entries.insert(
                id.clone(),
                AvatarEntry {
                    url,
                    fetched_at: now,
                },
            );
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelixUser {
    pub id: String,
    pub login: String,
    pub display_name: String,
    #[serde(default)]
    pub profile_image_url: Option<String>,
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

    async fn fetch_users_by_ids(&self, ids: &[&str]) -> Result<Vec<HelixUser>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(Deserialize)]
        struct UserResp {
            data: Vec<HelixUser>,
        }
        let token = self.access_token_provider.current_access_token().await?;
        let mut out = Vec::with_capacity(ids.len());
        // Twitch helix `/users` accepts up to 100 `id` query params per call.
        for chunk in ids.chunks(100) {
            let query: Vec<(&str, &str)> = chunk.iter().map(|id| ("id", *id)).collect();
            let resp: UserResp = helix_get(
                &self.http,
                &self.helix_base,
                "/helix/users",
                &query,
                &token,
                self.client_id.expose_secret(),
                "helix users (batch)",
            )
            .await?;
            out.extend(resp.data);
        }
        Ok(out)
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
