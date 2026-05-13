//! `/memory` viewer + editor routes.
//!
//! POST handlers go through `MemoryStore::write_with_guard` so a stale mtime
//! token surfaces as a conflict page instead of clobbering an AI/dreamer write.
//!
//! `:user_id` and `:slug` are validated *before* any filesystem access —
//! together they're the only barrier between an attacker URL and
//! `MemoryStore::read_kind`. `/memory/state/new` is declared *before*
//! `/memory/state/{slug}` so axum matches the literal first; a regression
//! test pins this ordering.

use std::collections::HashMap;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::ai::memory::store::{
    FrontmatterOverride, WriteError, WriteOutcome, validate_state_slug,
};
use twitch_1337_core::ai::memory::types::{FileKind, MemoryFile};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::{ConflictPayload, WebError};
use crate::flash;
use crate::routes::{initial_of, render, render_with};
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/memory", get(tree))
        .route("/memory/soul", get(view_soul).post(save_soul))
        .route("/memory/lore", get(view_lore).post(save_lore))
        .route("/memory/users", get(list_users))
        // `/memory/users/new` MUST precede `/memory/users/{user_id}` so the
        // literal route wins over the dynamic capture.
        .route("/memory/users/new", get(new_user_form).post(create_user))
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
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

#[derive(Template)]
#[template(path = "memory/editor.html")]
struct EditorTpl<'a> {
    title: &'a str,
    subtitle: &'a str,
    meta_path: String,
    body: &'a str,
    csrf: &'a str,
    mtime: u64,
    mtime_display: String,
    byte_cap: usize,
    /// Pre-computed `body.len() * 100 / byte_cap`, clamped to 100.
    /// Lives here (not the template) because Askama can't divide a
    /// `usize` into a percentage cleanly.
    pct: u8,
    save_url: &'a str,
    cancel_url: &'a str,
    delete_url: Option<&'a str>,
    error: Option<String>,
    user_login: &'a str,
    user_avatar_url: Option<&'a str>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
    /// Render the user-only frontmatter inputs (`username`, `display_name`).
    show_user_fm: bool,
    /// Render the state-only frontmatter input (`created_by`).
    show_state_fm: bool,
    fm_username: &'a str,
    fm_display_name: &'a str,
    fm_created_by: &'a str,
}

struct StateRow {
    slug: String,
    updated_at: String,
    created_by: String,
    /// Body byte count for the mini-bar in the state list.
    bytes: usize,
    /// `min(100, bytes * 100 / state_cap)` — pre-computed for the same
    /// reason as `EditorTpl::pct`.
    pct: u8,
    created_initial: String,
    /// `None` falls back to initial-circle in the template.
    profile_image_url: Option<String>,
}

