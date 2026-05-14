//! Embedded web dashboard for the twitch-1337 bot.
//!
//! Public surfaces:
//! - `/healthz` (always public)
//! - `/login`, `/auth/callback`, `/logout` (OAuth + post-login `?next=` deep-link)
//! - `/assets/*` (embedded htmx + pico bundles, immutable cache)
//!
//! Auth tiers:
//! - **Viewer** (allowlisted Twitch login): `/`, `/pings` (read), `/leaderboard`, `/flights`.
//!   Read-only — non-GET/HEAD methods are rejected by `viewer_method_guard`.
//! - **Mod** (broadcaster / hidden admins / helix moderators): pings mutations,
//!   `/memory/{soul,lore,users,state}` CRUD, stubs.

pub mod auth;
pub mod clock;
pub mod config;
#[cfg(feature = "dev-login")]
pub mod dev;
pub mod error;
pub mod flash;
pub mod helix;
pub mod nav;
pub mod routes;
pub mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use eyre::{Result, WrapErr as _};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower_cookies::CookieManagerLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

pub use crate::state::WebState;

pub struct WebDeps {
    pub bind_addr: SocketAddr,
    pub state: WebState,
}

/// Bind synchronously so a port-in-use failure aborts startup (loud) rather
/// than silently degrading the spawned task.
pub async fn bind(addr: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .wrap_err_with(|| format!("bind {addr}"))
}

pub async fn run_web(listener: TcpListener, deps: WebDeps, shutdown: Arc<Notify>) -> Result<()> {
    serve_app(listener, build_router(deps.state), shutdown).await
}

/// Serve a pre-built router with graceful shutdown.
pub async fn serve_app(listener: TcpListener, app: Router, shutdown: Arc<Notify>) -> Result<()> {
    let url = listener
        .local_addr()
        .map(|a| format!("http://{a}/"))
        .unwrap_or_else(|_| "<unknown>".to_owned());
    info!(
        target: "twitch_1337_web",
        version = routes::health::PKG_VERSION,
        sha = routes::health::GIT_SHA,
        "Build info",
    );
    info!(target: "twitch_1337_web", %url, "Web dashboard listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await
        .wrap_err("web serve")?;
    warn!(target: "twitch_1337_web", "Web dashboard stopped");
    Ok(())
}

async fn root_redirect(
    axum::extract::Extension(session): axum::extract::Extension<auth::session::Session>,
) -> axum::response::Redirect {
    match session.role {
        // Owner gets the same default landing as Mod — settings is one step
        // beyond, but the management surface is shared.
        auth::role::Role::Owner | auth::role::Role::Mod => axum::response::Redirect::to("/pings"),
        auth::role::Role::Viewer => axum::response::Redirect::to("/leaderboard"),
    }
}

pub fn build_router(state: WebState) -> Router {
    #[allow(unused_mut)]
    let mut public = Router::new()
        .merge(routes::health::router(state.irc_connected.clone()))
        .merge(routes::assets::router())
        .merge(auth::auth_router().with_state(state.clone()));
    #[cfg(feature = "dev-login")]
    {
        public = public.merge(dev::router(state.clone()));
    }

    let viewer_state = state.clone();
    let viewer = Router::new()
        .route("/", axum::routing::get(root_redirect))
        .merge(routes::pings::viewer_router())
        .merge(routes::leaderboard::router())
        .merge(routes::flights::viewer_router())
        .layer(axum::middleware::from_fn(auth::viewer_method_guard))
        .route_layer(axum::middleware::from_fn_with_state(
            viewer_state.clone(),
            |s, c, r, n| auth::require_role(auth::role::Role::Viewer, s, c, r, n),
        ))
        .with_state(viewer_state);

    let mod_state = state.clone();
    let mod_only = Router::new()
        .merge(routes::pings::mod_router())
        .merge(routes::flights::mod_router())
        .merge(routes::memory::router())
        .merge(routes::stubs::router())
        .route_layer(axum::middleware::from_fn_with_state(
            mod_state.clone(),
            auth::require_mod,
        ))
        .with_state(mod_state);

    let owner_state = state.clone();
    let owner_only = Router::new()
        .merge(routes::settings::owner_router())
        .route_layer(axum::middleware::from_fn_with_state(
            owner_state.clone(),
            auth::require_owner,
        ))
        .with_state(owner_state);

    public
        .merge(viewer)
        .merge(mod_only)
        .merge(owner_only)
        .layer(CookieManagerLayer::new())
        .layer(TraceLayer::new_for_http())
}
