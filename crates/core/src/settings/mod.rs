//! Dashboard-managed runtime settings.
//!
//! `Settings` is the fully-resolved snapshot read by command handlers via
//! a `SettingsHandle = Arc<ArcSwap<Settings>>`. Sparse `SettingsOverrides`
//! (see `overrides.rs`) live on disk at `$DATA_DIR/settings.ron`; missing
//! fields fall through to `compiled_defaults()`.
//!
//! Writes go through `SettingsStore::apply` (see `store.rs`) which
//! validates, atomically persists, swaps the handle, and appends an audit
//! entry.

pub mod audit;
pub mod overrides;
pub mod store;

#[cfg(any(test, feature = "testing"))]
pub use audit::MemoryAuditLog;
pub use audit::{AuditChange, AuditEntry, AuditError, AuditLog, FileAuditLog};
pub use overrides::{CooldownsOverrides, PingsOverrides, SettingsOverrides};
pub use store::{Actor, SettingsStore};

use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type SettingsHandle = Arc<ArcSwap<Settings>>;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub schema_version: u32,
    pub cooldowns: Cooldowns,
    pub pings: PingsSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cooldowns {
    pub ai: u64,
    pub news: u64,
    pub up: u64,
    pub feedback: u64,
    pub doener: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingsSettings {
    pub cooldown: u64,
    pub public: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Cooldowns,
    Pings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("validation failed")]
    Validation(Vec<FieldError>),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ron: {0}")]
    Ron(#[from] ron::error::SpannedError),
    #[error("persist: {0}")]
    Persist(#[from] crate::util::persist::AtomicPersistError),
}

impl From<Vec<FieldError>> for SettingsError {
    fn from(errs: Vec<FieldError>) -> Self {
        Self::Validation(errs)
    }
}

impl Settings {
    pub const fn compiled_defaults() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            cooldowns: Cooldowns {
                ai: 30,
                news: 60,
                up: 30,
                feedback: 300,
                doener: 30,
            },
            pings: PingsSettings {
                cooldown: 300,
                public: false,
            },
        }
    }

    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errs = Vec::new();
        fn bound(name: &str, v: u64, lo: u64, hi: u64, errs: &mut Vec<FieldError>) {
            if v < lo || v > hi {
                errs.push(FieldError {
                    field: name.to_owned(),
                    message: format!("must be {lo}..={hi} seconds (got {v})"),
                });
            }
        }
        bound("cooldowns.ai", self.cooldowns.ai, 1, 3600, &mut errs);
        bound("cooldowns.news", self.cooldowns.news, 1, 3600, &mut errs);
        bound("cooldowns.up", self.cooldowns.up, 1, 3600, &mut errs);
        bound(
            "cooldowns.feedback",
            self.cooldowns.feedback,
            1,
            3600,
            &mut errs,
        );
        bound(
            "cooldowns.doener",
            self.cooldowns.doener,
            1,
            3600,
            &mut errs,
        );
        bound("pings.cooldown", self.pings.cooldown, 1, 86_400, &mut errs);
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    pub fn resolve(defaults: &Settings, overrides: &overrides::SettingsOverrides) -> Settings {
        Settings {
            schema_version: SCHEMA_VERSION,
            cooldowns: Cooldowns {
                ai: overrides.cooldowns.ai.unwrap_or(defaults.cooldowns.ai),
                news: overrides.cooldowns.news.unwrap_or(defaults.cooldowns.news),
                up: overrides.cooldowns.up.unwrap_or(defaults.cooldowns.up),
                feedback: overrides
                    .cooldowns
                    .feedback
                    .unwrap_or(defaults.cooldowns.feedback),
                doener: overrides
                    .cooldowns
                    .doener
                    .unwrap_or(defaults.cooldowns.doener),
            },
            pings: PingsSettings {
                cooldown: overrides.pings.cooldown.unwrap_or(defaults.pings.cooldown),
                public: overrides.pings.public.unwrap_or(defaults.pings.public),
            },
        }
    }
}

#[cfg(any(test, feature = "testing"))]
pub fn test_handle() -> SettingsHandle {
    Arc::new(ArcSwap::from_pointee(Settings::compiled_defaults()))
}

#[cfg(test)]
mod resolve_tests {
    use super::overrides::{CooldownsOverrides, PingsOverrides, SettingsOverrides};
    use super::*;

    #[test]
    fn empty_overrides_equal_defaults() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides::default();
        assert_eq!(Settings::resolve(&defaults, &overrides), defaults);
    }

    #[test]
    fn cooldown_override_wins_per_field() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            schema_version: SCHEMA_VERSION,
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                ..Default::default()
            },
            pings: PingsOverrides::default(),
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert_eq!(resolved.cooldowns.ai, 15);
        assert_eq!(resolved.cooldowns.news, defaults.cooldowns.news);
        assert_eq!(resolved.pings, defaults.pings);
    }

    #[test]
    fn pings_public_override_flips_bool() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            pings: PingsOverrides {
                public: Some(true),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert!(resolved.pings.public);
        assert_eq!(resolved.pings.cooldown, defaults.pings.cooldown);
    }

    #[test]
    fn pings_cooldown_override_leaves_public_at_default() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            pings: PingsOverrides {
                cooldown: Some(600),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert_eq!(resolved.pings.cooldown, 600);
        assert_eq!(resolved.pings.public, defaults.pings.public);
    }

    #[test]
    fn validate_collects_multiple_errors() {
        let mut s = Settings::compiled_defaults();
        s.cooldowns.ai = 0;
        s.pings.cooldown = 0;
        let errs = s.validate().expect_err("both bounds violated");
        let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"cooldowns.ai"));
        assert!(fields.contains(&"pings.cooldown"));
    }

    #[test]
    fn validate_accepts_compiled_defaults() {
        Settings::compiled_defaults()
            .validate()
            .expect("compiled defaults pass validate()");
    }
}
