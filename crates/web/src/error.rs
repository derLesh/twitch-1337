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
    /// User has no valid session. Redirects to `/login`. The optional `next`
    /// captures the requested path so the callback can return there after
    /// successful login.
    #[error("unauthenticated; redirect to login")]
    Unauthenticated { next: Option<String> },
    #[error("forbidden")]
    Forbidden,
    #[error("csrf mismatch")]
    CsrfMismatch,
    #[error("validation: {field}: {msg}")]
    Validation { field: String, msg: String },
    /// Boxed so the variant doesn't dominate `WebError::result_large_err`
    /// clippy lint — the inner payload carries multiple String fields.
    #[error("conflict")]
    Conflict(Box<ConflictPayload>),
    /// OAuth flow failure (token exchange, user lookup, mod check). The
    /// inner `eyre::Report` carries the wrapped chain — logged via Debug
    /// (full chain + spantrace) when the response is built so cloudflare's
    /// 502 has a matching server-side trace.
    #[error("oauth exchange: {0}")]
    OAuthExchange(eyre::Report),
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
    pub current_mtime_display: String,
    pub draft: String,
    /// Hex-encoded csrf token to embed in the conflict resubmit form.
    /// The conflict template renders a fresh save form so the user can
    /// retry from inside the conflict page itself.
    pub csrf: String,
    /// Logged-in user's login, threaded through to the sidebar.
    pub user_login: String,
    /// Sidebar highlight key matching the originating editor's section.
    pub current_page: &'static str,
}

#[derive(Template)]
#[template(path = "auth/denied.html")]
struct DeniedTpl;

#[derive(Template)]
#[template(path = "auth/oauth_failed.html")]
struct OAuthFailedTpl;

#[derive(Template)]
#[template(path = "memory/conflict.html")]
struct ConflictTpl<'a> {
    kind: &'a str,
    id: &'a str,
    current_body: &'a str,
    current_mtime: u64,
    current_mtime_display: &'a str,
    draft: &'a str,
    csrf: &'a str,
    user_login: &'a str,
    current_page: &'static str,
}

/// Allow only same-origin absolute paths. Anything that smells like a
/// scheme, host, or CRLF is rejected so the redirect can't be turned
/// into an open-redirect or header-splitting vector. Backslashes are
/// rejected because browsers (per WHATWG URL spec) parse them as `/`,
/// turning `/\evil.example/x` into a protocol-relative URL.
///
/// Public so test binaries (in `crates/web/tests/`) can pin the validator
/// directly — they link as separate crates against the public API.
pub fn is_safe_redirect(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 256
        && !path.starts_with("//")
        && !path.contains("://")
        && !path.contains(['\r', '\n', '\\'])
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
            WebError::Unauthenticated { next } => {
                if let Some(path) = next.filter(|p| is_safe_redirect(p)) {
                    Redirect::to(&format!("/login?next={}", urlencoding::encode(&path)))
                        .into_response()
                } else {
                    Redirect::to("/login").into_response()
                }
            }
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
            WebError::Conflict(payload) => render(
                StatusCode::CONFLICT,
                &ConflictTpl {
                    kind: &payload.kind,
                    id: &payload.id,
                    current_body: &payload.current_body,
                    current_mtime: payload.current_mtime,
                    current_mtime_display: &payload.current_mtime_display,
                    draft: &payload.draft,
                    csrf: &payload.csrf,
                    user_login: &payload.user_login,
                    current_page: payload.current_page,
                },
            ),
            WebError::OAuthExchange(err) => {
                tracing::error!(
                    target: "twitch_1337_web",
                    error = ?err,
                    "oauth exchange failed"
                );
                // Almost always a stale/reused auth code (refresh, double-tab); offer retry instead of bare 502.
                render(StatusCode::BAD_GATEWAY, &OAuthFailedTpl)
            }
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
