//! In-memory session table.
//!
//! Sessions sit behind a `RwLock<HashMap>` keyed by a 64-hex-char random
//! id. TTL is sliding: every successful `get_and_touch` bumps `last_seen`,
//! so an active user stays logged in indefinitely. The role-gate middleware
//! also stamps `last_role_check` so it knows when to re-verify the helix
//! moderator list.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use eyre::Result;
use rand::Rng as _;

use crate::auth::role::Role;
use crate::clock::Clock;

pub type SessionId = String;

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: String,
    pub user_login: String,
    pub role: Role,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_role_check: DateTime<Utc>,
    pub csrf_value: [u8; 32],
}

impl Session {
    pub fn is_mod(&self) -> bool {
        self.role == Role::Mod
    }
}

pub struct SessionTable {
    inner: RwLock<HashMap<SessionId, Session>>,
    ttl: Duration,
    clock: Arc<dyn Clock>,
}

impl SessionTable {
    pub fn new(ttl: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl,
            clock,
        }
    }

    /// Returns the new session id together with the freshly-generated csrf
    /// value so the OAuth callback can set both cookies without a second
    /// lookup against the table.
    pub fn insert(
        &self,
        user_id: String,
        user_login: String,
        role: Role,
    ) -> Result<(SessionId, [u8; 32])> {
        let now = self.clock.now();
        let mut rng = rand::rng();
        let mut id_bytes = [0u8; 32];
        rng.fill_bytes(&mut id_bytes);
        let mut csrf = [0u8; 32];
        rng.fill_bytes(&mut csrf);
        let id = hex::encode(id_bytes);
        self.inner.write().unwrap().insert(
            id.clone(),
            Session {
                user_id,
                user_login,
                role,
                issued_at: now,
                last_seen: now,
                last_role_check: now,
                csrf_value: csrf,
            },
        );
        Ok((id, csrf))
    }

    pub fn get_and_touch(&self, id: &str) -> Option<Session> {
        let now = self.clock.now();
        let ttl = chrono::Duration::from_std(self.ttl).ok()?;
        let mut g = self.inner.write().unwrap();
        let session = g.get_mut(id)?;
        if now.signed_duration_since(session.last_seen) > ttl {
            g.remove(id);
            return None;
        }
        session.last_seen = now;
        Some(session.clone())
    }

    pub fn drop_session(&self, id: &str) {
        self.inner.write().unwrap().remove(id);
    }

    pub fn record_role_check(&self, id: &str) {
        let now = self.clock.now();
        if let Some(s) = self.inner.write().unwrap().get_mut(id) {
            s.last_role_check = now;
        }
    }
}
