//! Tracing / observability initialisation.

use chrono::Utc;
use tracing_error::ErrorLayer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// `HH:MM:SS.mmm` (UTC). The default `SystemTime` formatter emits the full
/// RFC3339 timestamp on every line (~30 chars including date + tz +
/// microseconds), which is overkill for a single-process bot whose logs
/// always belong to today. The short form preserves millisecond precision —
/// enough to order events within a handler — while leaving room for the
/// actual message content.
struct ShortTimer;

impl FormatTime for ShortTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", Utc::now().format("%H:%M:%S%.3f"))
    }
}

/// Install the global tracing subscriber (format + env-filter + error layer).
///
/// Call once at program startup before any spans are created.
pub fn install_tracing() {
    // `with_target(true)` surfaces the emitting module (e.g. `twitch_1337`
    // vs `twitch_irc` vs `reqwest`) so an unexpected line can be traced
    // back to a crate without grepping. Pair with `ShortTimer` for the
    // line-budget we just freed by dropping the date prefix.
    let fmt_layer = fmt::layer().with_target(true).with_timer(ShortTimer);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();
}
