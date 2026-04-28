//! A fake `Clock` driven by explicit `set`/`advance` calls plus registered waiters.
//!
//! Tests use `#[tokio::test(start_paused = true)]` so tokio-native sleeps
//! stop advancing wall-clock time. This clock is the authoritative source
//! of `now_utc` for the code under test; tests drive it with `advance` or
//! `set` to simulate time passing. Registered waiters (from `sleep_until`)
//! fire when their target becomes reached.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use tokio::sync::oneshot;

use twitch_1337::util::clock::Clock;

struct Waiter {
    target: DateTime<Utc>,
    tx: oneshot::Sender<()>,
}

pub struct FakeClock {
    now: Mutex<DateTime<Utc>>,
    waiters: Mutex<Vec<Waiter>>,
}

impl FakeClock {
    pub fn new(now: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now),
            waiters: Mutex::new(Vec::new()),
        })
    }

    pub fn set(&self, t: DateTime<Utc>) {
        *self.now.lock().unwrap() = t;
        self.wake_due(t);
    }

    pub fn advance(&self, delta: Duration) {
        let now = {
            let mut guard = self.now.lock().unwrap();
            *guard += delta;
            *guard
        };
        self.wake_due(now);
    }

    fn wake_due(&self, now: DateTime<Utc>) {
        let mut waiters = self.waiters.lock().unwrap();
        let mut pending = Vec::with_capacity(waiters.len());
        for w in waiters.drain(..) {
            if w.target <= now {
                let _ = w.tx.send(());
            } else {
                pending.push(w);
            }
        }
        *waiters = pending;
    }
}

#[async_trait]
impl Clock for FakeClock {
    fn now_utc(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }

    async fn sleep_until(&self, target: DateTime<Utc>) {
        let now = *self.now.lock().unwrap();
        if target <= now {
            return;
        }
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().unwrap().push(Waiter { target, tx });
        let _ = rx.await;
    }
}
