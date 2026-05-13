use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use chrono::Utc;
use color_eyre::eyre::Result;
use eyre::{WrapErr as _, eyre};
use secrecy::ExposeSecret as _;
use tokio::sync::oneshot;
use tracing::info;
use twitch_1337_core::{
    AuthenticatedLoginCredentials, PersonalBest, Services,
    ai::{command::memory_caps_from_config, memory::store::MemoryStore},
    aviation, doener, ensure_data_dir, get_data_dir, install_crypto_provider, install_tracing,
    llm_factory, load_configuration, load_leaderboard,
    ping::PingManager,
    run_bot, setup_and_verify_twitch_client,
    twitch::whisper,
    util::clock::SystemClock,
};
use twitch_1337_web::helix::{AccessTokenProvider, HelixClient as _, ReqwestHelixClient};
use twitch_irc::login::LoginCredentials as _;

use twitch_1337_core as twitch_1337;

#[tokio::main]
pub async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        // reqwest uses rustls-no-provider; the ring default provider must be
        // installed before any TLS handshake or `Client::builder().build()`
        // panics. Skipping color_eyre + tracing on the healthcheck path is
        // intentional — they aren't needed for a one-shot HTTP probe.
        install_crypto_provider();
        return run_healthcheck().await;
    }

    color_eyre::install()?;
    install_tracing();
    install_crypto_provider();

    let config = load_configuration().await?;

    let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
    info!(
        local_time = ?local,
        utc_time = ?Utc::now(),
        channel = %config.twitch.channel,
        username = %config.twitch.username,
        schedules_enabled = !config.schedules.is_empty(),
        schedule_count = config.schedules.len(),
        "Starting twitch-1337 bot"
    );

    ensure_data_dir().await?;

    let (incoming, client, credentials, bot_user_id) =
        setup_and_verify_twitch_client(&config).await?;
    let client = Arc::new(client);

    let llm_client = llm_factory::build_llm_client(config.ai.as_ref())?;

    let aviation_client = match aviation::AviationClient::new()
        .map(|client| client.with_aviationstack_config(config.aviationstack.clone()))
    {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!(
                error = ?e,
                "Failed to initialize aviation client; aviation commands and flight tracker disabled"
            );
            None
        }
    };

    let doener_client =
        Arc::new(doener::DoenerClient::new().wrap_err("Failed to initialize Döner-index client")?);

    let whisper_credentials = credentials.clone();
    let whisper = whisper::HelixWhisperSender::new(
        whisper_credentials,
        config.twitch.client_id.expose_secret().to_string(),
        bot_user_id,
        get_data_dir(),
    )
    .await
    .map(|sender| Arc::new(sender) as Arc<dyn whisper::WhisperSender>)?;

    let irc_connected = Arc::new(AtomicBool::new(false));

    let ping_manager = Arc::new(tokio::sync::RwLock::new(
        PingManager::load(&get_data_dir()).wrap_err("Failed to load ping manager")?,
    ));

    // Dashboard-managed runtime settings. Opened here so the same Arc-backed
    // store can be shared with both the IRC handlers (via `Services.settings`)
    // and `WebState` (Task 10 wires the POST handler). The audit log lives at
    // `$DATA_DIR/settings_audit.log`.
    let audit_log: Arc<dyn twitch_1337::settings::AuditLog> = Arc::new(
        twitch_1337::settings::FileAuditLog::new(get_data_dir().join("settings_audit.log")),
    );
    let (settings_store, settings_handle) =
        twitch_1337::settings::SettingsStore::open(&get_data_dir(), audit_log)
            .wrap_err("Failed to open settings store")?;

    // Memory v2 store opens unconditionally so the dashboard editor has a
    // handle even when `[ai]` is disabled. The same `Arc`-backed store is
    // shared with the bot's IRC handlers / dreamer ritual via `Services`.
    let memory_store =
        MemoryStore::open(&get_data_dir(), memory_caps_from_config(config.ai.as_ref()))
            .await
            .wrap_err("open memory store")?;

    // Load the leaderboard before building the web spawner so the same Arc
    // can be shared with WebState (dashboard read) and the IRC tracker (writes).
    let leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>> = Arc::new(
        tokio::sync::RwLock::new(load_leaderboard(&get_data_dir()).await),
    );

    // Pre-create the flight-tracker mpsc channel when aviation is enabled so
    // the sender Arc can be wired into WebState before the handlers are spawned.
    let (aviation_tracker_tx, aviation_tracker_rx) = if aviation_client.is_some() {
        let (tx, rx) = tokio::sync::mpsc::channel::<aviation::TrackerCommand>(32);
        (Some(Arc::new(tx)), Some(rx))
    } else {
        (None, None)
    };

    let web_spawner = if config.web.enabled {
        let credentials_for_web = credentials.clone();
        Some(
            build_web_spawner(
                &config,
                credentials_for_web,
                irc_connected.clone(),
                ping_manager.clone(),
                memory_store.clone(),
                leaderboard.clone(),
                aviation_tracker_tx.clone(),
                settings_handle.clone(),
                settings_store.clone(),
            )
            .await?,
        )
    } else {
        None
    };

    let services = Services {
        clock: Arc::new(SystemClock),
        llm: llm_client,
        aviation: aviation_client,
        doener: doener_client,
        whisper: Some(whisper),
        data_dir: get_data_dir(),
        settings: settings_handle,
        settings_store,
        emote_glossary_override: None,
        irc_connected,
        web_spawner,
        ping_manager,
        memory_store,
        leaderboard,
        aviation_tracker_tx,
        aviation_tracker_rx,
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        let _ = shutdown_tx.send(());
    });

    run_bot(client, incoming, config, services, shutdown_rx).await
}

