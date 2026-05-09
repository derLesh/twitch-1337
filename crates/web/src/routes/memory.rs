//! `/memory` viewer + editor routes (Tasks 5–6).
//!
//! Mounts under the authed sub-router so every entry point is mod-gated.
//! GET handlers render read-only viewers and create/edit forms; POST
//! handlers save/create/delete with `MemoryStore::write_with_guard` so a
//! stale mtime token surfaces as a conflict page instead of clobbering an
//! AI/dreamer write.
//!
//! ## Path validation
//!
//! `:user_id` is matched against `^[0-9]{1,32}$` and `:slug` is delegated
//! to `validate_state_slug` (the same rule the store enforces) *before*
//! any filesystem access — together they're the only barrier between an
//! attacker-controlled URL and `MemoryStore::read_kind`. Any other shape
//! returns `WebError::Validation` (400).
//!
//! ## Route precedence
//!
//! `/memory/state/new` is declared *before* `/memory/state/{slug}` so axum
//! matches the literal first. A regression test pins this ordering.

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::ai::memory::store::{WriteError, WriteOutcome, validate_state_slug};
use twitch_1337_core::ai::memory::types::{FileKind, MemoryFile};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::{ConflictPayload, WebError};
use crate::flash;
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/memory", get(tree))
        .route("/memory/soul", get(view_soul).post(save_soul))
        .route("/memory/lore", get(view_lore).post(save_lore))
        .route("/memory/users", get(list_users))
        .route("/memory/users/{user_id}", get(view_user).post(save_user))
        // `/memory/state/new` MUST precede `/memory/state/{slug}` so the
        // literal route wins over the dynamic capture.
        .route("/memory/state/new", get(new_state_form))
        // GET lists state notes; POST creates a new one (collection URL).
        .route("/memory/state", get(list_state).post(create_state))
        .route("/memory/state/{slug}", get(view_state).post(save_state))
        .route("/memory/state/{slug}/delete", post(delete_state))
}

#[derive(Template)]
#[template(path = "memory/tree.html")]
struct TreeTpl {
    user_count: usize,
    state_count: usize,
}

#[derive(Template)]
#[template(path = "memory/editor.html")]
struct EditorTpl<'a> {
    title: &'a str,
    body: &'a str,
    csrf: &'a str,
    mtime: u64,
    byte_cap: usize,
    save_url: &'a str,
    delete_url: Option<&'a str>,
    error: Option<String>,
}

struct StateRow {
    slug: String,
    updated_at: String,
    created_by: String,
}

#[derive(Template)]
#[template(path = "memory/state_list.html")]
struct StateListTpl {
    items: Vec<StateRow>,
}

struct UserRow {
    user_id: String,
    display_name: String,
    updated_at: String,
}

#[derive(Template)]
#[template(path = "memory/users_list.html")]
struct UsersListTpl {
    items: Vec<UserRow>,
}

fn render<T: Template>(tpl: &T) -> Result<Response, WebError> {
    let body = tpl
        .render()
        .map_err(|e| WebError::Internal(eyre::eyre!("render: {e}")))?;
    Ok(Html(body).into_response())
}

fn fmt_ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// `^[0-9]{1,32}$` without pulling in a regex crate — the only allowed
/// shape is a positive-length, ≤32-char ASCII-digit string. This is the
/// sole barrier between an attacker URL and `read_kind`, so it must reject
/// dot-segments, slashes, and percent-encoded byte sequences alike.
fn is_valid_user_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())
}

/// Wraps `validate_state_slug` so the route layer rejects bad slugs with the
/// same rule the store enforces (charset, length, `..`, reserved literals)
/// — `..` is the security-critical case: without it a `/memory/state/..`
/// URL would reach `read_kind` and could escape `memories/state/` to leak
/// SOUL.md or LORE.md via the state viewer.
fn validate_slug(slug: &str) -> Result<(), WebError> {
    validate_state_slug(slug).map_err(|_| WebError::Validation {
        field: "slug".into(),
        msg: "must be 1-64 chars, [a-zA-Z0-9._-], not `new`/`delete`, no `..`".into(),
    })
}

async fn tree(State(state): State<WebState>) -> Result<Response, WebError> {
    let store = &state.memory_store;
    let users = store
        .list_users()
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("list_users: {e}")))?;
    let states = store
        .list_state()
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("list_state: {e}")))?;
    render(&TreeTpl {
        user_count: users.len(),
        state_count: states.len(),
    })
}

