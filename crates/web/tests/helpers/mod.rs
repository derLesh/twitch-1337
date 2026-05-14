//! Shared fixtures for route-level integration tests in `crates/web/tests/`.
//!
//! Each test binary in `crates/web/tests/*.rs` includes this module via
//! `mod helpers;`, so cargo treats it as a separate compilation unit per
//! binary and dead-code warnings fire for any helper a given binary
//! doesn't use. The `#![allow(dead_code)]` here is the cheapest fix.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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

/// A clock that advances by one second on every call to `now()`.
/// The first call returns `T0`, the second `T0 + 1s`, etc.
/// Used to ensure that `elapsed > role_check_refresh` evaluates to `true`
/// even when `role_check_refresh` is `Duration::ZERO`.
pub struct StepClock {
    base: DateTime<Utc>,
    counter: AtomicI64,
}

impl StepClock {
    pub fn new(base: DateTime<Utc>) -> Self {
        Self {
            base,
            counter: AtomicI64::new(0),
        }
    }
}

impl Clock for StepClock {
    fn now(&self) -> DateTime<Utc> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        self.base + chrono::Duration::seconds(n)
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
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    build_state_inner(helix, Duration::from_secs(300), clock).await
}

/// Like [`build_state_with_dirs`] but lets the caller override the
/// `role_check_refresh` window.
///
/// Uses a [`StepClock`] that increments by one second on each `now()` call so
/// that `elapsed > role_check_refresh` evaluates to `true` even when
/// `role_check_refresh` is `Duration::ZERO` — the session is inserted at `T0`
/// and the first middleware check sees `T0 + Ns` where `N ≥ 1`.
pub async fn build_state_with_overrides(
    helix: Arc<dyn HelixClient>,
    role_check_refresh: Duration,
) -> (WebState, TempDir, TempDir) {
    let clock: Arc<dyn Clock> = Arc::new(StepClock::new(
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    build_state_inner(helix, role_check_refresh, clock).await
}

/// Variant that returns the ping, memory, and settings tempdirs so callers
/// can keep all three alive while exercising persistent CRUD paths — used
/// by `/settings` tests which actually hit `settings_store.apply/reset`.
pub async fn build_state_with_all_dirs(
    helix: Arc<dyn HelixClient>,
) -> (WebState, TempDir, TempDir, TempDir) {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    build_state_inner_keep_settings(helix, Duration::from_secs(300), clock).await
}

async fn build_state_inner(
    helix: Arc<dyn HelixClient>,
    role_check_refresh: Duration,
    clock: Arc<dyn Clock>,
) -> (WebState, TempDir, TempDir) {
    let (state, td_p, td_m, td_s) =
        build_state_inner_keep_settings(helix, role_check_refresh, clock).await;
    // Settings tempdir is intentionally dropped here; helper-driven tests
    // that need the on-disk `settings.ron` to survive should use
    // [`build_state_with_all_dirs`] instead.
    drop(td_s);
    (state, td_p, td_m)
}

async fn build_state_inner_keep_settings(
    helix: Arc<dyn HelixClient>,
    role_check_refresh: Duration,
    clock: Arc<dyn Clock>,
) -> (WebState, TempDir, TempDir, TempDir) {
    let pings_dir = TempDir::new().expect("pings tempdir");
    let memory_dir = TempDir::new().expect("memory tempdir");
    let pings = PingManager::load(pings_dir.path()).expect("load empty ping manager");
    let ping_manager = Arc::new(RwLock::new(pings));
    let memory_store = MemoryStore::open(memory_dir.path(), Caps::default())
        .await
        .expect("open memory store");
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
        role_check_refresh,
    });
    // Tests don't need a real production secret; a fixed 32-byte key keeps
    // signed-cookie round-trips deterministic across reruns.
    let signed_key = tower_cookies::Key::from(&[0x42u8; 64]);
    let settings_dir = TempDir::new().expect("settings tempdir");
    let audit: Arc<dyn twitch_1337_core::settings::AuditLog> =
        Arc::new(twitch_1337_core::settings::FileAuditLog::new(
            settings_dir.path().join("settings_audit.log"),
        ));
    let (settings_store, settings_handle) =
        twitch_1337_core::settings::SettingsStore::open(settings_dir.path(), audit)
            .expect("open settings store");
    let state = WebState {
        sessions,
        helix,
        irc_connected: Arc::new(AtomicBool::new(true)),
        config,
        clock,
        channel: Arc::from("testchannel"),
        broadcaster_id: Arc::from("100"),
        hidden_admins: Arc::from(Vec::<String>::new().into_boxed_slice()),
        viewer_allowlist: Arc::from(Vec::<String>::new().into_boxed_slice()),
        client_id: SecretString::new("test-client-id".to_owned().into()),
        oauth,
        ping_manager,
        memory_store,
        signed_key,
        leaderboard: Arc::new(RwLock::new(HashMap::new())),
        tracker_tx: None,
        avatar_cache: Arc::new(twitch_1337_web::helix::AvatarCache::new(
            Duration::from_secs(3600),
        )),
        owner_id: None,
        settings: settings_handle,
        settings_store,
    };
    (state, pings_dir, memory_dir, settings_dir)
}

