//! Shared application state for axum handlers.
//!
//! Constructed in the bin (or by integration tests) and passed to
//! [`crate::build_router`]. Every handler clones individual `Arc`s out of
//! this struct.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eyre::eyre;
use secrecy::ExposeSecret;
use secrecy::SecretString;
use tokio::sync::RwLock;
use tower_cookies::Key;
use twitch_1337_core::ai::memory::store::MemoryStore;
use twitch_1337_core::aviation::TrackerCommand;
use twitch_1337_core::commands::leaderboard::PersonalBest;
use twitch_1337_core::ping::PingManager;

use crate::auth::OAuthCtx;
use crate::auth::session::SessionTable;
use crate::clock::Clock;
use crate::config::WebConfig;
use crate::helix::HelixClient;

#[derive(Clone)]
pub struct WebState {
    pub sessions: Arc<SessionTable>,
    pub helix: Arc<dyn HelixClient>,
    pub irc_connected: Arc<AtomicBool>,
    pub config: Arc<WebConfig>,
    pub clock: Arc<dyn Clock>,
    /// primary channel login (lowercase Twitch login).
    pub channel: Arc<str>,
    /// resolved at startup via helix users by login.
    pub broadcaster_id: Arc<str>,
    pub hidden_admins: Arc<[String]>,
    /// Twitch user IDs granted viewer-tier access. Sourced from
    /// `[twitch].viewer_allowlist`.
    pub viewer_allowlist: Arc<[String]>,
    /// Twitch developer-app client id (used in `Client-Id` headers when the
    /// callback fetches the caller's user record).
    pub client_id: SecretString,
    pub oauth: Arc<OAuthCtx>,
    /// Shared ping manager (same instance the bot uses for `!p` / `!<ping>`
    /// commands). Wrapped in a tokio `RwLock` because writes (create/edit/
    /// delete) persist to disk and must serialize against IRC handler
    /// triggers.
    pub ping_manager: Arc<RwLock<PingManager>>,
    /// Shared v2 memory store (same `Arc`-backed instance the bot's `!ai`
    /// turn / dreamer ritual writes through). Sharing the store keeps the
    /// per-path mutex map coherent — two independent stores against the
    /// same on-disk tree would silently race past each other's locks.
    pub memory_store: MemoryStore,
    /// HMAC key for signed cookies (sid + csrf). Derived from
    /// `[web].session_secret` in the bin so tampering with sid is detected
    /// on the next request rather than handled by HashMap miss alone.
    pub signed_key: Key,
    /// Shared 1337 leaderboard (same `Arc` the tracker handler writes
    /// through). `None`-valued entries mean the user was removed. Read-only
    /// from web handlers; mutations happen only inside the IRC tracker.
    pub leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,
    /// Sender half of the flight-tracker command channel. `None` when
    /// aviation is disabled at startup; the `/flights` handler then renders
    /// the disabled placeholder instead of awaiting a snapshot.
    pub tracker_tx: Option<Arc<tokio::sync::mpsc::Sender<TrackerCommand>>>,
}

/// Derive the signed-cookie [`Key`] from `[web].session_secret`.
///
/// `Key::derive_from` panics on secrets shorter than 32 bytes; this helper
/// turns that into a typed `eyre` error so the bin can surface a clean
/// startup failure instead of crashing.
pub fn derive_session_key(secret: &SecretString) -> eyre::Result<Key> {
    let bytes = secret.expose_secret().as_bytes();
    if bytes.len() < 32 {
        return Err(eyre!(
            "web.session_secret must be at least 32 bytes (got {})",
            bytes.len()
        ));
    }
    Ok(Key::derive_from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_session_key_rejects_short_secret() {
        let secret = SecretString::from("a".repeat(31));
        let err = derive_session_key(&secret).expect_err("31 bytes must fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("at least 32 bytes"),
            "error message should mention min length, got: {msg}"
        );
        assert!(
            msg.contains("31"),
            "error message should mention actual length, got: {msg}"
        );
    }

    #[test]
    fn derive_session_key_accepts_32_bytes() {
        let secret = SecretString::from("a".repeat(32));
        derive_session_key(&secret).expect("32 bytes is the minimum and must succeed");
    }

    #[test]
    fn derive_session_key_accepts_64_bytes() {
        let secret = SecretString::from("a".repeat(64));
        derive_session_key(&secret).expect("64 bytes must succeed");
    }
}
