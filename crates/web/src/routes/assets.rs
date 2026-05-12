//! Embedded static asset router (`/assets/*`).
//!
//! Bakes css/js straight into the binary so the FROM-scratch musl image
//! stays self-contained. Debug builds resolve from `CARGO_MANIFEST_DIR`
//! via rust_embed's debug fallback + a `no-store` cache header, so asset
//! edits show up on refresh without a rebuild.

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "assets/"]
struct Assets;

pub fn router() -> Router {
    Router::new().route("/assets/{*path}", get(serve))
}

#[cfg(debug_assertions)]
fn cache_control() -> &'static str {
    "no-store"
}

#[cfg(not(debug_assertions))]
fn cache_control() -> &'static str {
    "public, max-age=31536000, immutable"
}

async fn serve(Path(path): Path<String>) -> impl IntoResponse {
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CACHE_CONTROL, cache_control()),
                ],
                Body::from(content.data),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
