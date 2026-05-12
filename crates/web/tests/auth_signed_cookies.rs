//! Smoke test: a tampered sid cookie is rejected by the signed extractor.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::build_router;

mod helpers;
use helpers::{FakeHelix, build_state, insert_session, install_crypto};

#[tokio::test]
async fn tampered_sid_redirects_to_login() {
    install_crypto();
    let state = build_state(std::sync::Arc::new(FakeHelix {
        moderators: vec!["12345".into()],
        users: std::collections::HashMap::new(),
    }))
    .await;
    let (signed_sid, _signed_csrf, _bare_csrf) = insert_session(&state, "12345", "alice");

    // tower-cookies signs as `BASE64(hmac_tag) || original_value`. Flipping
    // a char inside the HMAC tag prefix (first 44 chars) corrupts only the
    // signature; the embedded sid stays intact. A bare cookies.get(...)
    // would still find the live sid in SessionTable — only the signed
    // extractor's HMAC verification rejects this. That's what we want to
    // pin down.
    let mut tampered = signed_sid.clone();
    // SAFETY: base64 chars are ASCII; flipping A↔B preserves UTF-8.
    let bytes = unsafe { tampered.as_bytes_mut() };
    bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };

    let app = build_router(state);
    let req = Request::builder()
        .uri("/pings")
        .header(header::COOKIE, format!("tw1337_sid={tampered}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "tampered sid must redirect to /login (Unauthenticated)"
    );
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    // ?next= will be added in Task 5; for now, must at least begin with /login.
    assert!(location.starts_with("/login"), "got {location}");
}

#[tokio::test]
async fn untampered_sid_passes_through() {
    install_crypto();
    let state = build_state(std::sync::Arc::new(FakeHelix {
        moderators: vec!["12345".into()],
        users: std::collections::HashMap::new(),
    }))
    .await;
    let (signed_sid, _signed_csrf, _bare_csrf) = insert_session(&state, "12345", "alice");

    let app = build_router(state);
    let req = Request::builder()
        .uri("/pings")
        .header(header::COOKIE, format!("tw1337_sid={signed_sid}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
