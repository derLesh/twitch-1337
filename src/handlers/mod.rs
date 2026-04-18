//! Independent async handler tasks. Each submodule owns one long-running loop
//! subscribed to the shared broadcast channel of IRC messages or driving its own
//! polling cadence. Entry points are spawned from `main` and shut down together
//! via `tokio::select!`.

pub mod commands;
pub mod latency;
pub mod router;
pub mod schedules;
pub mod tracker_1337;
