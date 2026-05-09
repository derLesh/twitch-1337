//! `/pings` CRUD handlers.
//!
//! Mounted under the authed sub-router so every entry point is mod-gated.
//! Form POSTs validate the `_csrf` field against the session csrf value;
//! the HTMX delete uses an `X-Csrf-Token` header (no body to round-trip).
//! Persistence + validation reuse `PingManager`'s existing checks
//! (control-char rejection, duplicate detection, name allowlist).

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/pings", get(list).post(create))
        .route("/pings/new", get(new_form))
        .route("/pings/{name}", get(edit_form).post(update))
        .route("/pings/{name}/delete", post(delete))
}

#[derive(Clone)]
struct RowView {
    name: String,
    template: String,
    members: usize,
    created_by: String,
}

#[derive(Template)]
#[template(path = "pings/list.html")]
struct ListTpl {
    rows: Vec<RowView>,
    flash: Option<String>,
    csrf: String,
}

#[derive(Template)]
#[template(path = "pings/form.html")]
struct FormTpl<'a> {
    is_new: bool,
    name: &'a str,
    template_text: &'a str,
    csrf: &'a str,
    error: Option<String>,
}

fn render<T: Template>(tpl: &T) -> Result<Response, WebError> {
    let body = tpl
        .render()
        .map_err(|e| WebError::Internal(eyre::eyre!("render: {e}")))?;
    Ok(Html(body).into_response())
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let mgr = state.ping_manager.read().await;
    let mut rows: Vec<RowView> = mgr
        .iter()
        .map(|(name, ping)| RowView {
            name: name.clone(),
            template: ping.template.clone(),
            members: ping.members.len(),
            created_by: ping.created_by.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    drop(mgr);

    let tpl = ListTpl {
        rows,
        flash: flash::take(&cookies),
        csrf: csrf::encode(&session.csrf_value),
    };
    render(&tpl)
}

async fn new_form(Extension(session): Extension<Session>) -> Result<Response, WebError> {
    let csrf_hex = csrf::encode(&session.csrf_value);
    render(&FormTpl {
        is_new: true,
        name: "",
        template_text: "",
        csrf: &csrf_hex,
        error: None,
    })
}

#[derive(Deserialize)]
struct CreateForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    name: String,
    template: String,
}

async fn create(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<CreateForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let name = form.name.trim().to_owned();
    let template = form.template;

    let mut mgr = state.ping_manager.write().await;
    if mgr.ping_exists_ignore_case(&name) {
        tracing::info!(
            target: "twitch_1337_web",
            user_id = %session.user_id,
            action = "ping_create",
            target_name = %name,
            result = "duplicate",
        );
        return Err(WebError::DuplicateName { name });
    }
    if let Err(e) = mgr.create_ping(name.clone(), template, session.user_login.clone(), None) {
        tracing::warn!(
            target: "twitch_1337_web",
            user_id = %session.user_id,
            action = "ping_create",
            target_name = %name,
            result = "validation",
            error = ?e,
        );
        return Err(WebError::Validation {
            field: "template".into(),
            msg: e.to_string(),
        });
    }
    drop(mgr);

    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "ping_create",
        target_name = %name,
        result = "ok",
    );
    flash::set(&cookies, &format!("Created ping `{name}`."));
    Ok(Redirect::to("/pings").into_response())
}

async fn edit_form(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
) -> Result<Response, WebError> {
    let mgr = state.ping_manager.read().await;
    let template_text = mgr
        .get(&name)
        .ok_or_else(|| WebError::Validation {
            field: "name".into(),
            msg: format!("ping `{name}` does not exist"),
        })?
        .template
        .clone();
    drop(mgr);

    let csrf_hex = csrf::encode(&session.csrf_value);
    render(&FormTpl {
        is_new: false,
        name: &name,
        template_text: &template_text,
        csrf: &csrf_hex,
        error: None,
    })
}

#[derive(Deserialize)]
struct UpdateForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    template: String,
}

async fn update(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path(name): Path<String>,
    axum::Form(form): axum::Form<UpdateForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }

    let mut mgr = state.ping_manager.write().await;
    if let Err(e) = mgr.edit_template(&name, form.template) {
        tracing::warn!(
            target: "twitch_1337_web",
            user_id = %session.user_id,
            action = "ping_update",
            target_name = %name,
            result = "validation",
            error = ?e,
        );
        return Err(WebError::Validation {
            field: "template".into(),
            msg: e.to_string(),
        });
    }
    drop(mgr);

    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "ping_update",
        target_name = %name,
        result = "ok",
    );
    flash::set(&cookies, &format!("Updated ping `{name}`."));
    Ok(Redirect::to("/pings").into_response())
}

async fn delete(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    let header_token = headers
        .get("X-Csrf-Token")
        .and_then(|v| v.to_str().ok())
        .ok_or(WebError::CsrfMismatch)?;
    if !csrf::verify(header_token, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }

    let mut mgr = state.ping_manager.write().await;
    // Idempotent: delete on a missing ping is a no-op. The HTMX row swap
    // removes the visible row either way; double-clicks shouldn't 4xx.
    let _ = mgr.delete_ping(&name);
    drop(mgr);

    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "ping_delete",
        target_name = %name,
        result = "ok",
    );
    // Empty body so HTMX `hx-swap="outerHTML"` removes the row.
    Ok((StatusCode::OK, "").into_response())
}
