//! Owner-only settings page: live cooldowns + pings runtime knobs.
//!
//! The page reads `state.settings` for the current effective values and
//! `state.settings_store.defaults()` to show the compile-time fallbacks
//! beside each input. Saves go through `SettingsStore::apply`, which
//! validates, atomically persists, swaps the shared handle, and records an
//! audit entry. Reset clears one section (`cooldowns` or `pings`) back to
//! its defaults.

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::settings::{
    Actor, Cooldowns, CooldownsOverrides, FieldError, PingsOverrides, PingsSettings, Settings,
    SettingsError, SettingsOverrides, SettingsSection,
};

use crate::auth::Role;
use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::{render, render_with};
use crate::state::WebState;

pub fn owner_router() -> Router<WebState> {
    Router::new()
        .route("/settings", get(show).post(save))
        .route("/settings/reset/{section}", post(reset))
}

#[derive(Template)]
#[template(path = "settings.html")]
struct ShowTpl {
    csrf: String,
    flash: Option<String>,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
    current: Settings,
    defaults: Settings,
    errors: Vec<FieldError>,
}

async fn show(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let current = (**state.settings.load()).clone();
    let defaults = state.settings_store.defaults().clone();
    render(&ShowTpl {
        csrf: csrf::encode(&session.csrf_value),
        flash: flash::take(&cookies),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SETTINGS,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, Role::Owner),
        current,
        defaults,
        errors: Vec::new(),
    })
}

#[derive(Deserialize)]
struct SaveForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    cooldown_ai: u64,
    cooldown_news: u64,
    cooldown_up: u64,
    cooldown_feedback: u64,
    cooldown_doener: u64,
    ping_cooldown: u64,
    /// Unchecked HTML checkboxes don't submit a value at all, so a missing
    /// `ping_public` key means "false". Form fields with `value="1"` send
    /// `Some("1")` when checked.
    #[serde(default)]
    ping_public: Option<String>,
}

async fn save(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }

    let patch = SettingsOverrides {
        schema_version: twitch_1337_core::settings::SCHEMA_VERSION,
        cooldowns: CooldownsOverrides {
            ai: Some(form.cooldown_ai),
            news: Some(form.cooldown_news),
            up: Some(form.cooldown_up),
            feedback: Some(form.cooldown_feedback),
            doener: Some(form.cooldown_doener),
        },
        pings: PingsOverrides {
            cooldown: Some(form.ping_cooldown),
            public: Some(form.ping_public.is_some()),
        },
    };

    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };

    match state.settings_store.apply(patch, actor).await {
        Ok(_) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "settings_apply",
                result = "ok",
            );
            flash::set(&cookies, "Settings saved.");
            Ok(Redirect::to("/settings").into_response())
        }
        Err(SettingsError::Validation(errors)) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "settings_apply",
                result = "validation",
                error_count = errors.len(),
            );
            // Preserve the user's submitted (raw) values so they can correct
            // the invalid field without having to retype every other input.
            // Spec §7.2: "previously entered values are preserved".
            let submitted = Settings {
                schema_version: twitch_1337_core::settings::SCHEMA_VERSION,
                cooldowns: Cooldowns {
                    ai: form.cooldown_ai,
                    news: form.cooldown_news,
                    up: form.cooldown_up,
                    feedback: form.cooldown_feedback,
                    doener: form.cooldown_doener,
                },
                pings: PingsSettings {
                    cooldown: form.ping_cooldown,
                    public: form.ping_public.is_some(),
                },
            };
            let defaults = state.settings_store.defaults().clone();
            render_with(
                axum::http::StatusCode::BAD_REQUEST,
                &ShowTpl {
                    csrf: csrf::encode(&session.csrf_value),
                    flash: None,
                    user_login: session.user_login.clone(),
                    user_avatar_url: session.avatar_url.clone(),
                    current_page: crate::nav::SETTINGS,
                    is_mod: session.is_mod(),
                    is_broadcaster: session.is_broadcaster,
                    is_owner: matches!(session.role, Role::Owner),
                    current: submitted,
                    defaults,
                    errors,
                },
            )
        }
        Err(e) => Err(WebError::Internal(eyre::eyre!("settings apply: {e}"))),
    }
}

#[derive(Deserialize)]
struct ResetForm {
    #[serde(rename = "_csrf")]
    csrf: String,
}

async fn reset(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path(section): Path<String>,
    axum::Form(form): axum::Form<ResetForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let section = match section.as_str() {
        "cooldowns" => SettingsSection::Cooldowns,
        "pings" => SettingsSection::Pings,
        other => {
            return Err(WebError::Validation {
                field: "section".into(),
                msg: format!("unknown section `{other}`"),
            });
        }
    };
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    state.settings_store.reset(section, actor).await?;
    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "settings_reset",
        section = ?section,
        result = "ok",
    );
    flash::set(&cookies, "Reset to defaults.");
    Ok(Redirect::to("/settings").into_response())
}