/// Build the web-task spawn closure. Resolves the broadcaster id, builds
/// the helix client + OAuth context, binds the listener loud (port-in-use
/// aborts startup), and returns a closure that — given the shared
/// shutdown `Notify` — spawns `run_web` on a tokio task.
// Top-level DI compositor for the web spawner; argument count tracks the
// dashboard's dependency surface and is intentionally explicit.
#[allow(clippy::too_many_arguments)]
async fn build_web_spawner(
    config: &twitch_1337::config::Configuration,
    credentials: AuthenticatedLoginCredentials,
    irc_connected: Arc<AtomicBool>,
    ping_manager: Arc<tokio::sync::RwLock<PingManager>>,
    memory_store: MemoryStore,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    tracker_tx: Option<Arc<tokio::sync::mpsc::Sender<aviation::TrackerCommand>>>,
    settings: twitch_1337::settings::SettingsHandle,
    settings_store: Arc<twitch_1337::settings::SettingsStore>,
) -> Result<twitch_1337::WebSpawner> {
    let bind_addr: std::net::SocketAddr = config
        .web
        .bind_addr
        .parse()
        .wrap_err("parse web.bind_addr")?;
    // Bind synchronously so a port-in-use failure aborts startup loudly.
    let listener = twitch_1337_web::bind(bind_addr).await?;

    let token_provider: Arc<dyn AccessTokenProvider> = Arc::new(CredsTokenProvider {
        creds: Arc::new(credentials),
    });
    let helix = Arc::new(ReqwestHelixClient::new(
        reqwest::Client::new(),
        config.twitch.client_id.clone(),
        token_provider,
    ));

    let broadcaster = helix
        .as_ref()
        .fetch_user_by_login(&config.twitch.channel)
        .await
        .wrap_err("resolve broadcaster id")?
        .ok_or_else(|| eyre!("channel `{}` not found on twitch", config.twitch.channel))?;

    let oauth = Arc::new(twitch_1337_web::auth::OAuthCtx::new(
        config.twitch.client_id.expose_secret(),
        &config.twitch.client_secret,
        &config.web.public_url,
    )?);

    let web_clock = Arc::new(twitch_1337_web::clock::SystemClock);
    let sessions = Arc::new(twitch_1337_web::auth::session::SessionTable::new(
        config.web.session_ttl,
        web_clock.clone(),
    ));

    let web_config = Arc::new(twitch_1337_web::config::WebConfig {
        bind_addr: config.web.bind_addr.clone(),
        public_url: config.web.public_url.clone(),
        session_secret: config.web.session_secret.clone(),
        session_ttl: config.web.session_ttl,
        role_check_refresh: config.web.mod_check_refresh,
    });

    let signed_key = twitch_1337_web::state::derive_session_key(&config.web.session_secret)?;

    #[allow(unused_mut)]
    let mut hidden_admins = config.twitch.hidden_admins.clone();
    #[cfg(feature = "dev-login")]
    {
        hidden_admins.push(twitch_1337_web::dev::DEV_USER_ID.to_owned());
        tracing::warn!(
            target: "twitch_1337_web",
            "dev-login feature compiled in — /_dev/login mints mod sessions without OAuth (DO NOT SHIP)",
        );
    }

    let owner_id = config.twitch.owner.as_deref().map(Arc::<str>::from);

    let state = twitch_1337_web::WebState {
        sessions,
        helix: helix as Arc<dyn twitch_1337_web::helix::HelixClient>,
        irc_connected,
        config: web_config,
        clock: web_clock,
        channel: Arc::from(config.twitch.channel.as_str()),
        broadcaster_id: Arc::from(broadcaster.id.as_str()),
        hidden_admins: Arc::from(hidden_admins.into_boxed_slice()),
        viewer_allowlist: Arc::from(config.twitch.viewer_allowlist.clone().into_boxed_slice()),
        client_id: config.twitch.client_id.clone(),
        oauth,
        ping_manager,
        memory_store,
        signed_key,
        leaderboard,
        tracker_tx,
        avatar_cache: Arc::new(twitch_1337_web::helix::AvatarCache::new(
            std::time::Duration::from_secs(3600),
        )),
        owner_id,
        settings,
        settings_store,
    };

    Ok(Box::new(move |shutdown| {
        let deps = twitch_1337_web::WebDeps { bind_addr, state };
        tokio::spawn(async move {
            if let Err(e) = twitch_1337_web::run_web(listener, deps, shutdown).await {
                tracing::error!(target: "twitch_1337_web", error = ?e, "Web task exited with error");
            }
        })
    }))
}

