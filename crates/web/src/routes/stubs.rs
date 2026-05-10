//! Placeholder pages for sidebar entries that aren't wired up yet.
//!
//! Mounted under the authed sub-router so mod-gating still applies.

use askama::Template;
use axum::Router;
use axum::extract::Extension;
use axum::response::Response;
use axum::routing::get;

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::routes::render;
use crate::state::WebState;

struct StubMeta {
    title: &'static str,
    subtitle: &'static str,
    slug: &'static str,
    note: &'static str,
    nav: &'static str,
}

const SCHEDULES: StubMeta = StubMeta {
    title: "Schedules",
    subtitle: "Recurring announcements fired by the bot on a wall-clock schedule.",
    slug: "schedules",
    note: "Schedules currently live in config.toml and hot-reload on save. A web editor is on the roadmap.",
    nav: crate::nav::SCHEDULES,
};

const FLIGHTS: StubMeta = StubMeta {
    title: "Flights",
    subtitle: "Active flight tracker subscriptions and last-seen ADS-B telemetry.",
    slug: "flights",
    note: "Tracker state is persisted in flights.ron and managed via !track / !untrack in chat.",
    nav: crate::nav::FLIGHTS,
};

const LOGS: StubMeta = StubMeta {
    title: "Logs",
    subtitle: "Live event tail across handlers — 1337 pings, AI turns, dreamer rituals.",
    slug: "logs",
    note: "Logs are emitted to stdout via tracing; ship them to your log aggregator of choice.",
    nav: crate::nav::LOGS,
};

const CONFIG: StubMeta = StubMeta {
    title: "Config",
    subtitle: "Read-only view of effective configuration: cooldowns, AI settings, channels.",
    slug: "config",
    note: "Configuration is sourced from config.toml; restart the bot to apply non-schedule changes.",
    nav: crate::nav::CONFIG,
};

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/schedules", get(|s| render_stub(SCHEDULES, s)))
        .route("/flights", get(|s| render_stub(FLIGHTS, s)))
        .route("/logs", get(|s| render_stub(LOGS, s)))
        .route("/config", get(|s| render_stub(CONFIG, s)))
}

#[derive(Template)]
#[template(path = "stub.html")]
struct StubTpl<'a> {
    title: &'a str,
    subtitle: &'a str,
    slug: &'a str,
    note: &'a str,
    csrf: String,
    user_login: &'a str,
    current_page: &'static str,
}

async fn render_stub(
    meta: StubMeta,
    Extension(session): Extension<Session>,
) -> Result<Response, WebError> {
    render(&StubTpl {
        title: meta.title,
        subtitle: meta.subtitle,
        slug: meta.slug,
        note: meta.note,
        csrf: csrf::encode(&session.csrf_value),
        user_login: &session.user_login,
        current_page: meta.nav,
    })
}
