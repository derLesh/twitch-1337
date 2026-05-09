//! End-to-end tests for the auth router and mod-gate middleware.
//!
//! Drives `build_router` with a fake helix client through `tower::ServiceExt::oneshot`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::build_router;
use twitch_1337_web::helix::HelixClient;

mod helpers;
use helpers::{FakeHelix, build_state, install_crypto};

fn fake_helix() -> Arc<dyn HelixClient> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    })
}

#[tokio::test]
async fn root_redirects_to_pings_for_authed_request_only() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    // Without a session, `/` (which requires mod) redirects to /login.
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        location, "/login",
        "unauth root must redirect to /login (post-login deep-link not wired in v1)",
    );
}

#[tokio::test]
async fn login_route_redirects_to_twitch() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/login")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        location.starts_with("https://id.twitch.tv/oauth2/authorize"),
        "expected twitch authorize URL, got {location}"
    );
}

#[tokio::test]
async fn healthz_is_public() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn assets_serve_embedded_files() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/assets/app.css")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get(header::CONTENT_TYPE).unwrap(), "text/css");
}
