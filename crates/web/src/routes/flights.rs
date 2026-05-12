//! `/flights` — read-only live snapshot of currently tracked aircraft.

use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::sync::oneshot;
use tower_cookies::Cookies;
use twitch_1337_core::aviation::tracker::{TrackedFlightView, TrackerCommand};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::nav;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "flights/list.html")]
struct ListTpl {
    flights: Vec<TrackedFlightView>,
    aviation_disabled: bool,
    user_login: String,
    user_avatar_url: Option<String>,
    csrf: String,
    flash: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
}

pub fn viewer_router() -> Router<WebState> {
    Router::new().route("/flights", get(list))
}

pub fn mod_router() -> Router<WebState> {
    Router::new().route("/flights/delete", post(delete))
}

const TRACKER_TIMEOUT: Duration = Duration::from_millis(500);

/// Round-trips one tracker command and returns the reply, or `None` when
/// aviation is disabled, the channel is closed, or the reply times out.
async fn tracker_request<R>(
    state: &WebState,
    build_cmd: impl FnOnce(oneshot::Sender<R>) -> TrackerCommand,
) -> Option<R> {
    let tx = state.tracker_tx.as_ref()?;
    let (reply_tx, reply_rx) = oneshot::channel();
    if tx.send(build_cmd(reply_tx)).await.is_err() {
        return None;
    }
    tokio::time::timeout(TRACKER_TIMEOUT, reply_rx)
        .await
        .ok()
        .and_then(Result::ok)
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let aviation_disabled = state.tracker_tx.is_none();
    let flights = tracker_request(&state, |reply| TrackerCommand::Snapshot { reply })
        .await
        .unwrap_or_default();
    let csrf = csrf::encode(&session.csrf_value);
    let is_mod = session.is_mod();
    render(&ListTpl {
        flights,
        aviation_disabled,
        user_avatar_url: session.avatar_url.clone(),
        user_login: session.user_login,
        csrf,
        flash: flash::take(&cookies),
        current_page: nav::FLIGHTS,
        is_mod,
        is_broadcaster: session.is_broadcaster,
    })
}

#[derive(Deserialize)]
struct DeleteForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    identifier: String,
}

async fn delete(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<DeleteForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let identifier = form.identifier.trim().to_owned();
    if identifier.is_empty() {
        return Err(WebError::Validation {
            field: "identifier".into(),
            msg: "required".into(),
        });
    }
    let removed = tracker_request(&state, |reply| TrackerCommand::DeleteFromWeb {
        identifier: identifier.clone(),
        reply,
    })
    .await
    .flatten();
    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "flight_delete",
        target_id = %identifier,
        result = if removed.is_some() { "ok" } else { "not_found" },
    );
    let msg = match removed {
        Some(label) => format!("Untracked `{label}`."),
        None if state.tracker_tx.is_none() => "Aviation tracking disabled.".to_owned(),
        None => format!("`{identifier}` not found."),
    };
    flash::set(&cookies, &msg);
    Ok(Redirect::to("/flights").into_response())
}
