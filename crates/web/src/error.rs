//! Top-level error type that converts into a tailored axum response.
//!
//! Variants double as both the error model used inside handlers and the
//! presentation rule for the user — `Forbidden` renders the denied page,
//! `Conflict` renders the memory conflict page, etc. Internal errors log
//! with backtrace + return a generic 500.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// User has no valid session. Redirects to `/login`. Post-login deep-link
    /// (`?next=`) is not wired in v1 — every login lands the user on `/pings`.
    #[error("unauthenticated; redirect to login")]
    Unauthenticated,
    #[error("forbidden")]
    Forbidden,
    #[error("csrf mismatch")]
    CsrfMismatch,
    #[error("validation: {field}: {msg}")]
    Validation { field: String, msg: String },
    #[error("duplicate name: {name}")]
    DuplicateName { name: String },
    /// Boxed so the variant doesn't dominate `WebError::result_large_err`
    /// clippy lint — the inner payload carries multiple String fields.
    #[error("conflict")]
    Conflict(Box<ConflictPayload>),
    #[error("oauth exchange: {0}")]
    OAuthExchange(String),
    #[error("internal: {0}")]
    Internal(#[from] eyre::Report),
}

/// Inner payload for `WebError::Conflict`, boxed inside the variant to
/// keep the `Result<_, WebError>` size small (clippy `result_large_err`).
#[derive(Debug)]
pub struct ConflictPayload {
    pub kind: String,
    pub id: String,
    pub current_body: String,
    pub current_mtime: u64,
    pub draft: String,
    /// Hex-encoded csrf token to embed in the conflict resubmit form.
    /// The conflict template renders a fresh save form so the user can
    /// retry from inside the conflict page itself.
    pub csrf: String,
}

#[derive(Template)]
#[template(path = "auth/denied.html")]
struct DeniedTpl;

#[derive(Template)]
#[template(path = "memory/conflict.html")]
struct ConflictTpl<'a> {
    kind: &'a str,
    id: &'a str,
    current_body: &'a str,
    current_mtime: u64,
    draft: &'a str,
    csrf: &'a str,
}

fn render<T: Template>(status: StatusCode, tpl: &T) -> Response {
    match tpl.render() {
        Ok(body) => (status, Html(body)).into_response(),
        Err(err) => {
            tracing::error!(target: "twitch_1337_web", ?err, "template render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        match self {
            WebError::Unauthenticated => Redirect::to("/login").into_response(),
            WebError::Forbidden => render(StatusCode::FORBIDDEN, &DeniedTpl),
            WebError::CsrfMismatch => (
                StatusCode::FORBIDDEN,
                "Session expired, reload and try again",
            )
                .into_response(),
            WebError::Validation { field, msg } => (
                StatusCode::BAD_REQUEST,
                format!("validation: {field}: {msg}"),
            )
                .into_response(),
            WebError::DuplicateName { name } => (
                StatusCode::BAD_REQUEST,
                format!("ping `{name}` already exists"),
            )
                .into_response(),
            WebError::Conflict(payload) => render(
                StatusCode::CONFLICT,
                &ConflictTpl {
                    kind: &payload.kind,
                    id: &payload.id,
                    current_body: &payload.current_body,
                    current_mtime: payload.current_mtime,
                    draft: &payload.draft,
                    csrf: &payload.csrf,
                },
            ),
            WebError::OAuthExchange(msg) => (
                StatusCode::BAD_GATEWAY,
                format!("oauth exchange failed: {msg}"),
            )
                .into_response(),
            WebError::Internal(err) => {
                tracing::error!(
                    target: "twitch_1337_web",
                    error = ?err,
                    "internal error"
                );
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