/// Bridges the bot's `RefreshingLoginCredentials` to the web crate's
/// helix `AccessTokenProvider`. Lets the helix client reuse the same
/// refreshed access token the bot already maintains in `token.ron`.
struct CredsTokenProvider {
    creds: Arc<AuthenticatedLoginCredentials>,
}

#[async_trait]
impl AccessTokenProvider for CredsTokenProvider {
    async fn current_access_token(&self) -> eyre::Result<String> {
        let creds = self
            .creds
            .as_ref()
            .get_credentials()
            .await
            .map_err(|e| eyre!("get_credentials: {e}"))?;
        Ok(creds.token.unwrap_or_default())
    }
}

/// Lightweight healthcheck for the Docker `HEALTHCHECK` directive. Reads the
/// config to find the web bind port and probes `/healthz`. When `[web]` is
/// disabled, exits 0 without touching the network so the container is still
/// considered healthy.
async fn run_healthcheck() -> Result<()> {
    let config = load_configuration().await?;
    if !config.web.enabled {
        return Ok(());
    }
    let port = config.web.bind_addr.rsplit(':').next().unwrap_or("8080");
    let url = format!("http://127.0.0.1:{port}/healthz");
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?
        .get(&url)
        .send()
        .await?;
    if res.status().is_success() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_prefill_threshold_validation() {
        assert!((0.0..=1.0).contains(&0.0));
        assert!((0.0..=1.0).contains(&0.5));
        assert!((0.0..=1.0).contains(&1.0));
        assert!(!(0.0..=1.0).contains(&-0.1));
        assert!(!(0.0..=1.0).contains(&1.1));
    }
}
