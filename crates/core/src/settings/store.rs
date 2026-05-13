//! Owner of `$DATA_DIR/settings.ron`. Serializes writes, validates, swaps
//! the shared `SettingsHandle`, and appends an audit log entry per apply.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::Utc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use super::audit::{AuditChange, AuditEntry, AuditLog, berlin_now};
use super::overrides::SettingsOverrides;
use super::{Settings, SettingsError, SettingsHandle, SettingsSection};

const FILE_NAME: &str = "settings.ron";

#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: String,
    pub user_login: String,
}

pub struct SettingsStore {
    path: PathBuf,
    defaults: Settings,
    handle: SettingsHandle,
    audit: Arc<dyn AuditLog>,
    write_lock: Mutex<()>,
}

impl SettingsStore {
    /// Open the store from `$DATA_DIR`. Reads the existing `settings.ron`
    /// (if any), resolves against compile-time defaults, validates, and
    /// returns the `(store, handle)` pair. A corrupt or out-of-bound file
    /// is renamed to `settings.ron.quarantine-<unix_ts>` and the load
    /// falls back to compile defaults so the bot can still boot.
    pub fn open(
        data_dir: &Path,
        audit: Arc<dyn AuditLog>,
    ) -> Result<(Arc<Self>, SettingsHandle), SettingsError> {
        let path = data_dir.join(FILE_NAME);
        let defaults = Settings::compiled_defaults();
        let overrides = load_or_quarantine(&path)?;
        let resolved = Settings::resolve(&defaults, &overrides);
        if let Err(errs) = resolved.validate() {
            warn!(
                ?errs,
                "settings.ron failed validation; falling back to compile defaults"
            );
            quarantine(&path)?;
            let handle = Arc::new(ArcSwap::from_pointee(defaults.clone()));
            let store = Arc::new(Self {
                path,
                defaults,
                handle: handle.clone(),
                audit,
                write_lock: Mutex::new(()),
            });
            return Ok((store, handle));
        }
        let handle = Arc::new(ArcSwap::from_pointee(resolved));
        let store = Arc::new(Self {
            path,
            defaults,
            handle: handle.clone(),
            audit,
            write_lock: Mutex::new(()),
        });
        info!("settings store opened");
        Ok((store, handle))
    }

    pub fn handle(&self) -> &SettingsHandle {
        &self.handle
    }

    pub fn defaults(&self) -> &Settings {
        &self.defaults
    }

    pub async fn apply(
        &self,
        patch: SettingsOverrides,
        actor: Actor,
    ) -> Result<Settings, SettingsError> {
        let _g = self.write_lock.lock().await;
        let mut current = load_overrides_async(&self.path).await?.unwrap_or_default();
        let prior_resolved = Settings::resolve(&self.defaults, &current);
        merge_into(&mut current, &patch);
        let resolved = Settings::resolve(&self.defaults, &current);
        if let Err(errs) = resolved.validate() {
            return Err(SettingsError::Validation(errs));
        }
        crate::util::persist::atomic_save_ron_async(&current, &self.path).await?;
        self.handle.store(Arc::new(resolved.clone()));
        let changes = diff_changes(&prior_resolved, &resolved);
        if !changes.is_empty() {
            let entry = AuditEntry {
                ts: berlin_now(Utc::now()),
                actor_id: actor.user_id,
                actor_login: actor.user_login,
                changes,
            };
            if let Err(e) = self.audit.append(&entry) {
                error!(error = ?e, "audit append failed");
            }
        }
        Ok(resolved)
    }

    pub async fn reset(
        &self,
        section: SettingsSection,
        actor: Actor,
    ) -> Result<Settings, SettingsError> {
        let _g = self.write_lock.lock().await;
        let mut current = load_overrides_async(&self.path).await?.unwrap_or_default();
        let prior_resolved = Settings::resolve(&self.defaults, &current);
        match section {
            SettingsSection::Cooldowns => current.cooldowns = Default::default(),
            SettingsSection::Pings => current.pings = Default::default(),
        }
        let resolved = Settings::resolve(&self.defaults, &current);
        crate::util::persist::atomic_save_ron_async(&current, &self.path).await?;
        self.handle.store(Arc::new(resolved.clone()));
        let changes = diff_changes(&prior_resolved, &resolved);
        if !changes.is_empty() {
            let entry = AuditEntry {
                ts: berlin_now(Utc::now()),
                actor_id: actor.user_id,
                actor_login: actor.user_login,
                changes,
            };
            if let Err(e) = self.audit.append(&entry) {
                error!(error = ?e, "audit append failed");
            }
        }
        Ok(resolved)
    }
}

fn load_or_quarantine(path: &Path) -> Result<SettingsOverrides, SettingsError> {
    match load_overrides(path) {
        Ok(Some(o)) => Ok(o),
        Ok(None) => Ok(SettingsOverrides::default()),
        Err(e) => {
            warn!(error = ?e, "settings.ron is corrupt; quarantining");
            quarantine(path)?;
            Ok(SettingsOverrides::default())
        }
    }
}

fn load_overrides(path: &Path) -> Result<Option<SettingsOverrides>, SettingsError> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path)?;
    let parsed: SettingsOverrides = ron::from_str(&body)?;
    Ok(Some(parsed))
}