async fn view_kind(
    state: &WebState,
    session: &Session,
    kind: FileKind,
    title: String,
    save_url: String,
    delete_url: Option<String>,
) -> Result<Response, WebError> {
    let store = &state.memory_store;
    let mf = store
        .read_kind(&kind)
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("read_kind: {e}")))?;
    let mtime = store
        .current_mtime(&kind)
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("current_mtime: {e}")))?;
    let byte_cap = state.memory_store.caps().limit_for(&kind);
    let csrf_hex = csrf::encode(&session.csrf_value);
    render(&EditorTpl {
        title: &title,
        body: &mf.body,
        csrf: &csrf_hex,
        mtime,
        byte_cap,
        save_url: &save_url,
        delete_url: delete_url.as_deref(),
        error: None,
    })
}

async fn view_soul(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    view_kind(
        &state,
        &session,
        FileKind::Soul,
        "SOUL".to_owned(),
        "/memory/soul".to_owned(),
        None,
    )
    .await
}

async fn view_lore(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    view_kind(
        &state,
        &session,
        FileKind::Lore,
        "LORE".to_owned(),
        "/memory/lore".to_owned(),
        None,
    )
    .await
}

async fn list_users(State(state): State<WebState>) -> Result<Response, WebError> {
    let users: Vec<MemoryFile> = state
        .memory_store
        .list_users()
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("list_users: {e}")))?;
    let items = users
        .into_iter()
        .map(|mf| {
            let user_id = match &mf.kind {
                FileKind::User { user_id } => user_id.clone(),
                _ => String::new(),
            };
            let display_name = mf.frontmatter.display_name.unwrap_or_default();
            UserRow {
                user_id,
                display_name,
                updated_at: fmt_ts(mf.frontmatter.updated_at),
            }
        })
        .collect();
    render(&UsersListTpl { items })
}

async fn view_user(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(user_id): Path<String>,
) -> Result<Response, WebError> {
    if !is_valid_user_id(&user_id) {
        return Err(WebError::Validation {
            field: "user_id".into(),
            msg: "must be numeric, 1-32 digits".into(),
        });
    }
    let title = format!("User {user_id}");
    let save_url = format!("/memory/users/{user_id}");
    view_kind(
        &state,
        &session,
        FileKind::User {
            user_id: user_id.clone(),
        },
        title,
        save_url,
        None,
    )
    .await
}

async fn list_state(State(state): State<WebState>) -> Result<Response, WebError> {
    let items: Vec<MemoryFile> = state
        .memory_store
        .list_state()
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("list_state: {e}")))?;
    let items = items
        .into_iter()
        .map(|mf| {
            let slug = match &mf.kind {
                FileKind::State { slug } => slug.clone(),
                _ => String::new(),
            };
            StateRow {
                slug,
                updated_at: fmt_ts(mf.frontmatter.updated_at),
                created_by: mf.frontmatter.created_by.unwrap_or_default(),
            }
        })
        .collect();
    render(&StateListTpl { items })
}

async fn new_state_form(
    Extension(session): Extension<Session>,
    State(state): State<WebState>,
) -> Result<Response, WebError> {
    let csrf_hex = csrf::encode(&session.csrf_value);
    let cap = state.memory_store.caps().state_bytes;
    render(&EditorTpl {
        title: "new state note",
        body: "",
        csrf: &csrf_hex,
        mtime: 0,
        byte_cap: cap,
        save_url: "/memory/state",
        delete_url: None,
        error: None,
    })
}

async fn view_state(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(slug): Path<String>,
) -> Result<Response, WebError> {
    validate_slug(&slug)?;
    let title = format!("State / {slug}");
    let save_url = format!("/memory/state/{slug}");
    let delete_url = format!("/memory/state/{slug}/delete");
    view_kind(
        &state,
        &session,
        FileKind::State { slug: slug.clone() },
        title,
        save_url,
        Some(delete_url),
    )
    .await
}

#[derive(Deserialize)]
struct SaveForm {
    body: String,
    mtime: u64,
    #[serde(rename = "_csrf")]
    csrf: String,
}

#[derive(Deserialize)]
struct CreateStateForm {
    slug: String,
    body: String,
    #[serde(rename = "_csrf")]
    csrf: String,
}

