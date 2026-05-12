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
use twitch_1337_web::dev::{DEV_CSRF, DEV_SID, DEV_USER_ID, StubHelix, dev_new_session};
use twitch_1337_web::helix::HelixClient;
use twitch_1337_web::state::derive_session_key;
use twitch_1337_web::{WebState, bind, build_router, serve_app};

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

    // Pair with the fixed signed_key below: stable signed cookie across
    // restarts means no re-login when iterating.
    sessions.insert_with_id(DEV_SID, DEV_CSRF, dev_new_session());

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
        avatar_cache: Arc::new(twitch_1337_web::helix::AvatarCache::new(
            Duration::from_secs(3600),
        )),
    };

    let listener = bind(bind_addr).await?;
    let shutdown = Arc::new(Notify::new());
    let s = shutdown.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        s.notify_one();
    });

    let mut app = build_router(state);
    let livereload = tower_livereload::LiveReloadLayer::new();
    let reloader = livereload.reloader();
    app = app.layer(livereload);
    install_asset_watcher(reloader);

    serve_app(listener, app, shutdown).await
}

/// Watch `assets/` + `templates/` and ping the livereload SSE on
/// change. The debouncer owns its internal worker thread; we `Box::leak`
/// it because dropping the debouncer stops the watch — leaking is the
/// honest signal that this lives for the program lifetime.
fn install_asset_watcher(reloader: tower_livereload::Reloader) {
    use notify_debouncer_mini::{DebounceEventResult, new_debouncer, notify::RecursiveMode};

    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut debouncer = match new_debouncer(
        Duration::from_millis(200),
        move |res: DebounceEventResult| {
            if res.is_ok() {
                reloader.reload();
            }
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(target: "twitch_1337_web", error = ?e, "livereload watcher init failed");
            return;
        }
    };
    for sub in ["assets", "templates"] {
        let path = crate_dir.join(sub);
        if let Err(e) = debouncer.watcher().watch(&path, RecursiveMode::Recursive) {
            tracing::warn!(target: "twitch_1337_web", ?path, error = ?e, "livereload watch failed");
        }
    }
    Box::leak(Box::new(debouncer));
}
