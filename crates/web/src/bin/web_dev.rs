//! Standalone dashboard runner — `dev-login` only.
//!
//! Spins up the web crate against a shared dev-data dir without going
//! near IRC, the OAuth callback, or a real Helix client. Lets a parallel
//! dashboard run from a worktree on a non-conflicting port (default
//! 8761) while the full bot keeps running on 8760 from the main
//! checkout.
//!
//! Env knobs:
//!   - `BIND_ADDR`  default `127.0.0.1:8761`
//!   - `DATA_DIR`   default `./dev-data`
//!   - `CHANNEL`    default `devchannel`  (cosmetic, breadcrumb only)
//!
//! Caveat: `PingManager` and `MemoryStore` use atomic tmp+rename to
//! persist, so two processes against the same dir will not corrupt each
//! other's writes — but a last-writer-wins race is real. Don't run this
//! against the same dev-data the production bot is actively writing to
//! unless you're aware of that.

use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use eyre::{Result, WrapErr as _};
use secrecy::SecretString;
use tokio::sync::{Notify, RwLock};
use twitch_1337_core::ai::memory::store::MemoryStore;
use twitch_1337_core::ai::memory::types::Caps;
use twitch_1337_core::ping::PingManager;
use twitch_1337_core::{install_crypto_provider, install_tracing};
use twitch_1337_web::auth::OAuthCtx;
use twitch_1337_web::auth::session::SessionTable;
use twitch_1337_web::clock::SystemClock;
use twitch_1337_web::config::WebConfig;
use twitch_1337_web::dev::{DEV_USER_ID, StubHelix};
use twitch_1337_web::helix::HelixClient;
use twitch_1337_web::state::derive_session_key;
use twitch_1337_web::{WebDeps, WebState, bind, run_web};

#[tokio::main]
async fn main() -> Result<()> {
    install_tracing();
    install_crypto_provider();

    let bind_addr: SocketAddr = env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8761".to_owned())
        .parse()
        .wrap_err("parse BIND_ADDR")?;
    let data_dir: PathBuf = env::var_os("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./dev-data"));
    let channel = env::var("CHANNEL").unwrap_or_else(|_| "devchannel".to_owned());
    let public_url = format!("http://{bind_addr}");

    tracing::warn!(
        target: "twitch_1337_web",
        ?bind_addr, ?data_dir, channel = %channel,
        "web-dev starting (StubHelix, /_dev/login, no IRC) — DO NOT EXPOSE",
    );

    let pings = PingManager::load(&data_dir).wrap_err("load ping manager")?;
    let memory_store = MemoryStore::open(&data_dir, Caps::default())
        .await
        .wrap_err("open memory store")?;

    let clock = Arc::new(SystemClock);
    let session_ttl = Duration::from_secs(7 * 24 * 3600);
    let sessions = Arc::new(SessionTable::new(session_ttl, clock.clone()));

    let session_secret = SecretString::new("0".repeat(64).into());
    let signed_key = derive_session_key(&session_secret)?;

    let oauth = Arc::new(OAuthCtx::new(
        "dev-client-id",
        &SecretString::new("dev-secret".to_owned().into()),
        &public_url,
    )?);

    let web_config = Arc::new(WebConfig {
        bind_addr: bind_addr.to_string(),
        public_url,
        session_secret,
        session_ttl,
        role_check_refresh: Duration::from_secs(300),
    });

    let helix: Arc<dyn HelixClient> = Arc::new(StubHelix);

    let state = WebState {
        sessions,
        helix,
        irc_connected: Arc::new(AtomicBool::new(true)),
        config: web_config,
        clock,
        channel: Arc::from(channel.as_str()),
        broadcaster_id: Arc::from("0"),
        hidden_admins: Arc::from(vec![DEV_USER_ID.to_owned()].into_boxed_slice()),
        viewer_allowlist: Arc::from(Vec::<String>::new().into_boxed_slice()),
        client_id: SecretString::new("dev-client-id".to_owned().into()),
        oauth,
        ping_manager: Arc::new(RwLock::new(pings)),
        memory_store,
        signed_key,
        leaderboard: Arc::new(RwLock::new(HashMap::new())),
        tracker_tx: None,
    };

    let listener = bind(bind_addr).await?;
    let shutdown = Arc::new(Notify::new());
    let s = shutdown.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        s.notify_one();
    });
    run_web(listener, WebDeps { bind_addr, state }, shutdown).await
}