#[derive(Template)]
#[template(path = "memory/state_list.html")]
struct StateListTpl {
    items: Vec<StateRow>,
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

struct UserRow {
    user_id: String,
    display_name: String,
    updated_at: String,
    /// First non-frontmatter line, truncated to 140 chars — rendered in
    /// the user card body excerpt.
    note: String,
    /// Uppercased first character of the display name (or `?` if empty)
    /// for the avatar circle.
    initial: String,
    /// `None` falls back to the initial-circle in the template.
    profile_image_url: Option<String>,
}

#[derive(Template)]
#[template(path = "memory/users_list.html")]
struct UsersListTpl {
    items: Vec<UserRow>,
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

#[derive(Template)]
#[template(path = "memory/users_new.html")]
struct UsersNewTpl {
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

/// Resolve avatar URLs for a slice of Twitch user ids. Cache hits skip
/// helix entirely; cache misses fan out through a single batched helix
/// call. A helix error logs and returns whatever the cache held.
async fn fetch_avatars(state: &WebState, ids: &[&str]) -> HashMap<String, String> {
    let mut dedup: Vec<&str> = ids.to_vec();
    dedup.sort_unstable();
    dedup.dedup();
    let lookup = state
        .avatar_cache
        .lookup(&dedup, state.clock.as_ref())
        .await;
    let mut avatars = lookup.cached;
    if lookup.missing.is_empty() {
        return avatars;
    }
    let missing_refs: Vec<&str> = lookup.missing.iter().map(String::as_str).collect();
    match state.helix.fetch_users_by_ids(&missing_refs).await {
        Ok(found) => {
            state
                .avatar_cache
                .insert(&lookup.missing, &found, state.clock.as_ref())
                .await;
            for u in found {
                if let Some(url) = u.profile_image_url {
                    avatars.insert(u.id, url);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "twitch_1337_web",
                error = ?e,
                "helix fetch_users_by_ids failed; falling back to initials",
            );
        }
    }
    avatars
}

fn fmt_ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Render a `MemoryStore::current_mtime` value (millis since epoch) as a
/// human date. The raw u64 stays threaded into the hidden form input so
/// the optimistic-concurrency guard sees byte-identical millis after the
/// round-trip; this is purely for human display.
fn fmt_mtime_ms(ms: u64) -> String {
    if ms == 0 {
        return "new".to_owned();
    }
    let signed = i64::try_from(ms).unwrap_or(i64::MAX);
    DateTime::<Utc>::from_timestamp_millis(signed)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "—".to_owned())
}

/// `(used * 100 / cap).min(100)` as a `u8`. Clamps to 100 on `cap == 0`.
fn pct_of(used: usize, cap: usize) -> u8 {
    if cap == 0 {
        return 100;
    }
    let raw = used.saturating_mul(100) / cap;
    raw.min(100) as u8
}

fn meta_path_for(kind: &FileKind) -> String {
    format!("memories/{}", kind.relative_path().display())
}

fn subtitle_for_kind(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Soul => "The bot's stable self-description. Read on every `!ai` turn.",
        FileKind::Lore => "Channel-wide history and running threads.",
        FileKind::User { .. } => "Per-user character sheet.",
        FileKind::State { .. } => "Persistent state note.",
    }
}

/// Section list URL — the page Cancel/Back should return to. SOUL/LORE
/// have no list page so they fall back to the tree.
fn list_url_for(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Soul | FileKind::Lore => "/memory",
        FileKind::User { .. } => "/memory/users",
        FileKind::State { .. } => "/memory/state",
    }
}

fn nav_slug_for(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Soul => crate::nav::MEMORY_SOUL,
        FileKind::Lore => crate::nav::MEMORY_LORE,
        FileKind::User { .. } => crate::nav::MEMORY_USERS,
        FileKind::State { .. } => crate::nav::MEMORY_STATE,
    }
}

/// First non-empty line, stripped of a leading `# `, truncated to 140 chars.
/// Char-aware so we never slice mid-codepoint.
fn note_excerpt(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.strip_prefix("# ").unwrap_or(line);
    line.chars().take(140).collect()
}

/// `^[0-9]{1,32}$` without a regex crate. Sole barrier between an attacker URL
/// and `read_kind`, so it must reject dot-segments, slashes, and percent-encoded
/// byte sequences alike.
fn is_valid_user_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())
}

/// `..` is the security-critical case: without it a `/memory/state/..` URL would
/// reach `read_kind` and could escape `memories/state/` to leak SOUL.md or
/// LORE.md via the state viewer.
fn validate_slug(slug: &str) -> Result<(), WebError> {
    validate_state_slug(slug).map_err(|_| WebError::Validation {
        field: "slug".into(),
        msg: "must be 1-64 chars, [a-zA-Z0-9._-], not `new`/`delete`, no `..`".into(),
    })
}

async fn tree(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
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
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::MEMORY_TREE,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    })
}

