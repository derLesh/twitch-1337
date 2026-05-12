//! Dev-only login bypass for local testing.
//!
//! Compiled in only when the `dev-login` Cargo feature is enabled
//! (`cargo run --features dev-login`, or `just dev`). When present,
//! `/_dev/login` mints a signed session for [`DEV_USER_ID`] and redirects
//! to `/pings`, skipping the OAuth round-trip. The bin pushes
//! [`DEV_USER_ID`] into `hidden_admins` under the same cfg so the
//! mod-gate shortcut admits the session.
//!
//! Also exposes [`StubHelix`] — a zero-network HelixClient impl used by
//! the `web-dev` bin so the dashboard can run from a worktree against
//! shared dev-data without colliding with a real Twitch connection.
//!
//! Never compile into a production binary — anyone who can reach the
//! bind addr can mint a moderator session without OAuth.

use async_trait::async_trait;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::routing::get;
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::auth::routes::issue_session_cookies;
use crate::helix::{HelixClient, HelixUser};
use crate::state::WebState;

pub const DEV_USER_ID: &str = "1337";
pub const DEV_USER_LOGIN: &str = "devmod";

/// Deterministic session id used by `/_dev/login` and the web-dev
/// pre-seed. Fixed so the signed browser cookie survives a server
/// restart without a fresh login round-trip. 64 hex chars.
pub const DEV_SID: &str = "de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7de7d";
/// Deterministic csrf paired with [`DEV_SID`].
pub const DEV_CSRF: [u8; 32] = [0x42; 32];

/// Identity that both `/_dev/login` and the web-dev pre-seed install.
pub fn dev_new_session() -> crate::auth::session::NewSession {
    crate::auth::session::NewSession {
        user_id: DEV_USER_ID.to_owned(),
        user_login: DEV_USER_LOGIN.to_owned(),
        role: crate::auth::role::Role::Mod,
        avatar_url: None,
        is_broadcaster: false,
    }
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/_dev/login", get(login))
        .with_state(state)
}

#[derive(Deserialize)]
pub struct DevLoginQuery {
    /// Same-origin path to land on after the session is minted. Anything
    /// not starting with `/` (or starting with `//`) falls back to
    /// `/pings` so the route can't be coerced into open-redirecting.
    next: Option<String>,
}

async fn login(
    State(state): State<WebState>,
    cookies: Cookies,
    Query(q): Query<DevLoginQuery>,
) -> Redirect {
    state
        .sessions
        .insert_with_id(DEV_SID, DEV_CSRF, dev_new_session());
    issue_session_cookies(
        &cookies,
        &state.signed_key,
        DEV_SID.to_owned(),
        &DEV_CSRF,
        false,
    );
    let target = q
        .next
        .as_deref()
        .filter(|p| p.starts_with('/') && !p.starts_with("//"))
        .unwrap_or("/pings");
    Redirect::to(target)
}

/// Zero-network [`HelixClient`] for the `web-dev` bin.
///
/// `is_moderator` returns true only for [`DEV_USER_ID`] so the
/// require_mod sliding recheck admits the dev session and rejects any
/// other forged sid (the cookie sig already gates that, this is
/// defense-in-depth). User lookups echo the input so `/auth/callback`
/// would round-trip if accidentally exercised.
#[derive(Default)]
pub struct StubHelix;

#[async_trait]
impl HelixClient for StubHelix {
    async fn fetch_user_by_id(&self, id: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(Some(HelixUser {
            id: id.to_owned(),
            login: format!("user{id}"),
            display_name: format!("user{id}"),
            profile_image_url: None,
        }))
    }
    async fn fetch_user_by_login(&self, login: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(Some(HelixUser {
            id: "0".to_owned(),
            login: login.to_owned(),
            display_name: login.to_owned(),
            profile_image_url: None,
        }))
    }
    async fn is_moderator(&self, _broadcaster: &str, user_id: &str) -> eyre::Result<bool> {
        Ok(user_id == DEV_USER_ID)
    }
}
