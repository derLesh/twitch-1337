//! `/flights` — read-only live snapshot of currently tracked aircraft.

use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::Response;
use axum::routing::get;
use tokio::sync::oneshot;
use twitch_1337_core::aviation::tracker::{TrackedFlightView, TrackerCommand};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::nav;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "flights/list.html")]
struct ListTpl {
    flights: Vec<TrackedFlightView>,
    aviation_disabled: bool,
    user_login: String,
    csrf: String,
    current_page: &'static str,
    is_mod: bool,
}

pub fn router() -> Router<WebState> {
    Router::new().route("/flights", get(list))
}

const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    let (flights, aviation_disabled) = match state.tracker_tx.as_ref() {
        Some(tx) => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let send_ok = tx
                .send(TrackerCommand::Snapshot { reply: reply_tx })
                .await
                .is_ok();
            let flights = if send_ok {
                tokio::time::timeout(SNAPSHOT_TIMEOUT, reply_rx)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            (flights, false)
        }
        None => (Vec::new(), true),
    };
    let csrf = csrf::encode(&session.csrf_value);
    let is_mod = session.is_mod();
    render(&ListTpl {
        flights,
        aviation_disabled,
        user_login: session.user_login,
        csrf,
        current_page: nav::FLIGHTS,
        is_mod,
    })
}
