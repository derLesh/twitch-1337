//! Embedded static asset router (`/assets/*`).
//!
//! Bakes css/js straight into the binary so the FROM-scratch musl image
//! stays self-contained.

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

async fn serve(Path(path): Path<String>) -> impl IntoResponse {
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    // Embedded assets ship with the binary, so a deploy is
                    // the only thing that can change them — `immutable` is
                    // safe and saves repeat downloads on every page load.
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                Body::from(content.data),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
