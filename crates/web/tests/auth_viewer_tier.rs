//! Viewer-tier (allowlist) integration scenarios.

mod helpers;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use helpers::{
    FakeHelix, build_state_with_dirs, build_state_with_overrides, cookie_header, insert_session_as,
    install_crypto,
};
use tower::ServiceExt as _;
use twitch_1337_web::auth::Role;

fn empty_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: Default::default(),
    })
}

#[tokio::test]
async fn viewer_can_read_pings_leaderboard_flights() {
    install_crypto();
    let (state, _td_p, _td_m) = build_state_with_dirs(empty_helix()).await;
    let (sid, csrf, _bare) = insert_session_as(&state, "42", "alice", Role::Viewer);
    let cookie = cookie_header(&sid, &csrf);
    let app = twitch_1337_web::build_router(state.clone());

    for path in ["/pings", "/leaderboard", "/flights"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .method(Method::GET)
                    .header("cookie", cookie.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "viewer GET {path}");
    }
}

#[tokio::test]
async fn viewer_blocked_from_memory_and_mutations() {
    install_crypto();
    let (state, _td_p, _td_m) = build_state_with_dirs(empty_helix()).await;
    let (sid, csrf, _bare) = insert_session_as(&state, "42", "alice", Role::Viewer);
    let cookie = cookie_header(&sid, &csrf);
    let app = twitch_1337_web::build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/memory/soul")
                .method(Method::GET)
                .header("cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "viewer cannot read memory"
    );

    // POST to a mod-only route; require_role(Mod) should reject with 403.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/pings/anything/delete")
                .method(Method::POST)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "viewer cannot mutate pings"
    );
}

#[tokio::test]
async fn root_redirects_by_role() {
    install_crypto();
    let helix = Arc::new(FakeHelix {
        moderators: vec!["9".into()],
        users: Default::default(),
    });
    let (state, _td_p, _td_m) = build_state_with_dirs(helix).await;
    let (viewer_sid, viewer_csrf, _) = insert_session_as(&state, "42", "alice", Role::Viewer);
    let (mod_sid, mod_csrf, _) = insert_session_as(&state, "9", "boss", Role::Mod);
    let app = twitch_1337_web::build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .method(Method::GET)
                .header("cookie", cookie_header(&viewer_sid, &viewer_csrf))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_redirection(), "viewer / should redirect");
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/leaderboard",
        "viewer / should redirect to /leaderboard"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .method(Method::GET)
                .header("cookie", cookie_header(&mod_sid, &mod_csrf))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_redirection(), "mod / should redirect");
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/pings",
        "mod / should redirect to /pings"
    );
}

#[tokio::test]
async fn viewer_dropped_from_allowlist_after_recheck_window() {
    install_crypto();
    let (mut state, _td_p, _td_m) =
        build_state_with_overrides(empty_helix(), Duration::from_secs(0)).await;
    state.viewer_allowlist = Arc::from(vec!["42".to_owned()].into_boxed_slice());

    let (sid, csrf, _bare) = insert_session_as(&state, "42", "alice", Role::Viewer);
    let cookie = cookie_header(&sid, &csrf);
    let app = twitch_1337_web::build_router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/leaderboard")
                .method(Method::GET)
                .header("cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "first request should be 200");

    // Clone reuses the same SessionTable Arc, so the existing sid is still
    // valid after we clear the allowlist on the new state.
    let mut state2 = state.clone();
    state2.viewer_allowlist = Arc::from(Vec::<String>::new().into_boxed_slice());
    let app2 = twitch_1337_web::build_router(state2);

    let resp = app2
        .oneshot(
            Request::builder()
                .uri("/leaderboard")
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
        "viewer dropped from allowlist should be 403 on next recheck",
    );
}
