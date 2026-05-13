//! Sidebar navigation page identifiers.
//!
//! Each authed handler passes one of these to its `Tpl.current_page`
//! field; the sidebar template (`templates/sidebar.html`) compares
//! against the same literals to apply `class="active"`. Defining them
//! once here turns a typo on the Rust side into a name-resolution
//! error — the template still uses raw strings, but those are reviewed
//! together at the top of `sidebar.html` and any drift is visible in
//! a single file.

pub const LEADERBOARD: &str = "leaderboard";
pub const PINGS: &str = "pings";
pub const MEMORY_TREE: &str = "memory";
pub const MEMORY_SOUL: &str = "memory_soul";
pub const MEMORY_LORE: &str = "memory_lore";
pub const MEMORY_USERS: &str = "memory_users";
pub const MEMORY_STATE: &str = "memory_state";
pub const SCHEDULES: &str = "schedules";
pub const FLIGHTS: &str = "flights";
pub const LOGS: &str = "logs";
pub const CONFIG: &str = "config";
pub const SETTINGS: &str = "settings";
