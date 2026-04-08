use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use eyre::{Result, WrapErr, bail};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

const PINGS_FILENAME: &str = "pings.ron";

/// A single ping definition. The ping's name is the HashMap key in PingStore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ping {
    pub template: String,
    pub members: HashSet<String>,
    pub cooldown: Option<u64>,
    pub created_by: String,
}

/// Top-level container serialized to/from pings.ron.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingStore {
    pub pings: HashMap<String, Ping>,
}

/// Manages ping state and persistence.
pub struct PingManager {
    store: PingStore,
    last_triggered: HashMap<String, Instant>,
    path: PathBuf,
}

impl PingManager {
    /// Load pings from disk. Creates empty store if file doesn't exist.
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(PINGS_FILENAME);
        let store = if path.exists() {
            let data = std::fs::read_to_string(&path)
                .wrap_err("Failed to read pings.ron")?;
            ron::from_str(&data)
                .wrap_err("Failed to parse pings.ron")?
        } else {
            info!("No pings.ron found, starting with empty ping store");
            PingStore {
                pings: HashMap::new(),
            }
        };

        info!(count = store.pings.len(), "Loaded pings");

        Ok(Self {
            store,
            last_triggered: HashMap::new(),
            path,
        })
    }

    /// Write current state to disk using write+rename for atomicity.
    fn save(&self) -> Result<()> {
        let tmp_path = self.path.with_extension("ron.tmp");
        let data = ron::ser::to_string_pretty(&self.store, ron::ser::PrettyConfig::default())
            .wrap_err("Failed to serialize pings")?;
        std::fs::write(&tmp_path, &data)
            .wrap_err("Failed to write pings.ron.tmp")?;
        std::fs::rename(&tmp_path, &self.path)
            .wrap_err("Failed to rename pings.ron.tmp to pings.ron")?;
        debug!("Saved pings to disk");
        Ok(())
    }

    /// Create a new ping. Errors if name is invalid or already exists.
    pub fn create_ping(
        &mut self,
        name: String,
        template: String,
        created_by: String,
        cooldown: Option<u64>,
    ) -> Result<()> {
        if name.is_empty() {
            bail!("Ping-Name darf nicht leer sein");
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("Ping-Name darf nur Buchstaben, Zahlen, - und _ enthalten");
        }
        if self.store.pings.contains_key(&name) {
            bail!("Ping \"{}\" gibt es schon", name);
        }
        self.store.pings.insert(
            name,
            Ping {
                template,
                members: HashSet::new(),
                cooldown,
                created_by,
            },
        );
        self.save()
    }

    /// Delete a ping. Errors if it doesn't exist.
    pub fn delete_ping(&mut self, name: &str) -> Result<()> {
        if self.store.pings.remove(name).is_none() {
            bail!("Ping \"{}\" gibt es nicht", name);
        }
        self.last_triggered.remove(name);
        self.save()
    }

    /// Add a member to a ping. Errors if ping doesn't exist or user already a member.
    pub fn add_member(&mut self, ping_name: &str, username: &str) -> Result<()> {
        let ping = self.store.pings.get_mut(ping_name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", ping_name))?;
        let username_lower = username.to_lowercase();
        if !ping.members.insert(username_lower) {
            bail!("{} ist schon in \"{}\"", username, ping_name);
        }
        self.save()
    }

    /// Remove a member from a ping. Errors if ping doesn't exist or user not a member.
    pub fn remove_member(&mut self, ping_name: &str, username: &str) -> Result<()> {
        let ping = self.store.pings.get_mut(ping_name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", ping_name))?;
        let username_lower = username.to_lowercase();
        if !ping.members.remove(&username_lower) {
            bail!("{} ist nicht in \"{}\"", username, ping_name);
        }
        self.save()
    }

    /// Check if a ping exists.
    pub fn ping_exists(&self, name: &str) -> bool {
        self.store.pings.contains_key(name)
    }

    /// Check if a user is a member of a ping.
    pub fn is_member(&self, ping_name: &str, username: &str) -> bool {
        let username_lower = username.to_lowercase();
        self.store.pings.get(ping_name)
            .map(|p| p.members.contains(&username_lower))
            .unwrap_or(false)
    }

    /// List all ping names a user is subscribed to.
    pub fn list_pings_for_user(&self, username: &str) -> Vec<&str> {
        let username_lower = username.to_lowercase();
        self.store.pings.iter()
            .filter(|(_, p)| p.members.contains(&username_lower))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Check if a ping is off cooldown. Returns true if it can be triggered.
    pub fn check_cooldown(&self, ping_name: &str, default_cooldown: u64) -> bool {
        let ping = match self.store.pings.get(ping_name) {
            Some(p) => p,
            None => return false,
        };
        let cooldown_secs = ping.cooldown.unwrap_or(default_cooldown);
        match self.last_triggered.get(ping_name) {
            Some(last) => last.elapsed().as_secs() >= cooldown_secs,
            None => true,
        }
    }

    /// Record that a ping was triggered now.
    pub fn record_trigger(&mut self, ping_name: &str) {
        self.last_triggered.insert(ping_name.to_string(), Instant::now());
    }

    /// Render a ping's template with placeholders replaced.
    /// Returns None if ping doesn't exist or has no members.
    pub fn render_template(&self, ping_name: &str, sender: &str) -> Option<String> {
        let ping = self.store.pings.get(ping_name)?;
        if ping.members.is_empty() {
            return None;
        }
        let mentions = ping.members.iter()
            .map(|m| format!("@{m}"))
            .collect::<Vec<_>>()
            .join(" ");
        let rendered = ping.template
            .replace("{mentions}", &mentions)
            .replace("{sender}", sender);
        Some(rendered)
    }

}
