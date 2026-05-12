use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

#[tokio::test]
async fn viewer_method_guard_admits_get() {
    let app = Router::new()
        .route("/x", axum::routing::any(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            twitch_1337_web::auth::viewer_method_guard,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/x")
                .method(Method::GET)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn viewer_method_guard_admits_head() {
    let app = Router::new()
        .route("/x", axum::routing::any(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            twitch_1337_web::auth::viewer_method_guard,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/x")
                .method(Method::HEAD)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn viewer_method_guard_rejects_post() {
    let app = Router::new()
        .route("/x", axum::routing::any(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            twitch_1337_web::auth::viewer_method_guard,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/x")
                .method(Method::POST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn viewer_method_guard_rejects_delete() {
    let app = Router::new()
        .route("/x", axum::routing::any(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            twitch_1337_web::auth::viewer_method_guard,
        ));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/x")
                .method(Method::DELETE)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}
