use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::error::WebError;

pub mod assets;
pub mod flights;
pub mod health;
pub mod leaderboard;
pub mod memory;
pub mod pings;
pub mod settings;
pub mod stubs;

pub(crate) fn render<T: Template>(tpl: &T) -> Result<Response, WebError> {
    render_with(StatusCode::OK, tpl)
}

pub(crate) fn render_with<T: Template>(status: StatusCode, tpl: &T) -> Result<Response, WebError> {
    let body = tpl
        .render()
        .map_err(|e| WebError::Internal(eyre::eyre!("render: {e}")))?;
    Ok((status, Html(body)).into_response())
}

/// First char of `name`, uppercased; `?` if blank. Used for avatar tiles.
pub(crate) fn initial_of(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_owned())
}