async fn view_kind(
    state: &WebState,
    session: &Session,
    kind: FileKind,
    title: String,
    save_url: String,
    delete_url: Option<String>,
    preloaded: Option<MemoryFile>,
) -> Result<Response, WebError> {
    let cancel_url = list_url_for(&kind);
    let current_page = nav_slug_for(&kind);
    let store = &state.memory_store;
    let mf = match preloaded {
        Some(mf) => mf,
        None => store
            .read_kind(&kind)
            .await
            .map_err(|e| WebError::Internal(eyre::eyre!("read_kind: {e}")))?,
    };
    let mtime = store
        .current_mtime(&kind)
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("current_mtime: {e}")))?;
    let byte_cap = state.memory_store.caps().limit_for(&kind);
    let csrf_hex = csrf::encode(&session.csrf_value);
    let pct = pct_of(mf.body.len(), byte_cap);
    render(&EditorTpl {
        title: &title,
        subtitle: subtitle_for_kind(&kind),
        meta_path: meta_path_for(&kind),
        body: &mf.body,
        csrf: &csrf_hex,
        mtime,
        mtime_display: fmt_mtime_ms(mtime),
        byte_cap,
        pct,
        save_url: &save_url,
        cancel_url,
        delete_url: delete_url.as_deref(),
        error: None,
        user_login: &session.user_login,
        user_avatar_url: session.avatar_url.as_deref(),
        current_page,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
        show_user_fm: matches!(kind, FileKind::User { .. }),
        show_state_fm: matches!(kind, FileKind::State { .. }),
        fm_username: mf.frontmatter.username.as_deref().unwrap_or(""),
        fm_display_name: mf.frontmatter.display_name.as_deref().unwrap_or(""),
        fm_created_by: mf.frontmatter.created_by.as_deref().unwrap_or(""),
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
        None,
    )
    .await
}

async fn list_users(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    let users: Vec<MemoryFile> = state
        .memory_store
        .list_users()
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("list_users: {e}")))?;

    let user_ids: Vec<&str> = users
        .iter()
        .filter_map(|mf| match &mf.kind {
            FileKind::User { user_id } => Some(user_id.as_str()),
            _ => None,
        })
        .collect();
    let avatars = fetch_avatars(&state, &user_ids).await;

    let items = users
        .into_iter()
        .map(|mf| {
            let user_id = match &mf.kind {
                FileKind::User { user_id } => user_id.clone(),
                _ => String::new(),
            };
            let display_name = mf.frontmatter.display_name.unwrap_or_default();
            let initial = initial_of(if display_name.is_empty() {
                &user_id
            } else {
                &display_name
            });
            let note = note_excerpt(&mf.body);
            let profile_image_url = avatars.get(&user_id).cloned();
            UserRow {
                user_id,
                display_name,
                updated_at: fmt_ts(mf.frontmatter.updated_at),
                note,
                initial,
                profile_image_url,
            }
        })
        .collect();
    render(&UsersListTpl {
        items,
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::MEMORY_USERS,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    })
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
    let kind = FileKind::User {
        user_id: user_id.clone(),
    };
    let mf = state
        .memory_store
        .read_kind(&kind)
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("read_kind: {e}")))?;
    let title = mf
        .frontmatter
        .display_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(mf.frontmatter.username.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or(&user_id)
        .to_owned();
    let save_url = format!("/memory/users/{user_id}");
    view_kind(&state, &session, kind, title, save_url, None, Some(mf)).await
}

#[derive(Deserialize)]
struct NewUserForm {
    user_id: String,
    #[serde(rename = "_csrf")]
    csrf: String,
}

async fn new_user_form(Extension(session): Extension<Session>) -> Result<Response, WebError> {
    render(&UsersNewTpl {
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::MEMORY_USERS,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    })
}

/// Reads a numeric `user_id` from the form, validates, and redirects to
/// the editor at `/memory/users/{id}`. The editor renders an empty file
/// on first GET because `read_kind` synthesizes a blank `MemoryFile` for
/// missing paths, so no on-disk row is created until the user saves.
async fn create_user(
    Extension(session): Extension<Session>,
    axum::Form(form): axum::Form<NewUserForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let user_id = form.user_id.trim();
    if !is_valid_user_id(user_id) {
        return Err(WebError::Validation {
            field: "user_id".into(),
            msg: "must be numeric, 1-32 digits".into(),
        });
    }
    Ok(Redirect::to(&format!("/memory/users/{user_id}")).into_response())
}

