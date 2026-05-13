//! Integration tests for the owner-only `/settings` page.
//!
//! Drives `build_router` so the owner middleware + handler chain are
//! exercised end-to-end. The fixture seeds the session table with an
//! `owner_id` that matches the inserted Owner session so the periodic
//! role recheck inside `require_owner` admits cleanly.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::auth::Role;
use twitch_1337_web::build_router;

mod helpers;
use helpers::{
    FakeHelix, build_state_with_all_dirs, cookie_header, insert_session_as, install_crypto,
};

fn empty_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: Default::default(),
    })
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn owner_can_save_cooldown_and_handle_reflects_change() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let app = build_router(state.clone());
    // Construct a fully-populated form. The save handler treats every field
    // as authoritative, so we send the current value for everything except
    // the one knob we're changing.
    let defaults = state.settings_store.defaults().clone();
    let body = format!(
        "_csrf={csrf}&cooldown_ai=15&cooldown_news={n}&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(&bare_csrf),
        n = defaults.cooldowns.news,
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "owner save should redirect (303)",
    );
    assert_eq!(
        res.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/settings"),
    );
    assert_eq!(
        state.settings.load().cooldowns.ai,
        15,
        "live handle must reflect saved cooldown",
    );
}

#[tokio::test]
async fn non_owner_get_settings_returns_403() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    // Owner is someone else; the mod session must NOT be admitted as owner.
    state.owner_id = Some(Arc::from("999"));
    let (sid, csrf_cookie, _bare) = insert_session_as(&state, "42", "modder", Role::Mod);

    let app = build_router(state);
    let req = Request::builder()
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "Mod session must not satisfy require_owner on /settings",
    );
}

#[tokio::test]
async fn validation_error_renders_form_with_errors() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let before = state.settings.load().cooldowns.ai;
    let app = build_router(state.clone());
    let defaults = state.settings_store.defaults().clone();
    // The submitted news value (900) is valid and *different* from the
    // default (60), so the re-rendered form must echo it back to verify
    // we don't discard the user's other typed input on a validation error.
    assert_ne!(
        defaults.cooldowns.news, 900,
        "fixture assumes the default news cooldown is not already 900",
    );
    // cooldown_ai = 0 violates the 1..=3600 bound.
    let body = format!(
        "_csrf={csrf}&cooldown_ai=0&cooldown_news=900&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(&bare_csrf),
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "validation failure must render form with 400",
    );
    let html = body_string(res).await;
    assert!(
        html.contains("cooldowns.ai"),
        "response must surface the failing field; got: {html}"
    );
    // Submitted-value preservation: the valid sibling field (news=900) must
    // be echoed back into the form, not replaced by the stored default.
    assert!(
        html.contains("value=\"900\""),
        "submitted cooldown_news must be preserved on validation error; got: {html}"
    );
    assert!(
        !html.contains(&format!(
            "name=\"cooldown_news\" min=\"1\" max=\"3600\" value=\"{}\"",
            defaults.cooldowns.news
        )),
        "the stored default for news ({}) must NOT replace the submitted 900",
        defaults.cooldowns.news,
    );
    assert_eq!(
        state.settings.load().cooldowns.ai,
        before,
        "validation rejection must not mutate the live handle",
    );
}

#[tokio::test]
async fn reset_cooldowns_clears_section_overrides() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let defaults = state.settings_store.defaults().clone();
    // First push a non-default value via /settings POST.
    let app = build_router(state.clone());
    let body = format!(
        "_csrf={csrf}&cooldown_ai=15&cooldown_news={n}&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(&bare_csrf),
        n = defaults.cooldowns.news,
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(state.settings.load().cooldowns.ai, 15);

    // Then reset just the cooldowns section.
    let app = build_router(state.clone());
    let body = format!("_csrf={csrf}", csrf = urlencoding::encode(&bare_csrf));
    let req = Request::builder()
        .method("POST")
        .uri("/settings/reset/cooldowns")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "reset should redirect on success",
    );
    assert_eq!(
        state.settings.load().cooldowns.ai,
        defaults.cooldowns.ai,
        "reset must restore the compiled default",
    );
}
