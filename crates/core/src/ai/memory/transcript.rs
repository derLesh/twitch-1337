//! Append-as-you-go transcript file. Line format:
//!   `HH:MM:SS  <user>: <text>` (Berlin local time).
//! `today.md` is the open handle; ritual rotation renames it to
//! `<YYYY-MM-DD>.md` and opens a fresh `today.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Europe::Berlin;
use eyre::{Result, WrapErr as _};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct TranscriptWriter {
    inner: Arc<Mutex<Inner>>,
    transcripts_dir: PathBuf,
}

struct Inner {
    handle: tokio::fs::File,
}

impl TranscriptWriter {
    pub async fn open(memories_dir: &Path) -> Result<Self> {
        let dir = memories_dir.join("transcripts");
        tokio::fs::create_dir_all(&dir)
            .await
            .wrap_err_with(|| format!("create_dir_all {}", dir.display()))?;
        let path = dir.join("today.md");
        let handle = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .wrap_err_with(|| format!("open transcripts/today.md at {}", path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner { handle })),
            transcripts_dir: dir,
        })
    }

    pub async fn append_line(&self, ts: DateTime<Utc>, user: &str, text: &str) -> Result<()> {
        let local = ts.with_timezone(&Berlin);
        let safe_user = user.replace(['\n', '\r'], " ");
        let safe_text = text.replace(['\n', '\r'], " ");
        let line = format!(
            "{}  {}: {}\n",
            local.format("%H:%M:%S"),
            safe_user,
            safe_text
        );
        let mut g = self.inner.lock().await;
        g.handle
            .write_all(line.as_bytes())
            .await
            .wrap_err("write transcript")?;
        Ok(())
    }

    /// Close current `today.md`, rename to `<date>.md`, reopen empty `today.md`.
    pub async fn rotate_to(&self, date: NaiveDate) -> Result<PathBuf> {
        let mut g = self.inner.lock().await;
        g.handle.flush().await.ok();
        let today = self.transcripts_dir.join("today.md");
        let dated = self
            .transcripts_dir
            .join(format!("{}.md", date.format("%Y-%m-%d")));
        tokio::fs::rename(&today, &dated)
            .await
            .wrap_err_with(|| format!("rotate {} -> {}", today.display(), dated.display()))?;
        let new_handle = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&today)
            .await
            .wrap_err("reopen today.md")?;
        g.handle = new_handle;
        Ok(dated)
    }

    pub fn dated_path_for(&self, date: NaiveDate) -> PathBuf {
        self.transcripts_dir
            .join(format!("{}.md", date.format("%Y-%m-%d")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono::TimeZone as _;
    use chrono_tz::Europe::Berlin;

    #[tokio::test]
    async fn append_writes_line() {
        let dir = tempfile::tempdir().unwrap();
        let memories = dir.path().join("memories");
        tokio::fs::create_dir_all(memories.join("transcripts"))
            .await
            .unwrap();
        let w = TranscriptWriter::open(&memories).await.unwrap();
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 4, 28, 10, 42, 11)
            .unwrap();
        w.append_line(ts, "alice", "hi").await.unwrap();
        let s = tokio::fs::read_to_string(memories.join("transcripts/today.md"))
            .await
            .unwrap();
        assert!(s.contains("alice: hi"));
    }

    #[tokio::test]
    async fn rotate_renames_today_to_yesterday_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let memories = dir.path().join("memories");
        tokio::fs::create_dir_all(memories.join("transcripts"))
            .await
            .unwrap();
        let w = TranscriptWriter::open(&memories).await.unwrap();
        w.append_line(chrono::Utc::now(), "a", "x").await.unwrap();
        let yday = NaiveDate::from_ymd_opt(2026, 4, 27).unwrap();
        let dest = w.rotate_to(yday).await.unwrap();
        assert_eq!(dest.file_name().unwrap().to_string_lossy(), "2026-04-27.md");
        assert!(dest.exists());
        assert!(memories.join("transcripts/today.md").exists()); // reopened empty
        let _ = Berlin; // suppress unused
    }
}
