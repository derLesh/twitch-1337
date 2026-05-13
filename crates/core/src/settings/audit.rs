//! Append-only JSON-lines audit log for settings changes.

#[cfg(any(test, feature = "testing"))]
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub ts: DateTime<chrono_tz::Tz>,
    pub actor_id: String,
    pub actor_login: String,
    pub changes: Vec<AuditChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditChange {
    pub key: String,
    /// Old value, JSON-encoded. `null` if the field had no override (was at default).
    pub old: serde_json::Value,
    /// New value, JSON-encoded.
    pub new: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
}

pub trait AuditLog: Send + Sync {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError>;
}

pub struct FileAuditLog {
    path: std::path::PathBuf,
}

impl FileAuditLog {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AuditLog for FileAuditLog {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        use std::io::Write as _;
        let line = serde_json::to_string(entry)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.sync_all()?;
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
pub struct MemoryAuditLog {
    entries: Mutex<Vec<AuditEntry>>,
}

#[cfg(any(test, feature = "testing"))]
impl MemoryAuditLog {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn snapshot(&self) -> Vec<AuditEntry> {
        self.entries.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "testing"))]
impl Default for MemoryAuditLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl AuditLog for MemoryAuditLog {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        self.entries.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

pub fn berlin_now(now_utc: DateTime<Utc>) -> DateTime<chrono_tz::Tz> {
    now_utc.with_timezone(&chrono_tz::Europe::Berlin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> AuditEntry {
        AuditEntry {
            ts: berlin_now("2026-05-12T13:37:00Z".parse::<DateTime<Utc>>().unwrap()),
            actor_id: "12345678".into(),
            actor_login: "chronophylos".into(),
            changes: vec![AuditChange {
                key: "cooldowns.ai".into(),
                old: serde_json::Value::Number(30.into()),
                new: serde_json::Value::Number(15.into()),
            }],
        }
    }

    #[test]
    fn memory_log_records_entries() {
        let log = MemoryAuditLog::new();
        let e = sample_entry();
        log.append(&e).expect("append");
        log.append(&e).expect("append twice");
        assert_eq!(log.snapshot().len(), 2);
    }

    #[test]
    fn file_log_appends_one_json_line_per_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let log = FileAuditLog::new(&path);
        let e = sample_entry();
        log.append(&e).expect("first");
        log.append(&e).expect("second");
        let body = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("valid json");
            assert_eq!(parsed["actor_id"], "12345678");
            assert_eq!(parsed["changes"][0]["key"], "cooldowns.ai");
        }
    }

    #[test]
    fn file_log_survives_truncation_between_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let log = FileAuditLog::new(&path);
        log.append(&sample_entry()).expect("first");
        std::fs::remove_file(&path).expect("remove");
        log.append(&sample_entry()).expect("second after unlink");
        assert!(path.exists());
        let lines = std::fs::read_to_string(&path)
            .expect("read")
            .lines()
            .count();
        assert_eq!(lines, 1);
    }
}
