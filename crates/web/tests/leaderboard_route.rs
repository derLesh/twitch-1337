//! Integration tests for `/leaderboard` read-only route.
//!
//! The route is not yet mounted in `build_router` (Task 14 handles that),
//! so we assemble a minimal `Router<WebState>` locally that merges
//! `routes::leaderboard::router()` behind a `require_mod` layer. This keeps
//! the test self-contained and independent of Task 14.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::NaiveDate;
use tower::ServiceExt as _;
use twitch_1337_core::commands::leaderboard::PersonalBest;
use twitch_1337_web::WebState;
use twitch_1337_web::auth::require_mod;
use twitch_1337_web::routes::leaderboard;

mod helpers;
use helpers::{FakeHelix, build_state_with_dirs, cookie_header, insert_session, install_crypto};

fn helix() -> Arc<FakeHelix> {
    let mut users = HashMap::new();
    users.insert(
        "42".to_owned(),
        twitch_1337_web::helix::HelixUser {
            id: "42".into(),
            login: "admin".into(),
            display_name: "admin".into(),
        },
    );
    Arc::new(FakeHelix {
        moderators: vec!["42".into()],
        users,
    })
}

fn app(state: WebState) -> Router {
    Router::new()
        .merge(leaderboard::router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_mod,
        ))
        .with_state(state)
        .layer(tower_cookies::CookieManagerLayer::new())
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn leaderboard_renders_seeded_pb() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(helix()).await;

    // Seed one entry into the shared leaderboard.
    {
        let mut lb = state.leaderboard.write().await;
        lb.insert(
            "alice".into(),
            PersonalBest {
                ms: 123,
                date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            },
        );
    }

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/leaderboard")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();

    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(html.contains("alice"), "row must contain login; got {html}");
    assert!(html.contains("123"), "row must contain ms; got {html}");
}

#[tokio::test]
async fn leaderboard_empty_state_renders_without_rows() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(helix()).await;

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/leaderboard")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();

    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("No personal bests yet"),
        "empty state must render; got {html}"
    );
}

#[tokio::test]
async fn leaderboard_unauthenticated_redirects() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(helix()).await;

    let req = Request::builder()
        .uri("/leaderboard")
        .body(Body::empty())
        .unwrap();

    let res = app(state).oneshot(req).await.unwrap();
    // require_mod returns WebError::Unauthenticated → 401/redirect
    assert!(
        res.status() == StatusCode::SEE_OTHER || res.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated request must not get 200; got {}",
        res.status()
    );
}

#[tokio::test]
async fn leaderboard_rows_sorted_by_ms_asc() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(helix()).await;

    {
        let mut lb = state.leaderboard.write().await;
        lb.insert(
            "bob".into(),
            PersonalBest {
                ms: 500,
                date: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
            },
        );
        lb.insert(
            "alice".into(),
            PersonalBest {
                ms: 100,
                date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            },
        );
    }

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/leaderboard")
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();

    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;

    // alice (100 ms) must appear before bob (500 ms).
    let alice_pos = html.find("alice").expect("alice must appear");
    let bob_pos = html.find("bob").expect("bob must appear");
    assert!(
        alice_pos < bob_pos,
        "alice (100 ms) must rank above bob (500 ms); got {html}"
    );
}
