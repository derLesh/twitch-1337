//! Wall-clock abstraction for time-dependent handlers.
//!
//! Production code uses [`SystemClock`], which delegates to
//! [`chrono::Utc::now`] and [`tokio::time::sleep`]. Integration tests
//! substitute a fake clock that the test body advances manually, paired
//! with [`tokio::time::pause`] so tokio-native sleeps also advance
//! deterministically.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    async fn sleep_until(&self, target: DateTime<Utc>);
}

#[derive(Debug, Default)]
pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }

    async fn sleep_until(&self, target: DateTime<Utc>) {
        let delta = (target - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        tokio::time::sleep(delta).await;
    }
}
