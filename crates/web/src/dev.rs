//! Dev-only login bypass for local testing.
//!
//! Compiled in only when the `dev-login` Cargo feature is enabled
//! (`cargo run --features dev-login`, or `just dev`). When present,
//! `/_dev/login` mints a signed session for [`DEV_USER_ID`] and redirects
//! to `/pings`, skipping the OAuth round-trip. The bin pushes
//! [`DEV_USER_ID`] into `hidden_admins` under the same cfg so the
//! mod-gate shortcut admits the session.
//!
//! Never compile into a production binary — anyone who can reach the
//! bind addr can mint a moderator session without OAuth.

use axum::Router;
use axum::extract::State;
use axum::response::Redirect;
use axum::routing::get;
use tower_cookies::Cookies;

use crate::auth::routes::issue_session_cookies;
use crate::state::WebState;

pub const DEV_USER_ID: &str = "1337";
pub const DEV_USER_LOGIN: &str = "devmod";

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/_dev/login", get(login))
        .with_state(state)
}

async fn login(State(state): State<WebState>, cookies: Cookies) -> Redirect {
    let (sid, csrf_bytes) = state
        .sessions
        .insert(DEV_USER_ID.to_owned(), DEV_USER_LOGIN.to_owned())
        .expect("insert dev session");
    issue_session_cookies(&cookies, &state.signed_key, sid, &csrf_bytes, false);
    Redirect::to("/pings")
}
