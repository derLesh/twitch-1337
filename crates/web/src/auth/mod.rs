//! Web auth: OAuth + session + CSRF + role-gate plumbing.
//!
//! Module map:
//! - [`session`]: in-memory session table (TTL + sliding refresh)
//! - [`csrf`]: hex-encoded double-submit token helpers
//! - [`role_check`]: hidden_admins → broadcaster → helix moderators
//! - [`routes`]: login / callback / logout handlers + middleware

pub mod csrf;
pub mod role;
pub mod role_check;
pub mod session;

pub(crate) mod routes;

// `require_csrf` is intentionally not re-exported. CSRF is enforced
// per-handler (form-field `_csrf` path); the header-path middleware would
// silently admit form-only POSTs by design, so exporting it would mislead
// callers into thinking it provides blanket protection.
pub use role::Role;
pub use routes::{
    CSRF_COOKIE, OAuthCtx, SID_COOKIE, auth_router, require_mod, require_role, viewer_method_guard,
};
