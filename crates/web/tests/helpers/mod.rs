//! Shared fixtures for route-level integration tests in `crates/web/tests/`.
//!
//! Each test binary in `crates/web/tests/*.rs` includes this module via
//! `mod helpers;`, so cargo treats it as a separate compilation unit per
//! binary and dead-code warnings fire for any helper a given binary
//! doesn't use. The `#![allow(dead_code)]` here is the cheapest fix.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use tempfile::TempDir;
use tokio::sync::RwLock;
use twitch_1337_core::ai::memory::store::MemoryStore;
use twitch_1337_core::ai::memory::types::Caps;
use twitch_1337_core::ping::PingManager;
use twitch_1337_web::WebState;
use twitch_1337_web::auth::OAuthCtx;
use twitch_1337_web::auth::session::SessionTable;
use twitch_1337_web::clock::Clock;
use twitch_1337_web::config::WebConfig;
use twitch_1337_web::helix::{HelixClient, HelixUser};

pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

pub struct FakeHelix {
    pub moderators: Vec<String>,
    pub users: HashMap<String, HelixUser>,
}

#[async_trait]
impl HelixClient for FakeHelix {
    async fn fetch_user_by_id(&self, id: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(self.users.get(id).cloned())
    }
    async fn fetch_user_by_login(&self, login: &str) -> eyre::Result<Option<HelixUser>> {
        Ok(self.users.values().find(|u| u.login == login).cloned())
    }
    async fn is_moderator(&self, _broadcaster: &str, user_id: &str) -> eyre::Result<bool> {
        Ok(self.moderators.iter().any(|m| m == user_id))
    }
}

pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub async fn build_state(helix: Arc<dyn HelixClient>) -> WebState {
    let (state, _td_pings, _td_memory) = build_state_with_dirs(helix).await;
    // Test data dirs leak intentionally via TempDir's Drop running here. For
    // tests that just need WebState (without persistent mutations), we accept
    // that the underlying tempdirs are removed. The ping_manager / memory
    // store keep their in-memory state, so this only affects atomic
    // save+rename targets, which existing auth tests do not exercise.
    state
}

/// Variant that returns the ping data dir so callers can keep it alive while
/// exercising persistent ping CRUD paths. Memory data dir is dropped — use
/// [`build_state_with_dirs`] when both are needed.
pub async fn build_state_with_ping_dir(helix: Arc<dyn HelixClient>) -> (WebState, TempDir) {
    let (state, td_pings, _td_memory) = build_state_with_dirs(helix).await;
    (state, td_pings)
}

/// Variant that returns both the ping and memory tempdirs so callers can
/// keep them alive while exercising persistent CRUD paths.
pub async fn build_state_with_dirs(helix: Arc<dyn HelixClient>) -> (WebState, TempDir, TempDir) {
    let pings_dir = TempDir::new().expect("pings tempdir");
    let memory_dir = TempDir::new().expect("memory tempdir");
    let pings = PingManager::load(pings_dir.path()).expect("load empty ping manager");
    let ping_manager = Arc::new(RwLock::new(pings));
    let memory_store = MemoryStore::open(memory_dir.path(), Caps::default())
        .await
        .expect("open memory store");
    let clock = Arc::new(FixedClock(
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let sessions = Arc::new(SessionTable::new(Duration::from_secs(7200), clock.clone()));
    let oauth = Arc::new(
        OAuthCtx::new(
            "test-client-id",
            &SecretString::new("test-secret".to_owned().into()),
            "https://test.invalid",
        )
        .expect("test oauth"),
    );
    let config = Arc::new(WebConfig {
        bind_addr: "127.0.0.1:0".into(),
        public_url: "https://test.invalid".into(),
        session_secret: SecretString::new("0".repeat(64).into()),
        session_ttl: Duration::from_secs(7200),
        mod_check_refresh: Duration::from_secs(300),
    });
    let state = WebState {
        sessions,
        helix,
        irc_connected: Arc::new(AtomicBool::new(true)),
        config,
        clock,
        channel: Arc::from("testchannel"),
        broadcaster_id: Arc::from("100"),
        hidden_admins: Arc::from(Vec::<String>::new().into_boxed_slice()),
        client_id: SecretString::new("test-client-id".to_owned().into()),
        oauth,
        ping_manager,
        memory_store,
    };
    (state, pings_dir, memory_dir)
}

/// Insert a session for `(user_id, user_login)` and return the session id
/// alongside the hex-encoded csrf value the cookie + form fields would
/// carry. Used by ping/memory route tests to skip the OAuth round-trip.
pub fn insert_session(state: &WebState, user_id: &str, user_login: &str) -> (String, String) {
    let (sid, csrf) = state
        .sessions
        .insert(user_id.to_owned(), user_login.to_owned())
        .expect("insert session");
    (sid, hex::encode(csrf))
}

/// `Cookie:` header value combining sid + csrf, matching what the browser
/// would send after a successful login (sid is HttpOnly; csrf is JS-readable
/// but for header CSRF we just send the value back).
pub fn cookie_header(sid: &str, csrf: &str) -> String {
    format!("tw1337_sid={sid}; tw1337_csrf={csrf}")
}
