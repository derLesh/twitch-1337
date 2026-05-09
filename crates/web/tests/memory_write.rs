//! Integration tests for the `/memory/*` write/create/delete routes (Task 6).
//!
//! Drives `build_router` directly via `tower::ServiceExt::oneshot`. Sessions
//! pre-seeded so require_mod admits without an OAuth round-trip; FakeHelix
//! returns `is_moderator = true` so the periodic recheck is satisfied.

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

fn post_form(uri: &str, sid: &str, csrf: &str, body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::COOKIE, cookie_header(sid, csrf))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn save_soul_written_when_mtime_matches() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let mtime = state
        .memory_store
        .current_mtime(&FileKind::Soul)
        .await
        .unwrap();
    let body = format!(
        "_csrf={csrf}&mtime={mtime}&body=fresh-soul-body",
        csrf = urlencoding::encode(&csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/soul", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let mf = state.memory_store.read_kind(&FileKind::Soul).await.unwrap();
    assert!(mf.body.contains("fresh-soul-body"));
}

#[tokio::test]
async fn save_soul_409_on_stale_mtime() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    // Bump SOUL.md so the stored mtime is non-zero, then submit a form
    // pretending the user opened the page when SOUL.md didn't exist (mtime=0).
    state
        .memory_store
        .write(&FileKind::Soul, "current-on-disk", None, None)
        .await
        .unwrap();
    let body = format!(
        "_csrf={csrf}&mtime=0&body=user-draft-body",
        csrf = urlencoding::encode(&csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/soul", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let html = body_string(res).await;
    assert!(
        html.contains("current-on-disk"),
        "conflict page must show current body; got {html}"
    );
    assert!(
        html.contains("user-draft-body"),
        "conflict page must preserve user draft; got {html}"
    );
    // Disk must NOT have been overwritten.
    let mf = state.memory_store.read_kind(&FileKind::Soul).await.unwrap();
    assert!(mf.body.contains("current-on-disk"));
    assert!(!mf.body.contains("user-draft-body"));
}

#[tokio::test]
async fn save_oversized_body_returns_400() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let mtime = state
        .memory_store
        .current_mtime(&FileKind::Soul)
        .await
        .unwrap();
    // soul_bytes default cap is 4096 bytes (full file incl. frontmatter).
    // 5000 bytes guarantees over-cap.
    let huge = "x".repeat(5000);
    let body = format!(
        "_csrf={csrf}&mtime={mtime}&body={huge}",
        csrf = urlencoding::encode(&csrf),
    );
    let app = build_router(state);
    let res = app
        .oneshot(post_form("/memory/soul", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_state_reserved_slug_rejected() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let body = format!(
        "_csrf={csrf}&slug=new&body=anything",
        csrf = urlencoding::encode(&csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/state", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // No file at memories/state/new.md must exist.
    let kind = FileKind::State { slug: "new".into() };
    // read_kind returns empty body for missing files; check via list.
    let listed = state.memory_store.list_state().await.unwrap();
    assert!(
        listed
            .iter()
            .all(|f| !matches!(&f.kind, FileKind::State { slug } if slug == "new")),
        "reserved slug must not have produced a file"
    );
    let _ = kind;
}

#[tokio::test]
async fn create_state_round_trip() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let body = format!(
        "_csrf={csrf}&slug=quiz&body=score%3A%201",
        csrf = urlencoding::encode(&csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/state", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/memory/state/quiz");
    let mf = state
        .memory_store
        .read_kind(&FileKind::State {
            slug: "quiz".into(),
        })
        .await
        .unwrap();
    assert!(mf.body.contains("score: 1"));
    assert_eq!(mf.frontmatter.created_by.as_deref(), Some("9001"));
}

#[tokio::test]
async fn delete_state_round_trip() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    state
        .memory_store
        .write_state(
            &FileKind::State {
                slug: "doomed".into(),
            },
            "x",
            Some("9001"),
        )
        .await
        .unwrap();
    let body = format!("_csrf={csrf}", csrf = urlencoding::encode(&csrf));
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/state/doomed/delete", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/memory/state");
    let listed = state.memory_store.list_state().await.unwrap();
    assert!(
        listed
            .iter()
            .all(|f| !matches!(&f.kind, FileKind::State { slug } if slug == "doomed")),
        "deleted slug must be absent from list"
    );
}

#[tokio::test]
async fn save_user_id_invalid_400() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let body = format!(
        "_csrf={csrf}&mtime=0&body=anything",
        csrf = urlencoding::encode(&csrf),
    );
    let app = build_router(state);
    let res = app
        .oneshot(post_form("/memory/users/abc", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn save_csrf_mismatch_rejected() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let mtime = state
        .memory_store
        .current_mtime(&FileKind::Soul)
        .await
        .unwrap();
    let bad = "00".repeat(32);
    let body = format!(
        "_csrf={bad}&mtime={mtime}&body=tampered",
        bad = urlencoding::encode(&bad),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/soul", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_state_invalid_slug_400() {
    let (state, sid, csrf, _tdp, _tdm) = authed_setup().await;
    let body = format!("_csrf={csrf}", csrf = urlencoding::encode(&csrf));
    let app = build_router(state);
    let res = app
        .oneshot(post_form(
            "/memory/state/..%2Fevil/delete",
            &sid,
            &csrf,
            body,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
