//! Transient command-suspension manager.
//!
//! Tracks per-command suspension expiry timestamps in memory. State is not
//! persisted across restarts. A suspended command is one whose key (the
//! command name without the leading `!`, lowercased) maps to a future
//! `Instant`.
//!
//! Also provides [`parse_duration`] for parsing short human-friendly duration
//! strings used by the admin suspend command (`30s`, `10m`, `2h`, `1d`, or a
//! bare integer meaning seconds).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::debug;

/// Upper bound on a single suspension: 7 days.
pub const MAX_SUSPEND_DURATION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Transient per-command suspension tracker.
///
/// Cheap to clone: the inner map is shared via `Arc<RwLock<_>>`. Keys are
/// normalized to lowercase on all operations.
#[derive(Clone, Default)]
pub struct SuspensionManager {
    inner: Arc<RwLock<HashMap<String, Instant>>>,
}

impl SuspensionManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Suspend `key` for `duration`, overwriting any existing entry.
    pub async fn suspend(&self, key: &str, duration: Duration) {
        let key = key.to_ascii_lowercase();
        let expiry = Instant::now() + duration;
        debug!(key = %key, ?duration, "Command suspended");
        self.inner.write().await.insert(key, expiry);
    }

    /// Remove any suspension for `key`.
    ///
    /// Returns `true` only if an entry existed AND had not yet expired (i.e.
    /// it was actively suspending the command). Stale entries are removed
    /// silently and reported as `false`.
    pub async fn unsuspend(&self, key: &str) -> bool {
        let key = key.to_ascii_lowercase();
        let mut guard = self.inner.write().await;
        match guard.remove(&key) {
            Some(expiry) if expiry > Instant::now() => {
                debug!(key = %key, "Command unsuspended");
                true
            }
            _ => false,
        }
    }

    /// Return `Some(remaining)` if `key` is currently suspended, else `None`.
    /// Expired entries are not removed; callers read and ignore them.
    pub async fn is_suspended(&self, key: &str) -> Option<Duration> {
        let key = key.to_ascii_lowercase();
        let guard = self.inner.read().await;
        let expiry = guard.get(&key)?;
        expiry.checked_duration_since(Instant::now())
    }
}

/// Error returned by [`parse_duration`].
#[derive(Debug, PartialEq, Eq)]
pub enum ParseDurationError {
    /// Input was empty or whitespace-only.
    Empty,
    /// Numeric portion was missing, non-ASCII-digit, or overflowed `u64`.
    InvalidNumber,
    /// Unit suffix was not one of `s`, `m`, `h`, `d`.
    UnknownUnit,
    /// Parsed duration was zero.
    Zero,
    /// Parsed duration exceeded [`MAX_SUSPEND_DURATION`].
    TooLong,
}

impl fmt::Display for ParseDurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "duration is empty"),
            Self::InvalidNumber => write!(f, "invalid number"),
            Self::UnknownUnit => write!(f, "unknown unit (use s, m, h, or d)"),
            Self::Zero => write!(f, "duration must be greater than zero"),
            Self::TooLong => write!(f, "duration exceeds maximum of 7 days"),
        }
    }
}

impl std::error::Error for ParseDurationError {}