async fn list_state(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    let items: Vec<MemoryFile> = state
        .memory_store
        .list_state()
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("list_state: {e}")))?;
    let cap = state.memory_store.caps().state_bytes;
    let creator_ids: Vec<&str> = items
        .iter()
        .filter_map(|mf| {
            mf.frontmatter
                .created_by
                .as_deref()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    let avatars = fetch_avatars(&state, &creator_ids).await;
    let items = items
        .into_iter()
        .map(|mf| {
            let slug = match &mf.kind {
                FileKind::State { slug } => slug.clone(),
                _ => String::new(),
            };
            let bytes = mf.body.len();
            let created_by = mf.frontmatter.created_by.unwrap_or_default();
            let created_initial = initial_of(&created_by);
            let profile_image_url = avatars.get(&created_by).cloned();
            StateRow {
                slug,
                updated_at: fmt_ts(mf.frontmatter.updated_at),
                created_by,
                bytes,
                pct: pct_of(bytes, cap),
                created_initial,
                profile_image_url,
            }
        })
        .collect();
    render(&StateListTpl {
        items,
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::MEMORY_STATE,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    })
}

async fn new_state_form(
    Extension(session): Extension<Session>,
    State(state): State<WebState>,
) -> Result<Response, WebError> {
    let cap = state.memory_store.caps().state_bytes;
    let csrf_hex = csrf::encode(&session.csrf_value);
    render_state_create(
        StatusCode::OK,
        "",
        None,
        cap,
        &csrf_hex,
        &session.user_login,
        session.avatar_url.as_deref(),
        session.is_mod(),
        session.is_broadcaster,
        matches!(session.role, crate::auth::Role::Owner),
    )
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
        None,
    )
    .await
}

#[derive(Deserialize)]
struct SaveForm {
    body: String,
    mtime: u64,
    #[serde(rename = "_csrf")]
    csrf: String,
    /// Empty string = preserve prior on-disk value.
    #[serde(default)]
    fm_username: String,
    #[serde(default)]
    fm_display_name: String,
    #[serde(default)]
    fm_created_by: String,
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

/// Validates csrf, dispatches to `write_with_guard`, and on
/// `WriteError::{Full,StateFull,InvalidSlug}` re-renders the originating
/// editor with the user's draft + an inline error so the work isn't lost.
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
    cap: usize,
    delete_url: Option<String>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let current_page = nav_slug_for(&kind);
    let cancel_url = list_url_for(&kind);
    let fm_override = FrontmatterOverride {
        username: Some(form.fm_username.clone()),
        display_name: Some(form.fm_display_name.clone()),
        created_by: Some(form.fm_created_by.clone()),
    };
    let outcome = state
        .memory_store
        .write_with_guard(kind.clone(), &id, &form.body, Some(form.mtime), fm_override)
        .await;
    let csrf_hex = csrf::encode(&session.csrf_value);
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
                current_mtime_display: fmt_mtime_ms(current_mtime),
                current_mtime,
                draft: form.body,
                csrf: csrf_hex,
                user_login: session.user_login.clone(),
                user_avatar_url: session.avatar_url.clone(),
                is_mod: session.is_mod(),
                is_broadcaster: session.is_broadcaster,
                is_owner: matches!(session.role, crate::auth::Role::Owner),
                current_page,
                cancel_url,
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
            // Render the editor with the user's draft preserved. `Io` is the
            // only variant that lacks a meaningful form context — bubble it.
            let msg = match err {
                WriteError::Full => "exceeds byte cap".to_owned(),
                WriteError::StateFull => "state collection full".to_owned(),
                WriteError::InvalidSlug => "reserved or invalid slug".to_owned(),
                WriteError::Io(e) => return Err(WebError::Internal(e)),
            };
            render_with(
                StatusCode::BAD_REQUEST,
                &EditorTpl {
                    title: &label,
                    subtitle: subtitle_for_kind(&kind),
                    meta_path: meta_path_for(&kind),
                    body: &form.body,
                    csrf: &csrf_hex,
                    mtime: form.mtime,
                    mtime_display: fmt_mtime_ms(form.mtime),
                    byte_cap: cap,
                    pct: pct_of(form.body.len(), cap),
                    save_url: &redirect_to,
                    cancel_url,
                    delete_url: delete_url.as_deref(),
                    error: Some(msg),
                    user_login: &session.user_login,
                    user_avatar_url: session.avatar_url.as_deref(),
                    current_page,
                    is_mod: session.is_mod(),
                    is_broadcaster: session.is_broadcaster,
                    is_owner: matches!(session.role, crate::auth::Role::Owner),
                    show_user_fm: matches!(&kind, FileKind::User { .. }),
                    show_state_fm: matches!(&kind, FileKind::State { .. }),
                    fm_username: &form.fm_username,
                    fm_display_name: &form.fm_display_name,
                    fm_created_by: &form.fm_created_by,
                },
            )
        }
    }
}

