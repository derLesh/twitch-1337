//! Twitch IRC bot library crate.
//!
//! The binary (`src/main.rs`) reads config, builds a production
//! `TwitchIRCClient`, and runs the bot. Integration tests (`tests/`) use a
//! fake transport, fake clock, and fake LLM against the same handlers.

pub mod aviation;
pub mod clock;
pub mod commands;
pub mod config;
pub mod cooldown;
pub mod database;
pub mod flight_tracker;
pub mod handlers;
pub mod llm;
pub mod memory;
pub mod ping;
pub mod prefill;
pub mod token_storage;
pub mod util;

use twitch_irc::TwitchIRCClient;
use twitch_irc::login::RefreshingLoginCredentials;

/// Generic alias for any authenticated Twitch IRC client. The production
/// default is `SecureTCPTransport` + file-backed refreshing credentials.
pub type AuthenticatedTwitchClient<
    T = twitch_irc::SecureTCPTransport,
    L = RefreshingLoginCredentials<crate::token_storage::FileBasedTokenStorage>,
> = TwitchIRCClient<T, L>;

pub use handlers::tracker_1337::PersonalBest;
pub use token_storage::FileBasedTokenStorage;
pub use util::{
    APP_USER_AGENT, ChatHistory, MAX_RESPONSE_LENGTH, get_config_path, get_data_dir,
    parse_flight_duration, resolve_berlin_time, truncate_response,
};
