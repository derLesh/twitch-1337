//! Sidebar always renders on authed pages and highlights the current page.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::build_router;
use twitch_1337_web::helix::{HelixClient, HelixUser};

mod helpers;
use helpers::{
    FakeHelix, build_state_with_dirs, cookie_header, insert_session, insert_session_as,
    install_crypto,
};

/// FakeHelix flavored to admit the seeded test user during periodic mod
/// rechecks, mirroring `admin_helix` in the other route tests.
fn fake_helix_with_mod(user_id: &str, login: &str) -> Arc<dyn HelixClient> {
    let mut users = HashMap::new();
    users.insert(
        user_id.to_owned(),
        HelixUser {
            id: user_id.to_owned(),
            login: login.to_owned(),
            display_name: login.to_owned(),
            profile_image_url: None,
        },
    );
    Arc::new(FakeHelix {
        moderators: vec![user_id.to_owned()],
        users,
    })
}

async fn fetch_authed(uri: &str) -> String {
    install_crypto();
    let helix = fake_helix_with_mod("12345", "alice");
    let (state, _td_pings, _td_memory) = build_state_with_dirs(helix).await;
    let (sid, csrf, _bare) = insert_session(&state, "12345", "alice");
    let app = build_router(state);
    let req = Request::builder()
        .uri(uri)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{uri}: status");
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn pings_page_renders_sidebar_with_active_pings() {
    let body = fetch_authed("/pings").await;
    assert!(body.contains("twitch-1337"));
    assert!(body.contains("alice"), "sidebar must show user_login");
    assert!(
        body.contains("class=\"active\""),
        "current-page highlight missing"
    );
    // Crude: the active class must be on a li that links to /pings.
    let snippet = body
        .split("class=\"active\"")
        .nth(1)
        .expect("active class present");
    assert!(
        snippet.contains("/pings"),
        "active li should be the pings entry"
    );
}

#[tokio::test]
async fn memory_state_page_highlights_state() {
    let body = fetch_authed("/memory/state").await;
    let snippet = body
        .split("class=\"active\"")
        .nth(1)
        .expect("active class present");
    assert!(snippet.contains("/memory/state"));
}

#[tokio::test]
async fn sidebar_hides_memory_and_system_for_viewer() {
    install_crypto();
    let helix = Arc::new(FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    });
    let (state, _td_pings, _td_mem) = build_state_with_dirs(helix).await;
    let (sid, csrf, _bare) =
        insert_session_as(&state, "42", "alice", twitch_1337_web::auth::Role::Viewer);
    let app = build_router(state);
    let req = Request::builder()
        .uri("/leaderboard")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "/leaderboard: status");
    let bytes = to_bytes(res.into_body(), 128 * 1024).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        body.contains("Leaderboard"),
        "leaderboard nav entry present"
    );
    assert!(body.contains("Flights"), "flights nav entry present");
    assert!(
        !body.contains("/memory/soul"),
        "memory group hidden for viewer"
    );
    assert!(!body.contains("/logs"), "logs hidden for viewer");
    assert!(!body.contains("/schedules"), "schedules hidden for viewer");
    assert!(
        body.contains("class=\"me-role\"></span>"),
        "viewer footer renders empty role chip"
    );
}
