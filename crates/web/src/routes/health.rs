use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

pub fn router(irc_connected: Arc<AtomicBool>) -> Router {
    Router::new().route(
        "/healthz",
        get(move || {
            let flag = irc_connected.clone();
            async move {
                if flag.load(Ordering::Relaxed) {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }
        }),
    )
}
