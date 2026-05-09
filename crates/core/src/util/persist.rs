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

use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

/// Errors that can occur during atomic persistence operations.
#[derive(Debug, Error)]
pub enum AtomicPersistError {
    /// Failed to serialize value to RON format.
    #[error("Failed to serialize value to RON: {0}")]
    Serialization(#[from] ron::Error),

    /// Failed to write temporary file.
    #[error("Failed to write tmp file {path}: {source}")]
    WriteTmp {
        /// Path of the temporary file.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// Failed to rename temporary file to destination.
    #[error("Failed to rename {from} -> {to}: {source}")]
    Rename {
        /// Path of the source file.
        from: PathBuf,
        /// Path of the destination file.
        to: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, AtomicPersistError>;

fn serialize<T: Serialize>(value: &T) -> Result<String> {
    ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
        .map_err(AtomicPersistError::Serialization)
}

/// Synchronously serialize `value` as pretty RON and atomically write to `path`.
pub fn atomic_save_ron<T: Serialize>(value: &T, path: &Path) -> Result<()> {
    let tmp = path.with_extension("ron.tmp");
    let data = serialize(value)?;

    std::fs::write(&tmp, &data).map_err(|source| AtomicPersistError::WriteTmp {
        path: tmp.clone(),
        source,
    })?;

    std::fs::rename(&tmp, path).map_err(|source| AtomicPersistError::Rename {
        from: tmp.clone(),
        to: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

/// Async byte-level atomic write — does not require `Serialize`.
/// Writes `bytes` to a `.tmp` sibling then renames onto `path`.
pub async fn atomic_write_bytes_async(
    bytes: &[u8],
    path: &Path,
) -> std::result::Result<(), AtomicPersistError> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|source| AtomicPersistError::WriteTmp {
            path: tmp.clone(),
            source,
        })?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|source| AtomicPersistError::Rename {
            from: tmp.clone(),
            to: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

/// Async variant — uses `tokio::fs` so it does not block the runtime.
/// Serializes with `ron::to_string` (compact) to match the existing tracker
/// behaviour; switch to pretty if a call-site needs human inspection.
pub async fn atomic_save_ron_async<T: Serialize>(value: &T, path: &Path) -> Result<()> {
    let tmp = path.with_extension("ron.tmp");
    let data = serialize(value)?;

    tokio::fs::write(&tmp, data.as_bytes())
        .await
        .map_err(|source| AtomicPersistError::WriteTmp {
            path: tmp.clone(),
            source,
        })?;

    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|source| AtomicPersistError::Rename {
            from: tmp.clone(),
            to: path.to_path_buf(),
            source,
        })?;

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
