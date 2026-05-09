//! Web-crate-local config view.
//!
//! Mirrors the relevant fields of `core::config::WebConfig` so the web crate
//! stays decoupled from core. The bin populates this from the parsed
//! `Configuration` when wiring up the dashboard.

use std::time::Duration;

use secrecy::SecretString;

#[derive(Clone)]
pub struct WebConfig {
    pub bind_addr: String,
    pub public_url: String,
    pub session_secret: SecretString,
    pub session_ttl: Duration,
    pub mod_check_refresh: Duration,
}
