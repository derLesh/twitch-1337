//! Owner-tier (`require_owner`) integration scenarios.
//!
//! These tests focus on the `require_owner` middleware boundary via a
//! small test-only `/_test_owner` route so the middleware can be
//! exercised end-to-end in isolation. The real `/settings` route is
//! covered in `settings_route.rs`. The shape mirrors the pattern in
//! `auth_viewer_tier.rs` and `leaderboard_route.rs`.

mod helpers;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use helpers::{
    FakeHelix, build_state_with_dirs, build_state_with_overrides, cookie_header, insert_session_as,
    install_crypto,
};
use tower::ServiceExt as _;
use twitch_1337_web::WebState;
use twitch_1337_web::auth::{Role, require_owner};

fn empty_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: Default::default(),
    })
}

/// Build a minimal router with a single owner-gated route. We can't reuse
/// `twitch_1337_web::build_router` because no production route is
/// owner-gated yet (Task 9 owns `/settings`). The body just echoes "ok"
/// so the assertion can key off the status code.
fn owner_app(state: WebState) -> Router {
    Router::new()
        .route("/_test_owner", get(|| async { "ok" }))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_owner,
        ))
        .with_state(state)
        .layer(tower_cookies::CookieManagerLayer::new())
}

#[tokio::test]
async fn owner_session_admitted_by_require_owner() {
    install_crypto();
    let (mut state, _td_p, _td_m) = build_state_with_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("42"));

    let (sid, csrf, _bare) = insert_session_as(&state, "42", "alice", Role::Owner);
    let cookie = cookie_header(&sid, &csrf);
    let app = owner_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/_test_owner")
                .method(Method::GET)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "owner whose user_id matches state.owner_id should be admitted",
    );
}

#[tokio::test]
async fn mod_session_rejected_by_require_owner() {
    install_crypto();
    let (mut state, _td_p, _td_m) = build_state_with_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("99"));

    let (sid, csrf, _bare) = insert_session_as(&state, "42", "bob", Role::Mod);
    let cookie = cookie_header(&sid, &csrf);
    let app = owner_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/_test_owner")
                .method(Method::GET)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "Mod session must not satisfy require_owner",
    );
}

#[tokio::test]
async fn owner_dropped_when_owner_id_cleared() {
    install_crypto();
    // `Duration::ZERO` + `StepClock` means every request triggers a recheck.
    let (mut state, _td_p, _td_m) =
        build_state_with_overrides(empty_helix(), Duration::from_secs(0)).await;
    state.owner_id = Some(Arc::from("42"));

    let (sid, csrf, _bare) = insert_session_as(&state, "42", "alice", Role::Owner);
    let cookie = cookie_header(&sid, &csrf);
    let app = owner_app(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/_test_owner")
                .method(Method::GET)
                .header("cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "first request should be 200 while owner_id matches",
    );

    // Clone reuses the same SessionTable Arc, so the existing sid is still
    // valid after we clear owner_id on the new state — mirroring how
    // `auth_viewer_tier::viewer_dropped_from_allowlist_after_recheck_window`
    // mutates the allowlist on a cloned state.
    let mut state2 = state.clone();
    state2.owner_id = None;
    let app2 = owner_app(state2);

    let resp = app2
        .oneshot(
            Request::builder()
                .uri("/_test_owner")
                .method(Method::GET)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "owner_id cleared mid-session should drop the owner session on recheck",
    );
}
