//! Atomic file persistence helpers.
//!
//! Both helpers serialize to RON, write the destination with extension
//! replaced by `ron.tmp`, then rename onto the target so concurrent readers
//! see either the old or the new file but never a torn write. Callers should
//! pass a path ending in `.ron`. Sync (`atomic_save_ron`) and async
//! (`atomic_save_ron_async`) variants exist because some call-sites run
//! inside sync constructors and others inside `async fn`s with `tokio::fs`
//! already in scope. "Atomic" here means no torn reads; it does NOT imply
//! crash-safety (no `fsync` is performed).

use std::path::Path;

use eyre::{Result, WrapErr as _};
use serde::Serialize;

/// Synchronously serialize `value` as pretty RON and atomically write to `path`.
pub fn atomic_save_ron<T: Serialize>(value: &T, path: &Path) -> Result<()> {
    let tmp = path.with_extension("ron.tmp");
    let data = ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
        .wrap_err("Failed to serialize value to RON")?;
    std::fs::write(&tmp, &data)
        .wrap_err_with(|| format!("Failed to write tmp file {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .wrap_err_with(|| format!("Failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Async variant — uses `tokio::fs` so it does not block the runtime.
/// Serializes with `ron::to_string` (compact) to match the existing tracker
/// behaviour; switch to pretty if a call-site needs human inspection.
pub async fn atomic_save_ron_async<T: Serialize>(value: &T, path: &Path) -> Result<()> {
    let tmp = path.with_extension("ron.tmp");
    let data = ron::to_string(value).wrap_err("Failed to serialize value to RON")?;
    tokio::fs::write(&tmp, data.as_bytes())
        .await
        .wrap_err_with(|| format!("Failed to write tmp file {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .wrap_err_with(|| format!("Failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Sample {
        a: u32,
        b: String,
    }

    #[test]
    fn sync_writes_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.ron");
        let value = Sample {
            a: 7,
            b: "hi".into(),
        };
        atomic_save_ron(&value, &path).unwrap();
        let loaded: Sample = ron::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded, value);
        assert!(!path.with_extension("ron.tmp").exists());
    }

    #[tokio::test]
    async fn async_writes_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.ron");
        let value = Sample {
            a: 1,
            b: "x".into(),
        };
        atomic_save_ron_async(&value, &path).await.unwrap();
        let loaded: Sample =
            ron::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(loaded, value);
        assert!(!path.with_extension("ron.tmp").exists());
    }
}
