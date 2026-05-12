//! Integration tests for `/flights` read-only route.
//!
//! The route is not yet mounted in `build_router` (Task 14 handles that),
//! so we assemble a minimal `Router<WebState>` locally that merges
//! `routes::flights::router()` behind a `require_mod` layer. This keeps
//! the test self-contained and independent of Task 14.

mod helpers;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use helpers::{FakeHelix, build_state_with_dirs, cookie_header, insert_session, install_crypto};
use tokio::sync::mpsc;
use tower::ServiceExt as _;
use twitch_1337_core::aviation::tracker::{TrackedFlightView, TrackerCommand};
use twitch_1337_web::auth::require_mod;
use twitch_1337_web::routes::flights;

fn mod_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec!["42".into()],
        users: Default::default(),
    })
}

fn app(state: twitch_1337_web::WebState) -> Router {
    Router::new()
        .merge(flights::viewer_router())
        .merge(flights::mod_router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_mod,
        ))
        .with_state(state)
        .layer(tower_cookies::CookieManagerLayer::new())
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 128 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn flights_empty_state_when_no_tracker() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    // tracker_tx is None by default in test helpers
    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/flights")
        .method(Method::GET)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("disabled"),
        "should show aviation-disabled placeholder; got {html}"
    );
}

#[tokio::test]
async fn flights_renders_snapshot_from_tracker() {
    install_crypto();
    let (mut state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    let (tx, mut rx) = mpsc::channel::<TrackerCommand>(8);
    state.tracker_tx = Some(Arc::new(tx));

    // Spawn a tiny task that answers the next Snapshot command.
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if let TrackerCommand::Snapshot { reply } = cmd {
                let _ = reply.send(vec![TrackedFlightView {
                    identifier: "DLH123".into(),
                    callsign: Some("DLH123".into()),
                    owner_login: "alice".into(),
                    phase: "Cruise".into(),
                    altitude_ft: Some(34000),
                    ground_speed_kts: Some(450.0),
                    last_seen_secs_ago: Some(12),
                }]);
                break;
            }
        }
    });

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/flights")
        .method(Method::GET)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("DLH123"),
        "should render callsign; got {html}"
    );
    assert!(
        html.contains("alice"),
        "should render owner login; got {html}"
    );
}

#[tokio::test]
async fn flights_unauthenticated_redirects() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;

    let req = Request::builder()
        .uri("/flights")
        .body(Body::empty())
        .unwrap();

    let res = app(state).oneshot(req).await.unwrap();
    assert!(
        res.status() == StatusCode::SEE_OTHER || res.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated request must not get 200; got {}",
        res.status()
    );
}
