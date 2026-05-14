//! Sparse override types written to `$DATA_DIR/settings.ron`.
//!
//! Every field is `Option`; `Some` wins on resolve, `None` falls through
//! to `Settings::compiled_defaults()`. The sparse shape removes "what does
//! an empty value mean" ambiguity and lets the dashboard's "reset" button
//! clear individual sections without inventing a sentinel.

use serde::{Deserialize, Serialize};

use super::SCHEMA_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsOverrides {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub cooldowns: CooldownsOverrides,
    #[serde(default)]
    pub pings: PingsOverrides,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for SettingsOverrides {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            cooldowns: CooldownsOverrides::default(),
            pings: PingsOverrides::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CooldownsOverrides {
    #[serde(default)]
    pub ai: Option<u64>,
    #[serde(default)]
    pub news: Option<u64>,
    #[serde(default)]
    pub up: Option<u64>,
    #[serde(default)]
    pub feedback: Option<u64>,
    #[serde(default)]
    pub doener: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingsOverrides {
    #[serde(default)]
    pub cooldown: Option<u64>,
    #[serde(default)]
    pub public: Option<bool>,
}
