use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Outcome of an atomic "check + record" trigger attempt.
#[derive(Debug)]
pub enum TriggerDecision {
    /// Caller should silently do nothing: non-member, unknown ping, or no
    /// mentionable members after excluding the sender.
    Skip,
    /// Ping is still on cooldown; the remaining time is provided.
    OnCooldown(Duration),
    /// Ping should be sent with the rendered template. The trigger timestamp
    /// has already been recorded, so the caller just performs the send.
    Fire(String),
}

/// Reject control characters (CR/LF, NUL, etc.) that could split an IRC PRIVMSG
/// into two commands or break framing when the template is interpolated.
fn validate_template(template: &str) -> Result<()> {
    if template.chars().any(char::is_control) {
        bail!("Template darf keine Steuerzeichen (z.B. Zeilenumbrüche) enthalten");
    }
    Ok(())
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
            let data = std::fs::read_to_string(&path).wrap_err("Failed to read pings.ron")?;
            ron::from_str(&data).wrap_err("Failed to parse pings.ron")?
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
        std::fs::write(&tmp_path, &data).wrap_err("Failed to write pings.ron.tmp")?;
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
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("Ping-Name darf nur Buchstaben, Zahlen, - und _ enthalten");
        }
        validate_template(&template)?;
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

    /// Edit a ping's template. Errors if ping doesn't exist.
    pub fn edit_template(&mut self, name: &str, template: String) -> Result<()> {
        validate_template(&template)?;
        let ping = self
            .store
            .pings
            .get_mut(name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", name))?;
        ping.template = template;
        self.save()
    }

    /// Add a member to a ping. Errors if ping doesn't exist or user already a member.
    pub fn add_member(&mut self, ping_name: &str, username: &str) -> Result<()> {
        let ping = self
            .store
            .pings
            .get_mut(ping_name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", ping_name))?;
        let username_lower = username.to_lowercase();
        if !ping.members.insert(username_lower) {
            bail!("{} ist schon in \"{}\"", username, ping_name);
        }
        self.save()
    }

    /// Remove a member from a ping. Errors if ping doesn't exist or user not a member.
    pub fn remove_member(&mut self, ping_name: &str, username: &str) -> Result<()> {
        let ping = self
            .store
            .pings
            .get_mut(ping_name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", ping_name))?;
        let username_lower = username.to_lowercase();
        if !ping.members.remove(&username_lower) {
            bail!("{} ist nicht in \"{}\"", username, ping_name);
        }
        self.save()
    }

    /// Check if any ping matches the given name case-insensitively.
    /// Avoids heap allocation compared to `name.to_lowercase()` + `contains_key`.
    pub fn ping_exists_ignore_case(&self, name: &str) -> bool {
        self.store
            .pings
            .keys()
            .any(|k| k.eq_ignore_ascii_case(name))
    }

    /// Check if a user is a member of a ping.
    pub fn is_member(&self, ping_name: &str, username: &str) -> bool {
        self.store
            .pings
            .get(ping_name)
            .map(|p| p.members.contains(username))
            .unwrap_or(false)
    }

    /// List all ping names a user is subscribed to.
    pub fn list_pings_for_user(&self, username: &str) -> Vec<&str> {
        self.store
            .pings
            .iter()
            .filter(|(_, p)| p.members.contains(username))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Check if a ping is on cooldown. Returns `Some(remaining)` if on cooldown,
    /// `None` if it can be triggered (or ping doesn't exist).
    pub fn remaining_cooldown(
        &self,
        ping_name: &str,
        default_cooldown: Duration,
    ) -> Option<Duration> {
        let ping = self.store.pings.get(ping_name)?;
        let cooldown = ping.cooldown.map_or(default_cooldown, Duration::from_secs);
        match self.last_triggered.get(ping_name) {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed < cooldown {
                    Some(cooldown - elapsed)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    /// Record that a ping was triggered now.
    pub fn record_trigger(&mut self, ping_name: &str) {
        self.last_triggered
            .insert(ping_name.to_string(), Instant::now());
    }

    /// Atomically check membership + cooldown, render the template, and
    /// record the trigger timestamp — closing the window where two concurrent
    /// triggers both pass the cooldown check.
    ///
    /// `public = true` mirrors `[pings].public` and lets non-members fire.
    pub fn try_record_trigger(
        &mut self,
        ping_name: &str,
        sender: &str,
        default_cooldown: Duration,
        public: bool,
    ) -> TriggerDecision {
        if !public && !self.is_member(ping_name, sender) {
            return TriggerDecision::Skip;
        }
        if let Some(remaining) = self.remaining_cooldown(ping_name, default_cooldown) {
            return TriggerDecision::OnCooldown(remaining);
        }
        let Some(rendered) = self.render_template(ping_name, sender) else {
            return TriggerDecision::Skip;
        };
        self.record_trigger(ping_name);
        TriggerDecision::Fire(rendered)
    }

    /// Render a ping's template with placeholders replaced.
    /// Returns None if ping doesn't exist or has no mentionable members
    /// (i.e., all members are the sender).
    pub fn render_template(&self, ping_name: &str, sender: &str) -> Option<String> {
        let ping = self.store.pings.get(ping_name)?;
        let mentions = ping
            .members
            .iter()
            .filter(|m| m.as_str() != sender)
            .map(|m| format!("@{m}"))
            .collect::<Vec<_>>()
            .join(" ");
        if mentions.is_empty() {
            return None;
        }
        let rendered = ping
            .template
            .replace("{mentions}", &mentions)
            .replace("{sender}", sender);
        Some(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_manager(dir: &Path) -> PingManager {
        PingManager {
            store: PingStore {
                pings: HashMap::new(),
            },
            last_triggered: HashMap::new(),
            path: dir.join(PINGS_FILENAME),
        }
    }

    fn test_manager(dir: &Path) -> PingManager {
        let mut mgr = empty_manager(dir);
        mgr.create_ping(
            "test".into(),
            "Hey {mentions}!".into(),
            "admin".into(),
            None,
        )
        .unwrap();
        mgr
    }

    #[test]
    fn edit_template_updates_template() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());

        mgr.edit_template("test", "New template {mentions}".into())
            .unwrap();

        let ping = mgr.store.pings.get("test").unwrap();
        assert_eq!(ping.template, "New template {mentions}");
    }

    #[test]
    fn edit_template_preserves_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();
        mgr.add_member("test", "bob").unwrap();

        mgr.edit_template("test", "Updated {mentions}".into())
            .unwrap();

        let ping = mgr.store.pings.get("test").unwrap();
        assert!(ping.members.contains("alice"));
        assert!(ping.members.contains("bob"));
        assert_eq!(ping.members.len(), 2);
    }

    #[test]
    fn edit_template_nonexistent_ping_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());

        let result = mgr.edit_template("nope", "whatever".into());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("gibt es nicht"));
    }

    #[test]
    fn create_ping_rejects_newline_in_template() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = empty_manager(dir.path());

        let result = mgr.create_ping(
            "bad".into(),
            "Hey {mentions}\r\nPRIVMSG #other :pwned".into(),
            "admin".into(),
            None,
        );
        assert!(result.is_err());
        assert!(!mgr.store.pings.contains_key("bad"));
    }

    #[test]
    fn edit_template_rejects_control_chars() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());

        let result = mgr.edit_template("test", "Hey\x00injection".into());
        assert!(result.is_err());
        let ping = mgr.store.pings.get("test").unwrap();
        assert_eq!(ping.template, "Hey {mentions}!");
    }

    #[test]
    fn edit_template_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.edit_template("test", "Persisted {mentions}".into())
            .unwrap();

        // Reload from disk
        let mgr2 = PingManager::load(dir.path()).unwrap();
        let ping = mgr2.store.pings.get("test").unwrap();
        assert_eq!(ping.template, "Persisted {mentions}");
    }

    #[test]
    fn render_template_excludes_sender() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();
        mgr.add_member("test", "bob").unwrap();

        let result = mgr.render_template("test", "alice").unwrap();
        assert!(result.contains("@bob"), "should mention bob");
        assert!(
            !result.contains("@alice"),
            "should not mention sender alice"
        );
    }

    #[test]
    fn render_template_returns_none_when_sender_is_only_member() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();

        let result = mgr.render_template("test", "alice");
        assert!(
            result.is_none(),
            "should return None when only member is sender"
        );
    }

    #[test]
    fn render_template_excludes_sender_lowercase() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();
        mgr.add_member("test", "bob").unwrap();

        // Twitch IRC sender.login is always lowercase
        let result = mgr.render_template("test", "alice").unwrap();
        assert!(!result.contains("@alice"), "should exclude sender");
        assert!(result.contains("@bob"));
    }

    #[test]
    fn remaining_cooldown_returns_none_when_never_triggered() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();

        assert!(
            mgr.remaining_cooldown("test", Duration::from_secs(300))
                .is_none()
        );
    }

    #[test]
    fn remaining_cooldown_returns_some_when_on_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();
        mgr.record_trigger("test");

        let remaining = mgr.remaining_cooldown("test", Duration::from_secs(300));
        assert!(remaining.is_some());
        let secs = remaining.unwrap().as_secs();
        assert!(secs > 0 && secs <= 300, "expected 1..=300, got {secs}");
    }

    #[test]
    fn remaining_cooldown_returns_none_for_nonexistent_ping() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = empty_manager(dir.path());

        assert!(
            mgr.remaining_cooldown("nope", Duration::from_secs(300))
                .is_none()
        );
    }

    #[test]
    fn try_record_trigger_is_atomic_across_consecutive_calls() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();
        mgr.add_member("test", "bob").unwrap();

        // First call: should fire and record the trigger.
        let first = mgr.try_record_trigger("test", "bob", Duration::from_secs(300), false);
        match first {
            TriggerDecision::Fire(rendered) => {
                assert!(rendered.contains("@alice"));
                assert!(!rendered.contains("@bob"));
            }
            other => panic!("expected Fire on first call, got {other:?}"),
        }

        // Second immediate call: cooldown must already be in effect because
        // record_trigger ran under the same `&mut self` as the check.
        let second = mgr.try_record_trigger("test", "bob", Duration::from_secs(300), false);
        match second {
            TriggerDecision::OnCooldown(remaining) => {
                assert!(remaining.as_secs() <= 300);
            }
            other => panic!("expected OnCooldown on second call, got {other:?}"),
        }
    }

    #[test]
    fn try_record_trigger_respects_membership() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();

        // Non-member with public=false is rejected and must not consume the cooldown.
        let decision = mgr.try_record_trigger("test", "stranger", Duration::from_secs(300), false);
        assert!(matches!(decision, TriggerDecision::Skip));

        // Alice is the sole member, so render_template produces no mentions → Skip.
        // The important invariant: the previous rejection did not record a trigger,
        // so we don't get OnCooldown here.
        let decision = mgr.try_record_trigger("test", "alice", Duration::from_secs(300), false);
        assert!(matches!(decision, TriggerDecision::Skip));
    }

    #[test]
    fn try_record_trigger_public_allows_non_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path());
        mgr.add_member("test", "alice").unwrap();

        let decision = mgr.try_record_trigger("test", "stranger", Duration::from_secs(300), true);
        assert!(
            matches!(decision, TriggerDecision::Fire(_)),
            "public=true should allow non-members to fire, got {decision:?}"
        );
    }
}