/// Insert a session for `(user_id, user_login)` and return
/// `(signed_sid_for_cookie, signed_csrf_for_cookie, bare_csrf_for_form_field)`.
/// Used by ping/memory route tests to skip the OAuth round-trip; cookie
/// headers must carry the signed values (HMAC tag + payload) because the
/// production OAuth callback writes them via `cookies.signed(&key)`. The
/// bare csrf is what the user's browser fills into the `_csrf=` form field
/// or `X-Csrf-Token` header — `crate::auth::csrf::verify` compares the bare
/// hex against `session.csrf_value`.
pub fn insert_session(
    state: &WebState,
    user_id: &str,
    user_login: &str,
) -> (String, String, String) {
    insert_session_as(state, user_id, user_login, twitch_1337_web::auth::Role::Mod)
}

/// Like [`insert_session`] but lets the caller specify the role.
pub fn insert_session_as(
    state: &WebState,
    user_id: &str,
    user_login: &str,
    role: twitch_1337_web::auth::Role,
) -> (String, String, String) {
    let (sid, csrf) = state
        .sessions
        .insert(twitch_1337_web::auth::session::NewSession {
            user_id: user_id.to_owned(),
            user_login: user_login.to_owned(),
            role,
            avatar_url: None,
            is_broadcaster: false,
        })
        .expect("insert session");
    let bare_csrf = hex::encode(csrf);
    let signed_sid = sign_for_tests(state, "tw1337_sid", &sid);
    let signed_csrf = sign_for_tests(state, "tw1337_csrf", &bare_csrf);
    (signed_sid, signed_csrf, bare_csrf)
}

/// Sign a cookie value against `state.signed_key` so handlers see a cookie
/// the signed extractor will accept. Round-trips through an in-memory
/// `Cookies` jar so we get the exact signed string the production
/// `Set-Cookie` path emits.
pub fn sign_for_tests(state: &WebState, name: &str, value: &str) -> String {
    use tower_cookies::Cookies;
    let cookies = Cookies::default();
    let signed = cookies.signed(&state.signed_key);
    signed.add(
        tower_cookies::Cookie::build((name.to_owned(), value.to_owned()))
            .path("/")
            .build(),
    );
    cookies
        .get(name)
        .expect("cookie present after signed add")
        .value()
        .to_owned()
}

/// `Cookie:` header value combining sid + csrf, matching what the browser
/// would send after a successful login. Tests inject the *signed* values,
/// because that's what hits the server in production.
pub fn cookie_header(signed_sid: &str, signed_csrf: &str) -> String {
    format!("tw1337_sid={signed_sid}; tw1337_csrf={signed_csrf}")
}
