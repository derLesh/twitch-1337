//! Smoke tests for the `/assets/*` route. Ensures the htmx bundle is real
//! (not the development stub) and that assets ship with the expected
//! cache-control header so deploy reviews catch a regression to placeholders.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
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

async fn fetch_asset(path: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn htmx_bundle_is_real_not_a_stub() {
    let (status, _headers, bytes) = fetch_asset("/assets/htmx.min.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        bytes.len() > 40_000,
        "htmx bundle suspiciously small: {} bytes",
        bytes.len()
    );
    let s = std::str::from_utf8(&bytes).expect("htmx must be utf-8");
    // The v1 stub printed "htmx placeholder; templates render but interactivity
    // is stubbed" via console.log. The real bundle never carries that phrase.
    assert!(
        !s.contains("htmx placeholder"),
        "htmx still contains the dev stub"
    );
    assert!(
        !s.contains("TODO: replace"),
        "htmx still contains a TODO stub marker"
    );
    assert!(s.contains("htmx"), "htmx bundle must mention itself");
}

#[tokio::test]
async fn self_hosted_fonts_are_served() {
    for path in [
        "/assets/fonts/geist-latin.woff2",
        "/assets/fonts/geist-mono-latin.woff2",
    ] {
        let (status, headers, bytes) = fetch_asset(path).await;
        assert_eq!(status, StatusCode::OK, "{path} must serve");
        assert!(
            bytes.len() > 5_000,
            "{path} suspiciously small: {} bytes",
            bytes.len()
        );
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "font/woff2",
            "{path} must report woff2 mime",
        );
    }
}

#[tokio::test]
async fn assets_emit_immutable_cache_control() {
    let (status, headers, _bytes) = fetch_asset("/assets/app.css").await;
    assert_eq!(status, StatusCode::OK);
    let cc = headers
        .get(header::CACHE_CONTROL)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cc.contains("immutable"),
        "expected immutable cache-control, got `{cc}`"
    );
}
