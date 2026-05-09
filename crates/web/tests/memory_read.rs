//! Integration tests for the read-only `/memory/*` viewer routes (Task 5).
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
use tempfile::TempDir;
use tower::ServiceExt as _;
use twitch_1337_core::ai::memory::types::FileKind;
use twitch_1337_web::WebState;
use twitch_1337_web::build_router;
use twitch_1337_web::helix::{HelixClient, HelixUser};

mod helpers;
use helpers::{FakeHelix, build_state_with_dirs, cookie_header, insert_session, install_crypto};

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

/// Build (state, sid, csrf, _td_pings, _td_memory). The memory tempdir keeps
/// the on-disk SOUL/LORE seeds + any test-written files alive while the
/// store reads them; dropping it would invalidate paths the route handlers
/// still reach.
async fn authed_setup() -> (WebState, String, String, TempDir, TempDir) {
    install_crypto();
    let user_id = "9001";
    let helix = admin_helix(user_id);
    let (state, td_pings, td_memory) = build_state_with_dirs(helix).await;
    let (sid, csrf) = insert_session(&state, user_id, "admin");
    (state, sid, csrf, td_pings, td_memory)
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn get(uri: &str, sid: &str, csrf: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, cookie_header(sid, csrf))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn tree_renders_section_counts() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    state
        .memory_store
        .write(
            &FileKind::User {
                user_id: "1".into(),
            },
            "user body",
            Some("alice"),
            Some("Alice"),
        )
        .await
        .unwrap();
    state
        .memory_store
        .write_state(
            &FileKind::State {
                slug: "quiz".into(),
            },
            "state body",
            Some("9001"),
        )
        .await
        .unwrap();

    let app = build_router(state);
    let res = app.oneshot(get("/memory", &sid, &csrf)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("users (1)"),
        "expected users count; got {html}"
    );
    assert!(
        html.contains("state (1)"),
        "expected state count; got {html}"
    );
}

#[tokio::test]
async fn view_soul_renders_body() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    // Overwrite the seeded SOUL.md with a known marker so the test isn't
    // coupled to seed_soul.md content.
    state
        .memory_store
        .write(&FileKind::Soul, "MARKER-SOUL-BODY", None, None)
        .await
        .unwrap();

    let app = build_router(state);
    let res = app.oneshot(get("/memory/soul", &sid, &csrf)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("MARKER-SOUL-BODY"),
        "soul body must render; got {html}"
    );
    assert!(html.contains("name=\"mtime\""), "form needs mtime token");
}

#[tokio::test]
async fn view_user_with_invalid_id_400() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let app = build_router(state);
    // Path traversal attempt — must not reach the store.
    let res = app
        .oneshot(get("/memory/users/..%2Fevil", &sid, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn view_user_with_alpha_id_400() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let app = build_router(state);
    let res = app
        .oneshot(get("/memory/users/abc", &sid, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn view_state_with_traversal_slug_400() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let app = build_router(state);
    let res = app
        .oneshot(get("/memory/state/..", &sid, &csrf))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "`..` slug must not reach the store",
    );
}

#[tokio::test]
async fn view_state_new_resolves_to_create_form() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let app = build_router(state);
    let res = app
        .oneshot(get("/memory/state/new", &sid, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    // The literal `/memory/state/new` route must win over the dynamic
    // `/memory/state/{slug}` capture. The new-form title is unique.
    assert!(
        html.contains("new state note"),
        "expected new-state form, got {html}"
    );
    // Guard against accidentally rendering the {slug=new} viewer instead:
    assert!(
        !html.contains("State / new"),
        "literal route lost to dynamic capture; got {html}"
    );
}

#[tokio::test]
async fn state_list_renders_existing() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    for slug in ["alpha", "bravo"] {
        state
            .memory_store
            .write_state(&FileKind::State { slug: slug.into() }, "x", Some("9001"))
            .await
            .unwrap();
    }
    let app = build_router(state);
    let res = app
        .oneshot(get("/memory/state", &sid, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(html.contains("alpha"), "expected slug alpha; got {html}");
    assert!(html.contains("bravo"), "expected slug bravo; got {html}");
    assert!(
        html.contains("State notes (2)"),
        "expected count; got {html}"
    );
}

#[tokio::test]
async fn view_user_round_trip() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    state
        .memory_store
        .write(
            &FileKind::User {
                user_id: "12345".into(),
            },
            "user-marker-body",
            Some("alice"),
            Some("Alice"),
        )
        .await
        .unwrap();
    let app = build_router(state);
    let res = app
        .oneshot(get("/memory/users/12345", &sid, &csrf))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("user-marker-body"),
        "viewer must render body; got {html}"
    );
}
