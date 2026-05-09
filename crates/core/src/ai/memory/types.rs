//! Public types shared across the v2 memory module.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

/// Speaker / actor role. `Dreamer` is the ritual LLM, never a Twitch user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Role {
    Regular = 0,
    Moderator = 1,
    Broadcaster = 2,
    Dreamer = 3,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Regular => "regular",
            Role::Moderator => "moderator",
            Role::Broadcaster => "broadcaster",
            Role::Dreamer => "dreamer",
        }
    }
}

/// Identifies one file in the memory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    Soul,
    Lore,
    User { user_id: String },
    State { slug: String },
}

impl FileKind {
    pub fn relative_path(&self) -> PathBuf {
        match self {
            FileKind::Soul => PathBuf::from("SOUL.md"),
            FileKind::Lore => PathBuf::from("LORE.md"),
            FileKind::User { user_id } => PathBuf::from(format!("users/{user_id}.md")),
            FileKind::State { slug } => PathBuf::from(format!("state/{slug}.md")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    pub updated_at: DateTime<Utc>,
    /// Twitch login (lowercase username). Set on user files; absent on
    /// SOUL/LORE/state.
    pub username: Option<String>,
    /// Twitch display name (cased). Set on user files; absent on
    /// SOUL/LORE/state.
    pub display_name: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub kind: FileKind,
    pub frontmatter: Frontmatter,
    pub body: String,
}

/// Per-file byte caps and the global state-file count cap.
///
/// Byte caps apply to the *full emitted file* (frontmatter + body), so the
/// on-disk size never exceeds the configured limit. The cap is enforced
/// inside the per-path mutex in `MemoryStore::write` / `write_state`, so
/// concurrent writes to the same file cannot smuggle an over-cap body past
/// the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub soul_bytes: usize,
    pub lore_bytes: usize,
    pub user_bytes: usize,
    pub state_bytes: usize,
    /// Max number of `state/<slug>.md` files that may exist at once.
    /// Enforced inside `MemoryStore::write_state` under a global state-create
    /// lock so two concurrent new-slug writes can't both pass the cap check.
    pub max_state_files: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            soul_bytes: 4096,
            lore_bytes: 12_288,
            user_bytes: 4096,
            state_bytes: 2048,
            max_state_files: 16,
        }
    }
}

impl Caps {
    pub fn limit_for(&self, kind: &FileKind) -> usize {
        match kind {
            FileKind::Soul => self.soul_bytes,
            FileKind::Lore => self.lore_bytes,
            FileKind::User { .. } => self.user_bytes,
            FileKind::State { .. } => self.state_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_orderable_for_perm_table() {
        assert_eq!(Role::Regular as u8 + 1, Role::Moderator as u8);
        assert_eq!(Role::Moderator as u8 + 1, Role::Broadcaster as u8);
        assert_eq!(Role::Broadcaster as u8 + 1, Role::Dreamer as u8);
    }

    #[test]
    fn caps_default_matches_spec() {
        let c = Caps::default();
        assert_eq!(c.soul_bytes, 4096);
        assert_eq!(c.lore_bytes, 12_288);
        assert_eq!(c.user_bytes, 4096);
        assert_eq!(c.state_bytes, 2048);
        assert_eq!(c.max_state_files, 16);
    }

    #[test]
    fn file_kind_relative_path_round_trips() {
        assert_eq!(FileKind::Soul.relative_path().to_str().unwrap(), "SOUL.md");
        assert_eq!(FileKind::Lore.relative_path().to_str().unwrap(), "LORE.md");
        assert_eq!(
            FileKind::User {
                user_id: "12345".into()
            }
            .relative_path()
            .to_str()
            .unwrap(),
            "users/12345.md"
        );
        assert_eq!(
            FileKind::State {
                slug: "quiz".into()
            }
            .relative_path()
            .to_str()
            .unwrap(),
            "state/quiz.md"
        );
    }
}
