use std::sync::{Arc, atomic::AtomicBool};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use twitch_1337_web::routes::health::router;

#[tokio::test]
async fn healthz_returns_200_when_irc_connected() {
    let flag = Arc::new(AtomicBool::new(true));
    let app = router(flag);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthz_returns_503_when_irc_disconnected() {
    let flag = Arc::new(AtomicBool::new(false));
    let app = router(flag);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}