async fn load_overrides_async(path: &Path) -> Result<Option<SettingsOverrides>, SettingsError> {
    if !tokio::fs::try_exists(path).await? {
        return Ok(None);
    }
    let body = tokio::fs::read_to_string(path).await?;
    let parsed: SettingsOverrides = ron::from_str(&body)?;
    Ok(Some(parsed))
}

fn quarantine(path: &Path) -> Result<(), SettingsError> {
    if !path.exists() {
        return Ok(());
    }
    let ts = chrono::Utc::now().timestamp();
    let target = path.with_extension(format!("ron.quarantine-{ts}"));
    std::fs::rename(path, &target)?;
    warn!(target = ?target, "settings.ron quarantined");
    Ok(())
}

fn merge_into(into: &mut SettingsOverrides, patch: &SettingsOverrides) {
    if let Some(v) = patch.cooldowns.ai {
        into.cooldowns.ai = Some(v);
    }
    if let Some(v) = patch.cooldowns.news {
        into.cooldowns.news = Some(v);
    }
    if let Some(v) = patch.cooldowns.up {
        into.cooldowns.up = Some(v);
    }
    if let Some(v) = patch.cooldowns.feedback {
        into.cooldowns.feedback = Some(v);
    }
    if let Some(v) = patch.cooldowns.doener {
        into.cooldowns.doener = Some(v);
    }
    if let Some(v) = patch.pings.cooldown {
        into.pings.cooldown = Some(v);
    }
    if let Some(v) = patch.pings.public {
        into.pings.public = Some(v);
    }
}

fn diff_changes(prior: &Settings, next: &Settings) -> Vec<AuditChange> {
    let mut out = Vec::new();
    macro_rules! cmp_u64 {
        ($key:literal, $field:ident . $sub:ident) => {
            if prior.$field.$sub != next.$field.$sub {
                out.push(AuditChange {
                    key: $key.into(),
                    old: serde_json::Value::from(prior.$field.$sub),
                    new: serde_json::Value::from(next.$field.$sub),
                });
            }
        };
    }
    cmp_u64!("cooldowns.ai", cooldowns.ai);
    cmp_u64!("cooldowns.news", cooldowns.news);
    cmp_u64!("cooldowns.up", cooldowns.up);
    cmp_u64!("cooldowns.feedback", cooldowns.feedback);
    cmp_u64!("cooldowns.doener", cooldowns.doener);
    cmp_u64!("pings.cooldown", pings.cooldown);
    if prior.pings.public != next.pings.public {
        out.push(AuditChange {
            key: "pings.public".into(),
            old: serde_json::Value::from(prior.pings.public),
            new: serde_json::Value::from(next.pings.public),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::audit::MemoryAuditLog;
    use crate::settings::overrides::CooldownsOverrides;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<SettingsStore>,
        SettingsHandle,
        Arc<MemoryAuditLog>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = Arc::new(MemoryAuditLog::new());
        let (store, handle) =
            SettingsStore::open(dir.path(), log.clone()).expect("open empty store");
        (dir, store, handle, log)
    }

    #[tokio::test]
    async fn empty_dir_yields_compile_defaults() {
        let (_dir, _store, handle, _log) = fixture();
        assert_eq!(**handle.load(), Settings::compiled_defaults());
    }

    #[tokio::test]
    async fn apply_persists_writes_handle_and_audits() {
        let (_dir, store, handle, log) = fixture();
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        store.apply(patch, actor).await.expect("apply");
        assert_eq!(handle.load().cooldowns.ai, 15);
        // round-trip from disk
        let dropped_handle = SettingsStore::open(store.path.parent().unwrap(), log.clone())
            .expect("reopen")
            .1;
        assert_eq!(dropped_handle.load().cooldowns.ai, 15);
        let entries = log.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].changes.len(), 1);
        assert_eq!(entries[0].changes[0].key, "cooldowns.ai");
    }

    #[tokio::test]
    async fn apply_rejects_out_of_bound_with_validation_error() {
        let (_dir, store, _handle, _log) = fixture();
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(0),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        match store.apply(patch, actor).await {
            Err(SettingsError::Validation(errs)) => {
                assert_eq!(errs[0].field, "cooldowns.ai");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reset_clears_section_back_to_defaults() {
        let (_dir, store, handle, _log) = fixture();
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                news: Some(45),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        store.apply(patch, actor.clone()).await.expect("apply");
        assert_eq!(handle.load().cooldowns.ai, 15);
        store
            .reset(SettingsSection::Cooldowns, actor)
            .await
            .expect("reset");
        let s = handle.load();
        assert_eq!(s.cooldowns.ai, Settings::compiled_defaults().cooldowns.ai);
        assert_eq!(
            s.cooldowns.news,
            Settings::compiled_defaults().cooldowns.news
        );
    }

    #[tokio::test]
    async fn corrupt_ron_falls_back_to_defaults_and_quarantines() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(FILE_NAME), "not valid ron").expect("write garbage");
        let log = Arc::new(MemoryAuditLog::new());
        let (_store, handle) = SettingsStore::open(dir.path(), log).expect("open should not fail");
        assert_eq!(**handle.load(), Settings::compiled_defaults());
        // settings.ron has been renamed away
        assert!(!dir.path().join(FILE_NAME).exists());
        let quarantined = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("settings.ron.quarantine-")
            });
        assert!(quarantined, "quarantine file must exist");
    }
}
