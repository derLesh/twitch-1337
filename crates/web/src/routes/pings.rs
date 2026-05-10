//! `/pings` CRUD handlers.
//!
//! Mounted under the authed sub-router so every entry point is mod-gated.
//! Form POSTs validate the `_csrf` field against the session csrf value;
//! the HTMX delete uses an `X-Csrf-Token` header (no body to round-trip).
//! Persistence + validation reuse `PingManager`'s existing checks
//! (control-char rejection, duplicate detection, name allowlist).

use std::collections::HashSet;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::commands::normalize_username;
use twitch_1337_core::ping::PingManager;

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::{initial_of, render, render_with};
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/pings", get(list).post(create))
        .route("/pings/new", get(new_form))
        .route("/pings/{name}", get(edit_form).post(update))
        .route("/pings/{name}/delete", post(delete))
        .route("/pings/{name}/members", post(add_member))
        .route(
            "/pings/{name}/members/{username}/delete",
            post(remove_member),
        )
}

#[derive(Clone)]
struct RowView {
    name: String,
    template: String,
    members: usize,
    created_by: String,
    created_initial: String,
}

#[derive(Template)]
#[template(path = "pings/list.html")]
struct ListTpl {
    rows: Vec<RowView>,
    total_pings: usize,
    total_members: usize,
    with_variables: usize,
    custom_cooldowns: usize,
    flash: Option<String>,
    csrf: String,
    user_login: String,
    current_page: &'static str,
}

#[derive(Template)]
#[template(path = "pings/form.html")]
struct FormTpl<'a> {
    is_new: bool,
    name: &'a str,
    template_text: &'a str,
    csrf: &'a str,
    error: Option<String>,
    user_login: &'a str,
    current_page: &'static str,
    /// Sorted lowercase logins. Empty on the create form.
    members: Vec<String>,
    /// Inline error from a recent add/remove attempt, rendered above the
    /// member list. Independent of the template-edit `error` field.
    member_error: Option<String>,
}

