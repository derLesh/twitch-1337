//! Embedded web dashboard for the twitch-1337 bot.
//!
//! Public surfaces:
//! - `/healthz` (always public)
//! - `/login`, `/auth/callback`, `/logout` (OAuth + post-login `?next=` deep-link)
//! - `/assets/*` (embedded htmx + pico bundles, immutable cache)
//!
//! Mod-gated surfaces (sliding helix re-check via `require_mod`):
//! - `/pings` — ping CRUD against the bot's `PingManager`
//! - `/memory/{soul,lore,users,state}` — AI memory browse + edit with
//!   `MemoryStore::write_with_guard` for mtime-aware conflict UX

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
    let local_addr = listener.local_addr().ok();
    let app = build_router(deps.state);
    info!(target: "twitch_1337_web", ?local_addr, "Web dashboard listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.notified().await })
        .await
        .wrap_err("web serve")?;
    warn!(target: "twitch_1337_web", "Web dashboard stopped");
    Ok(())
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

    let authed = Router::new()
        .route(
            "/",
            axum::routing::get(|| async { axum::response::Redirect::to("/pings") }),
        )
        .merge(routes::pings::router())
        .merge(routes::memory::router())
        .merge(routes::stubs::router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_mod,
        ))
        .with_state(state);

    public
        .merge(authed)
        .layer(CookieManagerLayer::new())
        .layer(TraceLayer::new_for_http())
}
