//! `/leaderboard` — read-only 1337 PB ranking.

use askama::Template;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::Response;
use axum::routing::get;

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "leaderboard/list.html")]
struct ListTpl {
    rows: Vec<RowView>,
    user_login: String,
    csrf: String,
    current_page: &'static str,
    is_mod: bool,
}

struct RowView {
    rank: usize,
    login: String,
    ms: u64,
    date: chrono::NaiveDate,
}

pub fn router() -> Router<WebState> {
    Router::new().route("/leaderboard", get(list))
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    let lb = state.leaderboard.read().await;
    let mut entries: Vec<(String, u64, chrono::NaiveDate)> = lb
        .iter()
        .map(|(login, pb)| (login.clone(), pb.ms, pb.date))
        .collect();
    drop(lb);

    // Sort by best time ascending; break ties alphabetically by login.
    entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let rows = entries
        .into_iter()
        .enumerate()
        .map(|(i, (login, ms, date))| RowView {
            rank: i + 1,
            login,
            ms,
            date,
        })
        .collect();

    let csrf = csrf::encode(&session.csrf_value);
    let is_mod = session.is_mod();
    render(&ListTpl {
        rows,
        user_login: session.user_login,
        csrf,
        current_page: crate::nav::LEADERBOARD,
        is_mod,
    })
}