async fn save_soul(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    let cap = state.memory_store.caps().soul_bytes;
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::Soul,
        "SOUL".to_owned(),
        String::new(),
        form,
        "/memory/soul".to_owned(),
        cap,
        None,
    )
    .await
}

async fn save_lore(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    let cap = state.memory_store.caps().lore_bytes;
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::Lore,
        "LORE".to_owned(),
        String::new(),
        form,
        "/memory/lore".to_owned(),
        cap,
        None,
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
    let cap = state.memory_store.caps().user_bytes;
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
        cap,
        None,
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
    let delete_url = format!("/memory/state/{slug}/delete");
    let cap = state.memory_store.caps().state_bytes;
    save_kind(
        &state,
        &session,
        &cookies,
        FileKind::State { slug: slug.clone() },
        format!("state {slug}"),
        slug,
        form,
        redirect,
        cap,
        Some(delete_url),
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
    let csrf_hex = csrf::encode(&session.csrf_value);
    let cap = state.memory_store.caps().state_bytes;

    if let Err(e) = validate_slug(&form.slug) {
        let msg = match e {
            WebError::Validation { msg, .. } => msg,
            // `validate_slug` only ever produces a `Validation` error; treat
            // anything else as a programming bug rather than papering over
            // it with a wrong user-facing message.
            other => return Err(other),
        };
        return render_state_create(
            StatusCode::BAD_REQUEST,
            &form.body,
            Some(msg),
            cap,
            &csrf_hex,
            &session.user_login,
            session.avatar_url.as_deref(),
            session.is_mod(),
            session.is_broadcaster,
            matches!(session.role, crate::auth::Role::Owner),
        );
    }
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
            let msg = match err {
                WriteError::Full => "exceeds byte cap".to_owned(),
                WriteError::StateFull => "state collection full".to_owned(),
                WriteError::InvalidSlug => "reserved or invalid slug".to_owned(),
                WriteError::Io(e) => return Err(WebError::Internal(e)),
            };
            render_state_create(
                StatusCode::BAD_REQUEST,
                &form.body,
                Some(msg),
                cap,
                &csrf_hex,
                &session.user_login,
                session.avatar_url.as_deref(),
                session.is_mod(),
                session.is_broadcaster,
                matches!(session.role, crate::auth::Role::Owner),
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
// 8 args is just past clippy's default of 7. Threading a struct here would
// obscure the call sites without trimming any state; allow it locally.
fn render_state_create(
    status: StatusCode,
    body: &str,
    error: Option<String>,
    cap: usize,
    csrf_hex: &str,
    user_login: &str,
    user_avatar_url: Option<&str>,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
) -> Result<Response, WebError> {
    render_with(
        status,
        &EditorTpl {
            title: "new state note",
            subtitle: "A persistent note keyed by slug.",
            meta_path: "memories/state/<slug>.md".to_owned(),
            body,
            csrf: csrf_hex,
            mtime: 0,
            mtime_display: fmt_mtime_ms(0),
            byte_cap: cap,
            pct: pct_of(body.len(), cap),
            save_url: "/memory/state",
            cancel_url: "/memory/state",
            delete_url: None,
            error,
            user_login,
            user_avatar_url,
            current_page: crate::nav::MEMORY_STATE,
            is_mod,
            is_broadcaster,
            is_owner,
            show_user_fm: false,
            show_state_fm: false,
            fm_username: "",
            fm_display_name: "",
            fm_created_by: "",
        },
    )
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
