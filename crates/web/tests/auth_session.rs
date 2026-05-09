use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use twitch_1337_web::auth::session::SessionTable;
use twitch_1337_web::clock::Clock;

struct StubClock(std::sync::Mutex<DateTime<Utc>>);

impl Clock for StubClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

impl StubClock {
    fn new(t: DateTime<Utc>) -> Self {
        Self(std::sync::Mutex::new(t))
    }
    fn advance(&self, secs: i64) {
        let mut g = self.0.lock().unwrap();
        *g += chrono::Duration::seconds(secs);
    }
}

#[test]
fn session_round_trips() {
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let table = SessionTable::new(Duration::from_secs(7 * 24 * 3600), clock.clone());
    let (id, _csrf) = table
        .insert("12345".into(), "alice".into())
        .expect("insert");
    let got = table.get_and_touch(&id).expect("present");
    assert_eq!(got.user_login, "alice");
    assert_eq!(got.user_id, "12345");
}

#[test]
fn session_expires_after_ttl() {
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let table = SessionTable::new(Duration::from_secs(60), clock.clone());
    let (id, _csrf) = table.insert("12345".into(), "alice".into()).unwrap();
    clock.advance(61);
    assert!(
        table.get_and_touch(&id).is_none(),
        "expected expiry past TTL"
    );
}

#[test]
fn session_sliding_refresh_keeps_alive() {
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let table = SessionTable::new(Duration::from_secs(120), clock.clone());
    let (id, _csrf) = table.insert("12345".into(), "alice".into()).unwrap();
    clock.advance(60);
    assert!(table.get_and_touch(&id).is_some()); // bumps last_seen
    clock.advance(90);
    assert!(
        table.get_and_touch(&id).is_some(),
        "sliding refresh should keep alive"
    );
    clock.advance(150);
    assert!(table.get_and_touch(&id).is_none());
}