/// Snapshot a ping for the edit-form template: cloned template text + a
/// sorted member list. Returns `None` when the ping doesn't exist so callers
/// can map to a 4xx without re-borrowing the manager.
fn ping_snapshot(mgr: &PingManager, name: &str) -> Option<(String, Vec<String>)> {
    let p = mgr.get(name)?;
    let mut members: Vec<String> = p.members.iter().cloned().collect();
    members.sort();
    Some((p.template.clone(), members))
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let mgr = state.ping_manager.read().await;
    let mut rows: Vec<RowView> = Vec::new();
    let mut unique_members: HashSet<&str> = HashSet::new();
    let mut with_variables: usize = 0;
    let mut custom_cooldowns: usize = 0;
    for (name, ping) in mgr.iter() {
        rows.push(RowView {
            name: name.clone(),
            template: ping.template.clone(),
            members: ping.members.len(),
            created_initial: initial_of(&ping.created_by),
            created_by: ping.created_by.clone(),
        });
        for m in &ping.members {
            unique_members.insert(m.as_str());
        }
        if ping.template.contains("{mentions}") || ping.template.contains("{sender}") {
            with_variables += 1;
        }
        if ping.cooldown.is_some() {
            custom_cooldowns += 1;
        }
    }
    let total_members = unique_members.len();
    drop(mgr);

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let total_pings = rows.len();
    let tpl = ListTpl {
        rows,
        total_pings,
        total_members,
        with_variables,
        custom_cooldowns,
        flash: flash::take(&cookies),
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        current_page: crate::nav::PINGS,
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
        user_login: &session.user_login,
        current_page: crate::nav::PINGS,
        members: Vec::new(),
        member_error: None,
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
    let csrf_hex = csrf::encode(&session.csrf_value);

    let mut mgr = state.ping_manager.write().await;
    if mgr.ping_exists_ignore_case(&name) {
        tracing::info!(
            target: "twitch_1337_web",
            user_id = %session.user_id,
            action = "ping_create",
            target_name = %name,
            result = "duplicate",
        );
        return render_with(
            StatusCode::BAD_REQUEST,
            &FormTpl {
                is_new: true,
                name: &name,
                template_text: &template,
                csrf: &csrf_hex,
                error: Some(format!("ping `{name}` already exists")),
                user_login: &session.user_login,
                current_page: crate::nav::PINGS,
                members: Vec::new(),
                member_error: None,
            },
        );
    }
    if let Err(e) = mgr.create_ping(
        name.clone(),
        template.clone(),
        session.user_login.clone(),
        None,
    ) {
        tracing::warn!(
            target: "twitch_1337_web",
            user_id = %session.user_id,
            action = "ping_create",
            target_name = %name,
            result = "validation",
            error = ?e,
        );
        return render_with(
            StatusCode::BAD_REQUEST,
            &FormTpl {
                is_new: true,
                name: &name,
                template_text: &template,
                csrf: &csrf_hex,
                error: Some(e.to_string()),
                user_login: &session.user_login,
                current_page: crate::nav::PINGS,
                members: Vec::new(),
                member_error: None,
            },
        );
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
    let (template_text, members) = {
        let mgr = state.ping_manager.read().await;
        ping_snapshot(&mgr, &name).ok_or_else(|| WebError::Validation {
            field: "name".into(),
            msg: format!("ping `{name}` does not exist"),
        })?
    };

    let csrf_hex = csrf::encode(&session.csrf_value);
    render(&FormTpl {
        is_new: false,
        name: &name,
        template_text: &template_text,
        csrf: &csrf_hex,
        error: None,
        user_login: &session.user_login,
        current_page: crate::nav::PINGS,
        members,
        member_error: None,
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
    let csrf_hex = csrf::encode(&session.csrf_value);
    let template = form.template;

    let mut mgr = state.ping_manager.write().await;
    if let Err(e) = mgr.edit_template(&name, template.clone()) {
        tracing::warn!(
            target: "twitch_1337_web",
            user_id = %session.user_id,
            action = "ping_update",
            target_name = %name,
            result = "validation",
            error = ?e,
        );
        let members = ping_snapshot(&mgr, &name)
            .map(|(_, m)| m)
            .unwrap_or_default();
        return render_with(
            StatusCode::BAD_REQUEST,
            &FormTpl {
                is_new: false,
                name: &name,
                template_text: &template,
                csrf: &csrf_hex,
                error: Some(e.to_string()),
                user_login: &session.user_login,
                current_page: crate::nav::PINGS,
                members,
                member_error: None,
            },
        );
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

/// Twitch login: lowercase ASCII alphanumeric + underscore, 1–25 chars.
/// (Twitch enforces ≤25 chars and that exact charset on signup.) Validated
/// here so a stray space or `@` doesn't end up persisted in the member set.
fn is_valid_twitch_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 25
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[derive(Deserialize)]
struct AddMemberForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    username: String,
}

async fn add_member(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path(name): Path<String>,
    axum::Form(form): axum::Form<AddMemberForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let username = normalize_username(form.username.trim());
    let csrf_hex = csrf::encode(&session.csrf_value);
    let invalid_login = !is_valid_twitch_login(&username);

    let mut mgr = state.ping_manager.write().await;
    let result = if invalid_login {
        Err("username must be 1–25 chars: lowercase letters, digits, underscore".to_owned())
    } else {
        mgr.add_member(&name, &username).map_err(|e| e.to_string())
    };
    let (template_text, members) =
        ping_snapshot(&mgr, &name).ok_or_else(|| WebError::Validation {
            field: "name".into(),
            msg: format!("ping `{name}` does not exist"),
        })?;
    drop(mgr);

    match result {
        Ok(()) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "ping_member_add",
                target_name = %name,
                target_user = %username,
                result = "ok",
            );
            flash::set(&cookies, &format!("Added `{username}` to `{name}`."));
            Ok(Redirect::to(&format!("/pings/{name}")).into_response())
        }
        Err(msg) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "ping_member_add",
                target_name = %name,
                target_user = %username,
                result = "error",
                error = %msg,
            );
            render_with(
                StatusCode::BAD_REQUEST,
                &FormTpl {
                    is_new: false,
                    name: &name,
                    template_text: &template_text,
                    csrf: &csrf_hex,
                    error: None,
                    user_login: &session.user_login,
                    current_page: crate::nav::PINGS,
                    members,
                    member_error: Some(msg),
                },
            )
        }
    }
}

async fn remove_member(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path((name, username)): Path<(String, String)>,
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
    // Idempotent: missing ping or non-member is a no-op so the row swap
    // succeeds without flashing an error to the user.
    let _ = mgr.remove_member(&name, &username);
    drop(mgr);

    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "ping_member_remove",
        target_name = %name,
        target_user = %username,
        result = "ok",
    );
    flash::set(&cookies, &format!("Removed `{username}` from `{name}`."));
    // Empty body so HTMX `hx-swap="outerHTML"` removes the row.
    Ok((StatusCode::OK, "").into_response())
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
