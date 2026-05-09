//! Wall-clock abstraction for the web crate.
//!
//! A small `now()`-only trait so route tests can substitute a stub clock
//! without dragging in the async `sleep_until` half of `core::util::clock::Clock`
//! (which exists for time-driven schedulers — not for HTTP request timing).

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
