use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

const MEMORY_FILENAME: &str = "ai_memory.ron";

/// A single remembered fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub fact: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Persistent store of AI memories, serialized to RON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStore {
    pub memories: HashMap<String, Memory>,
}

impl MemoryStore {
    /// Load from disk. Returns empty store if file doesn't exist.
    pub fn load(data_dir: &Path) -> Result<(Self, PathBuf)> {
        let path = data_dir.join(MEMORY_FILENAME);
        let store = if path.exists() {
            let data =
                std::fs::read_to_string(&path).wrap_err("Failed to read ai_memory.ron")?;
            ron::from_str(&data).wrap_err("Failed to parse ai_memory.ron")?
        } else {
            info!("No ai_memory.ron found, starting with empty memory store");
            Self {
                memories: HashMap::new(),
            }
        };

        info!(count = store.memories.len(), "Loaded AI memories");
        Ok((store, path))
    }

    /// Write current state to disk using write+rename for atomicity.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp_path = path.with_extension("ron.tmp");
        let data = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .wrap_err("Failed to serialize AI memories")?;
        std::fs::write(&tmp_path, &data).wrap_err("Failed to write ai_memory.ron.tmp")?;
        std::fs::rename(&tmp_path, path)
            .wrap_err("Failed to rename ai_memory.ron.tmp to ai_memory.ron")?;
        debug!("Saved AI memories to disk");
        Ok(())
    }

    /// Format memories for injection into the system prompt.
    /// Returns None if there are no memories.
    pub fn format_for_prompt(&self) -> Option<String> {
        if self.memories.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = self
            .memories
            .iter()
            .map(|(key, mem)| format!("- {}: {}", key, mem.fact))
            .collect();
        lines.sort(); // Deterministic ordering
        Some(format!("\n\n## Known facts\n{}", lines.join("\n")))
    }

    /// Format memories for the extraction prompt (key: fact list).
    pub fn format_for_extraction(&self) -> String {
        if self.memories.is_empty() {
            return "(none)".to_string();
        }
        let mut lines: Vec<String> = self
            .memories
            .iter()
            .map(|(key, mem)| format!("- {}: {}", key, mem.fact))
            .collect();
        lines.sort();
        lines.join("\n")
    }
}
