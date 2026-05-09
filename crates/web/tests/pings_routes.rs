//! Integration tests for `/pings` CRUD routes.
//!
//! Drives `build_router` directly via `tower::ServiceExt::oneshot`. Sessions
//! are pre-seeded so the require_mod middleware admits without an OAuth
//! round-trip; the FakeHelix client returns `is_moderator = true` for the
//! seeded user so the periodic mod-recheck inside `require_mod` doesn't
//! deny.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::build_router;
use twitch_1337_web::helix::{HelixClient, HelixUser};

mod helpers;
use helpers::{
    FakeHelix, build_state_with_ping_dir, cookie_header, insert_session, install_crypto,
};

/// FakeHelix flavored to admit the test admin during periodic mod rechecks.
fn admin_helix(user_id: &str) -> Arc<dyn HelixClient> {
    let mut users = HashMap::new();
    users.insert(
        user_id.to_owned(),
        HelixUser {
            id: user_id.to_owned(),
            login: "admin".into(),
            display_name: "admin".into(),
        },
    );
    Arc::new(FakeHelix {
        moderators: vec![user_id.to_owned()],
        users,
    })
}

/// Build (state, sid, csrf, _td) — `_td` keeps the ping data dir alive while
/// tests exercise PingManager's atomic save+rename. Drop it to clean up.
async fn authed_setup() -> (twitch_1337_web::WebState, String, String, tempfile::TempDir) {
    install_crypto();
    let user_id = "9001";
    let helix = admin_helix(user_id);
    let (state, td) = build_state_with_ping_dir(helix).await;
    let (sid, csrf) = insert_session(&state, user_id, "admin");
    (state, sid, csrf, td)
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn list_renders_existing_pings() {
    let (state, sid, csrf, _td) = authed_setup().await;
    {
        let mut mgr = state.ping_manager.write().await;
        mgr.create_ping("team".into(), "Hey {mentions}".into(), "admin".into(), None)
            .unwrap();
    }
    let app = build_router(state);
    let req = Request::builder()
        .uri("/pings")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("team"),
        "list should contain ping name; got {html}"
    );
    assert!(
        html.contains("Hey {mentions}"),
        "list should show template; got {html}"
    );
}

#[tokio::test]
async fn create_rejects_control_chars() {
    let (state, sid, csrf, _td) = authed_setup().await;
    let app = build_router(state.clone());
    let body = format!(
        "_csrf={csrf}&name=bad&template=oops%0Aboom",
        csrf = urlencoding::encode(&csrf),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/pings")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let mgr = state.ping_manager.read().await;
    assert!(
        mgr.get("bad").is_none(),
        "control-char ping must not persist"
    );
}

#[tokio::test]
async fn create_rejects_duplicate_name() {
    let (state, sid, csrf, _td) = authed_setup().await;
    {
        let mut mgr = state.ping_manager.write().await;
        mgr.create_ping("dup".into(), "x {mentions}".into(), "admin".into(), None)
            .unwrap();
    }
    let app = build_router(state.clone());
    // Case-insensitive dup check: try "DUP".
    let body = format!(
        "_csrf={csrf}&name=DUP&template=other%20{{mentions}}",
        csrf = urlencoding::encode(&csrf),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/pings")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let mgr = state.ping_manager.read().await;
    assert_eq!(mgr.iter().count(), 1, "no second ping should be created");
}

#[tokio::test]
async fn edit_round_trip() {
    let (state, sid, csrf, _td) = authed_setup().await;
    {
        let mut mgr = state.ping_manager.write().await;
        mgr.create_ping("team".into(), "Hey {mentions}".into(), "admin".into(), None)
            .unwrap();
    }

    // GET edit form — must contain the existing template.
    let app = build_router(state.clone());
    let req = Request::builder()
        .uri("/pings/team")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("Hey {mentions}"),
        "edit form should pre-fill template; got {html}"
    );

    // POST update with a new template.
    let app = build_router(state.clone());
    let body = format!(
        "_csrf={csrf}&template=Updated%20{{mentions}}",
        csrf = urlencoding::encode(&csrf),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/pings/team")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let mgr = state.ping_manager.read().await;
    assert_eq!(mgr.get("team").unwrap().template, "Updated {mentions}");
}

#[tokio::test]
async fn delete_via_htmx_header_succeeds() {
    let (state, sid, csrf, _td) = authed_setup().await;
    {
        let mut mgr = state.ping_manager.write().await;
        mgr.create_ping("doomed".into(), "{mentions}".into(), "admin".into(), None)
            .unwrap();
    }
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/pings/doomed/delete")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .header("X-Csrf-Token", csrf.clone())
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert_eq!(
        html, "",
        "delete returns empty body for HTMX outerHTML swap"
    );
    let mgr = state.ping_manager.read().await;
    assert!(mgr.get("doomed").is_none(), "delete must remove the ping");
}

#[tokio::test]
async fn delete_without_csrf_rejected() {
    let (state, sid, csrf, _td) = authed_setup().await;
    {
        let mut mgr = state.ping_manager.write().await;
        mgr.create_ping("survives".into(), "{mentions}".into(), "admin".into(), None)
            .unwrap();
    }
    let app = build_router(state.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/pings/survives/delete")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        // No X-Csrf-Token header.
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let mgr = state.ping_manager.read().await;
    assert!(
        mgr.get("survives").is_some(),
        "ping must persist when csrf header is absent",
    );
}

#[tokio::test]
async fn create_rejects_bad_form_csrf() {
    let (state, sid, csrf, _td) = authed_setup().await;
    let app = build_router(state.clone());
    let body = format!(
        "_csrf={bad}&name=tampered&template=hi",
        bad = urlencoding::encode(&"00".repeat(32)),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/pings")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let mgr = state.ping_manager.read().await;
    assert!(
        mgr.get("tampered").is_none(),
        "ping must not persist with mismatched _csrf",
    );
}
