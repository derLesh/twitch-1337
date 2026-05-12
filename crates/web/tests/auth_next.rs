//! Post-login `?next=` deep-link tests.
//!
//! Covers:
//! - `is_safe_redirect` validator semantics (scheme/host/CRLF/length).
//! - `require_mod` capturing the requested path into `?next=` on
//!   `WebError::Unauthenticated`.
//! - `/login` silently dropping unsafe `?next=` values rather than
//!   stashing them in the `tw1337_next` cookie.
//!
//! The callback round-trip (next consumed → redirect to that path) needs a
//! wiremock + cookie injection setup that the existing `auth_routes.rs`
//! test doesn't have. Deferred to live smoke testing — the unit tests
//! below cover validation + the middleware capture path, which is the
//! security-critical part.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::build_router;
use twitch_1337_web::error::is_safe_redirect;
use twitch_1337_web::helix::HelixClient;

mod helpers;
use helpers::{FakeHelix, build_state, install_crypto};

fn fake_helix() -> Arc<dyn HelixClient> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    })
}

#[test]
fn safe_redirect_rejects_scheme_and_host() {
    assert!(is_safe_redirect("/pings"));
    assert!(is_safe_redirect("/memory/state/notes"));
    assert!(!is_safe_redirect("//evil.example/x"));
    assert!(!is_safe_redirect("https://evil.example/"));
    assert!(!is_safe_redirect("javascript:alert(1)"));
    assert!(!is_safe_redirect("/path\r\nSet-Cookie: x=1"));
    assert!(!is_safe_redirect("/\\evil.example/x"));
    assert!(!is_safe_redirect("/foo\\bar"));
    assert!(!is_safe_redirect(&"/".repeat(257)));
}

#[tokio::test]
async fn unauth_request_redirects_to_login_with_next() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/memory/state/notes")
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
    assert_eq!(
        location, "/login?next=%2Fmemory%2Fstate%2Fnotes",
        "expected exact encoded next path, got {location}"
    );
}

#[tokio::test]
async fn login_with_unsafe_next_drops_it_silently() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/login?next=https://evil.example/x")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    // Login still redirects to twitch authorize; the unsafe next must NOT
    // appear as a Set-Cookie value.
    let set_cookie = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !set_cookie.contains("tw1337_next="),
        "unsafe next must not be stashed; saw: {set_cookie}"
    );
}