/// Parse a short duration string.
///
/// Accepted forms: `30s`, `10m`, `2h`, `1d`, or a bare integer (seconds).
/// Duration must be > 0 and <= [`MAX_SUSPEND_DURATION`] (7 days).
pub fn parse_duration(input: &str) -> Result<Duration, ParseDurationError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ParseDurationError::Empty);
    }

    let (num_part, multiplier) = match s.as_bytes().last() {
        Some(&b) if b.is_ascii_digit() => (s, 1u64),
        Some(&b's') => (&s[..s.len() - 1], 1u64),
        Some(&b'm') => (&s[..s.len() - 1], 60u64),
        Some(&b'h') => (&s[..s.len() - 1], 3600u64),
        Some(&b'd') => (&s[..s.len() - 1], 86_400u64),
        Some(_) => return Err(ParseDurationError::UnknownUnit),
        None => return Err(ParseDurationError::Empty),
    };

    if num_part.is_empty() || !num_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseDurationError::InvalidNumber);
    }

    let n: u64 = num_part
        .parse()
        .map_err(|_| ParseDurationError::InvalidNumber)?;
    let secs = n
        .checked_mul(multiplier)
        .ok_or(ParseDurationError::InvalidNumber)?;

    if secs == 0 {
        return Err(ParseDurationError::Zero);
    }

    let duration = Duration::from_secs(secs);
    if duration > MAX_SUSPEND_DURATION {
        return Err(ParseDurationError::TooLong);
    }

    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_accepts_valid_forms() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn parse_duration_trims_whitespace() {
        assert_eq!(parse_duration("  30s  ").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert_eq!(parse_duration(""), Err(ParseDurationError::Empty));
        assert_eq!(parse_duration("   "), Err(ParseDurationError::Empty));
    }

    #[test]
    fn parse_duration_rejects_non_numeric() {
        assert_eq!(parse_duration("abc"), Err(ParseDurationError::UnknownUnit));
        // "-1" ends in a digit so the first-arm fast path treats the whole
        // string as seconds; the leading '-' then fails the digit-only check.
        assert_eq!(parse_duration("-1"), Err(ParseDurationError::InvalidNumber));
    }

    #[test]
    fn parse_duration_rejects_zero() {
        assert_eq!(parse_duration("0"), Err(ParseDurationError::Zero));
        assert_eq!(parse_duration("0s"), Err(ParseDurationError::Zero));
        assert_eq!(parse_duration("0m"), Err(ParseDurationError::Zero));
    }

    #[test]
    fn parse_duration_rejects_above_cap() {
        assert_eq!(parse_duration("8d"), Err(ParseDurationError::TooLong));
        assert_eq!(parse_duration("604801"), Err(ParseDurationError::TooLong));
    }

    #[test]
    fn parse_duration_accepts_exact_cap() {
        assert_eq!(parse_duration("7d").unwrap(), MAX_SUSPEND_DURATION);
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert_eq!(parse_duration("1w"), Err(ParseDurationError::UnknownUnit));
        assert_eq!(parse_duration("5y"), Err(ParseDurationError::UnknownUnit));
    }

    #[test]
    fn parse_duration_rejects_missing_number() {
        assert_eq!(parse_duration("s"), Err(ParseDurationError::InvalidNumber));
        assert_eq!(parse_duration("m"), Err(ParseDurationError::InvalidNumber));
    }

    #[tokio::test]
    async fn suspend_and_is_suspended() {
        let mgr = SuspensionManager::new();
        mgr.suspend("ai", Duration::from_secs(60)).await;
        let remaining = mgr.is_suspended("ai").await;
        assert!(remaining.is_some());
        assert!(remaining.unwrap() <= Duration::from_secs(60));
    }

    #[tokio::test]
    async fn is_suspended_returns_none_after_expiry() {
        let mgr = SuspensionManager::new();
        mgr.suspend("ai", Duration::from_millis(50)).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(mgr.is_suspended("ai").await.is_none());
    }

    #[tokio::test]
    async fn unsuspend_removes_active_entry() {
        let mgr = SuspensionManager::new();
        mgr.suspend("ai", Duration::from_secs(60)).await;
        assert!(mgr.unsuspend("ai").await);
        assert!(mgr.is_suspended("ai").await.is_none());
    }

    #[tokio::test]
    async fn unsuspend_unknown_key_returns_false() {
        let mgr = SuspensionManager::new();
        assert!(!mgr.unsuspend("nope").await);
    }

    #[tokio::test]
    async fn unsuspend_expired_entry_returns_false() {
        let mgr = SuspensionManager::new();
        mgr.suspend("ai", Duration::from_millis(20)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!mgr.unsuspend("ai").await);
    }

    #[tokio::test]
    async fn suspend_overwrites_existing_entry() {
        let mgr = SuspensionManager::new();
        mgr.suspend("ai", Duration::from_secs(3600)).await;
        mgr.suspend("ai", Duration::from_millis(30)).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(mgr.is_suspended("ai").await.is_none());
    }

    #[tokio::test]
    async fn keys_are_lowercased() {
        let mgr = SuspensionManager::new();
        mgr.suspend("AI", Duration::from_secs(60)).await;
        assert!(mgr.is_suspended("ai").await.is_some());
        assert!(mgr.is_suspended("Ai").await.is_some());
        assert!(mgr.unsuspend("aI").await);
    }

    #[tokio::test]
    async fn clone_shares_state() {
        let mgr = SuspensionManager::new();
        let clone = mgr.clone();
        mgr.suspend("ai", Duration::from_secs(60)).await;
        assert!(clone.is_suspended("ai").await.is_some());
    }
}