#[derive(Deserialize)]
struct CsrfOnly {
    #[serde(rename = "_csrf")]
    csrf: String,
}

/// Common save path for SOUL/LORE/users/state. Validates csrf, dispatches
/// to `write_with_guard`, and maps every WriteError variant to a
/// user-facing response so the handler bodies stay one-liners.
// Eight args: state + session + cookies + kind + label + id + form + redirect.
// Splitting them into a struct would just rename the noise without removing it.
#[allow(clippy::too_many_arguments)]
async fn save_kind(
    state: &WebState,
    session: &Session,
    cookies: &Cookies,
    kind: FileKind,
    label: String,
    id: String,
    form: SaveForm,
    redirect_to: String,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let outcome = state
        .memory_store
        .write_with_guard(kind.clone(), &id, &form.body, Some(form.mtime))
        .await;
    match outcome {
        Ok(WriteOutcome::Written { .. }) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_write",
                target_label = %label,
                target_id = %id,
                result = "ok",
            );
            flash::set(cookies, &format!("{label} saved"));
            Ok(Redirect::to(&redirect_to).into_response())
        }
        Ok(WriteOutcome::Conflict {
            current_body,
            current_mtime,
        }) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_write",
                target_label = %label,
                target_id = %id,
                result = "conflict",
            );
            Err(WebError::Conflict(Box::new(ConflictPayload {
                kind: label,
                id,
                current_body,
                current_mtime,
                draft: form.body,
                csrf: csrf::encode(&session.csrf_value),
            })))
        }
        Err(err) => {
            tracing::warn!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_write",
                target_label = %label,
                target_id = %id,
                result = "error",
                error = ?err,
            );
            Err(map_write_error(err))
        }
    }
}

fn map_write_error(err: WriteError) -> WebError {
    match err {
        WriteError::Full => WebError::Validation {
            field: "body".into(),
            msg: "exceeds byte cap".into(),
        },
        WriteError::StateFull => WebError::Validation {
            field: "slug".into(),
            msg: "state collection full".into(),
        },
        WriteError::InvalidSlug => WebError::Validation {
            field: "slug".into(),
            msg: "reserved or invalid".into(),
        },
        WriteError::Io(e) => WebError::Internal(e),
    }
}

async fn save_soul(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::Soul,
        "SOUL".to_owned(),
        String::new(),
        form,
        "/memory/soul".to_owned(),
    )
    .await
}

async fn save_lore(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::Lore,
        "LORE".to_owned(),
        String::new(),
        form,
        "/memory/lore".to_owned(),
    )
    .await
}

async fn save_user(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path(user_id): Path<String>,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    if !is_valid_user_id(&user_id) {
        return Err(WebError::Validation {
            field: "user_id".into(),
            msg: "must be numeric, 1-32 digits".into(),
        });
    }
    let redirect = format!("/memory/users/{user_id}");
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::User {
            user_id: user_id.clone(),
        },
        format!("user {user_id}"),
        user_id,
        form,
        redirect,
    )
    .await
}

async fn save_state(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path(slug): Path<String>,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    validate_slug(&slug)?;
    let redirect = format!("/memory/state/{slug}");
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::State { slug: slug.clone() },
        format!("state {slug}"),
        slug,
        form,
        redirect,
    )
    .await
}

async fn create_state(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<CreateStateForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    validate_slug(&form.slug)?;
    let slug = form.slug.clone();
    match state
        .memory_store
        .write_state(
            &FileKind::State { slug: slug.clone() },
            &form.body,
            Some(&session.user_id),
        )
        .await
    {
        Ok(()) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_create",
                target_label = "state",
                target_id = %slug,
                result = "ok",
            );
            flash::set(&cookies, &format!("state `{slug}` created"));
            Ok(Redirect::to(&format!("/memory/state/{slug}")).into_response())
        }
        Err(err) => {
            tracing::warn!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_create",
                target_label = "state",
                target_id = %slug,
                result = "error",
                error = ?err,
            );
            Err(map_write_error(err))
        }
    }
}

async fn delete_state(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(slug): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<CsrfOnly>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    validate_slug(&slug)?;
    state
        .memory_store
        .delete_state(&slug)
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("delete_state: {e}")))?;
    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "memory_delete",
        target_label = "state",
        target_id = %slug,
        result = "ok",
    );
    flash::set(&cookies, &format!("state `{slug}` deleted"));
    Ok(Redirect::to("/memory/state").into_response())
}
