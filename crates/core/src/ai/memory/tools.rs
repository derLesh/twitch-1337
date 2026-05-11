//! Tool args + definitions for the v2 chat-turn and dreamer loops.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use llm::{ToolCall, ToolDefinition, ToolExecutor, ToolResultMessage};

use crate::ai::memory::sanitize::{
    PathError, SlugError, WritePath, check_body, has_trailing_iso_date, parse_slug,
    parse_write_path, write_path_to_kind,
};
use crate::ai::memory::store::{MemoryStore, WriteError};
use crate::ai::memory::types::{FileKind, Role};

// ---------------------------------------------------------------------------
// Arg structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteFileArgs {
    pub path: String,
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteStateArgs {
    pub slug: String,
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteStateArgs {
    pub slug: String,
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

pub fn chat_turn_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::derived::<WriteFileArgs>(
            "write_file",
            "Overwrite a memory file (SOUL.md, LORE.md, or users/<id>.md). Body is the new full prose body; frontmatter is store-managed. Permission-gated by speaker role.",
        ),
        ToolDefinition::derived::<WriteStateArgs>(
            "write_state",
            "Create or overwrite a state file at state/<slug>.md. slug is lowercase a–z 0–9 dashes, ≤64 chars.",
        ),
        ToolDefinition::derived::<DeleteStateArgs>(
            "delete_state",
            "Remove a state file. Regulars may only delete state files they created.",
        ),
    ]
}

pub fn dreamer_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::derived::<WriteFileArgs>(
            "write_file",
            "Overwrite SOUL.md / LORE.md / users/<id>.md.",
        ),
        ToolDefinition::derived::<WriteStateArgs>("write_state", "Overwrite state/<slug>.md."),
        ToolDefinition::derived::<DeleteStateArgs>("delete_state", "Remove a stale state file."),
    ]
}

// ---------------------------------------------------------------------------
// ChatTurnExecutor
// ---------------------------------------------------------------------------

pub struct ChatTurnExecutorOpts {
    pub store: MemoryStore,
    pub speaker_user_id: String,
    pub speaker_login: String,
    pub speaker_display_name: String,
    pub speaker_role: Role,
    /// Maximum write-class tool calls (write_file, write_state, delete_state)
    /// per turn.
    pub max_writes_per_turn: usize,
}

pub struct ChatTurnExecutor {
    opts: ChatTurnExecutorOpts,
    write_count: AtomicUsize,
}

impl ChatTurnExecutor {
    pub fn new(opts: ChatTurnExecutorOpts) -> Self {
        Self {
            opts,
            write_count: AtomicUsize::new(0),
        }
    }

    fn role(&self) -> Role {
        self.opts.speaker_role
    }

    /// Returns `true` if the speaker is allowed to write to `path`.
    fn permitted_write_path(&self, path: &WritePath) -> bool {
        match (path, self.role()) {
            // Only Dreamer may write SOUL.md
            (WritePath::Soul, Role::Dreamer) => true,
            (WritePath::Soul, _) => false,
            // Moderator, Broadcaster, Dreamer may write LORE.md; Regular may not
            (WritePath::Lore, Role::Regular) => false,
            (WritePath::Lore, _) => true,
            // Regular may only write their own user file
            (WritePath::User { user_id }, Role::Regular) => user_id == &self.opts.speaker_user_id,
            // Higher roles can write any user file
            (WritePath::User { .. }, _) => true,
        }
    }

    /// Attempt to consume one write from the per-turn quota.
    /// Returns `true` if the write is allowed, `false` when the quota is
    /// already exhausted.
    fn try_consume_write_quota(&self) -> bool {
        // fetch_add returns the *previous* value before the increment.
        let used = self.write_count.fetch_add(1, Ordering::SeqCst);
        if used >= self.opts.max_writes_per_turn {
            // Undo the spurious increment so the counter doesn't overflow on
            // repeated exhausted calls.
            self.write_count
                .store(self.opts.max_writes_per_turn, Ordering::SeqCst);
            false
        } else {
            true
        }
    }

    async fn handle_write_file(&self, call: &ToolCall) -> String {
        let args: WriteFileArgs = match call.parse_args() {
            Ok(a) => a,
            Err(_) => return "invalid_arguments".into(),
        };
        let path = match parse_write_path(&args.path) {
            Ok(p) => p,
            Err(PathError) => return "invalid_path".into(),
        };
        if !self.permitted_write_path(&path) {
            return "permission_denied".into();
        }
        if check_body(&args.body).is_err() {
            return "invalid_body".into();
        }
        if !self.try_consume_write_quota() {
            return "write_quota_exhausted".into();
        }

        let kind = write_path_to_kind(path);
        // Pass identity only for the speaker's own user file. For other user
        // files, `None` lets the store preserve whatever's already on disk.
        let (login, display) = if matches!(&kind, FileKind::User { user_id } if user_id == &self.opts.speaker_user_id)
        {
            (
                Some(self.opts.speaker_login.as_str()),
                Some(self.opts.speaker_display_name.as_str()),
            )
        } else {
            (None, None)
        };
        match self
            .opts
            .store
            .write(&kind, &args.body, login, display)
            .await
        {
            Ok(()) => "ok".into(),
            Err(WriteError::Full) => "file_full".into(),
            Err(WriteError::StateFull) => "state_full".into(), // unreachable for write()
            Err(WriteError::InvalidSlug) => "invalid_slug".into(), // unreachable for non-state write
            Err(WriteError::Io(e)) => format!("io_error: {e}"),
        }
    }

    async fn handle_write_state(&self, call: &ToolCall) -> String {
        let args: WriteStateArgs = match call.parse_args() {
            Ok(a) => a,
            Err(_) => return "invalid_arguments".into(),
        };
        let slug = match parse_slug(&args.slug) {
            Ok(s) => s,
            Err(SlugError::Invalid) => return "invalid_slug".into(),
            Err(SlugError::Reserved) => return "reserved_slug".into(),
        };
        if has_trailing_iso_date(&slug) {
            return "dated_slug".into();
        }
        if check_body(&args.body).is_err() {
            return "invalid_body".into();
        }
        if !self.try_consume_write_quota() {
            return "write_quota_exhausted".into();
        }

        match self
            .opts
            .store
            .write_state(
                &FileKind::State { slug },
                &args.body,
                Some(self.opts.speaker_user_id.as_str()),
            )
            .await
        {
            Ok(()) => "ok".into(),
            Err(WriteError::Full) => "file_full".into(),
            Err(WriteError::StateFull) => "state_full".into(),
            Err(WriteError::InvalidSlug) => "invalid_slug".into(),
            Err(WriteError::Io(e)) => format!("io_error: {e}"),
        }
    }

    async fn handle_delete_state(&self, call: &ToolCall) -> String {
        let args: DeleteStateArgs = match call.parse_args() {
            Ok(a) => a,
            Err(_) => return "invalid_arguments".into(),
        };
        let slug = match parse_slug(&args.slug) {
            Ok(s) => s,
            Err(SlugError::Invalid) => return "invalid_slug".into(),
            Err(SlugError::Reserved) => return "reserved_slug".into(),
        };

        // Check ownership before consuming quota.
        let owner = self
            .opts
            .store
            .read_kind(&FileKind::State { slug: slug.clone() })
            .await
            .ok()
            .and_then(|f| f.frontmatter.created_by);
        let allowed = match self.role() {
            Role::Regular => owner.as_deref() == Some(self.opts.speaker_user_id.as_str()),
            _ => true,
        };
        if !allowed {
            return "permission_denied".into();
        }

        if !self.try_consume_write_quota() {
            return "write_quota_exhausted".into();
        }

        match self.opts.store.delete_state(&slug).await {
            Ok(()) => "ok".into(),
            Err(e) => format!("io_error: {e}"),
        }
    }
}

#[async_trait]
impl ToolExecutor for ChatTurnExecutor {
    async fn execute(&self, call: &ToolCall) -> ToolResultMessage {
        let content = match call.name.as_str() {
            "write_file" => self.handle_write_file(call).await,
            "write_state" => self.handle_write_state(call).await,
            "delete_state" => self.handle_delete_state(call).await,
            _ => "unknown_tool".to_string(),
        };
        ToolResultMessage::for_call(call, content)
    }
}

// ---------------------------------------------------------------------------
// DreamerExecutor
// ---------------------------------------------------------------------------

pub struct DreamerExecutorOpts {
    pub store: MemoryStore,
    pub max_writes_per_turn: usize,
}

pub struct DreamerExecutor {
    inner: ChatTurnExecutor,
}

impl DreamerExecutor {
    pub fn new(opts: DreamerExecutorOpts) -> Self {
        Self {
            inner: ChatTurnExecutor::new(ChatTurnExecutorOpts {
                store: opts.store,
                speaker_user_id: "dreamer".into(),
                speaker_login: String::new(),
                speaker_display_name: String::new(),
                speaker_role: Role::Dreamer,
                max_writes_per_turn: opts.max_writes_per_turn,
            }),
        }
    }
}

#[async_trait]
impl ToolExecutor for DreamerExecutor {
    async fn execute(&self, call: &ToolCall) -> ToolResultMessage {
        self.inner.execute(call).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn chat_turn_tools_has_three_named_tools() {
        let names: Vec<_> = chat_turn_tools().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["write_file", "write_state", "delete_state"]);
    }

    #[test]
    fn dreamer_tools_matches_chat_turn_tools() {
        let names: Vec<_> = dreamer_tools().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["write_file", "write_state", "delete_state"]);
    }

    #[test]
    fn write_file_args_round_trip() {
        let v = serde_json::json!({"path": "users/12.md", "body": "hi"});
        let parsed: WriteFileArgs = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.path, "users/12.md");
        assert_eq!(parsed.body, "hi");
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;
    use crate::ai::memory::store::MemoryStore;
    use crate::ai::memory::types::Caps;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: args,
            arguments_parse_error: None,
        }
    }

    async fn make_executor(role: Role, store: MemoryStore, max_writes: usize) -> ChatTurnExecutor {
        ChatTurnExecutor::new(ChatTurnExecutorOpts {
            store,
            speaker_user_id: "12345".into(),
            speaker_login: "alice".into(),
            speaker_display_name: "Alice".into(),
            speaker_role: role,
            max_writes_per_turn: max_writes,
        })
    }

    #[tokio::test]
    async fn regular_writes_own_user_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        let r = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "users/12345.md", "body": "hi"}),
            ))
            .await;
        assert_eq!(r.content, "ok");
    }

    #[tokio::test]
    async fn regular_cannot_write_other_user_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        let r = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "users/99.md", "body": "hi"}),
            ))
            .await;
        assert_eq!(r.content, "permission_denied");
    }

    #[tokio::test]
    async fn regular_cannot_write_lore_or_soul() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        for path in ["LORE.md", "SOUL.md"] {
            let r = exec
                .execute(&call(
                    "write_file",
                    serde_json::json!({"path": path, "body": "x"}),
                ))
                .await;
            assert_eq!(r.content, "permission_denied", "regular wrote {path}");
        }
    }

    #[tokio::test]
    async fn moderator_can_write_lore_but_not_soul() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Moderator, store.clone(), 8).await;
        let r = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "LORE.md", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "ok");
        let r = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "SOUL.md", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "permission_denied");
    }

    #[tokio::test]
    async fn invalid_path_and_invalid_slug_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        let r = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "../etc/passwd", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "invalid_path");
        let r = exec
            .execute(&call(
                "write_state",
                serde_json::json!({"slug": "Foo", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "invalid_slug");
    }

    #[tokio::test]
    async fn dated_slug_is_blocked_on_write_but_delete_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;

        // write rejects trailing -YYYY-MM-DD slugs so dated-suffix accumulation
        // cannot recur via fresh writes.
        let r = exec
            .execute(&call(
                "write_state",
                serde_json::json!({"slug": "av-depot-2026-05-08", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "dated_slug");

        // delete still accepts dated slugs so the dreamer can prune the backlog.
        store
            .write_state(
                &FileKind::State {
                    slug: "legacy-2026-05-08".into(),
                },
                "x",
                Some("12345"),
            )
            .await
            .unwrap();
        let r = exec
            .execute(&call(
                "delete_state",
                serde_json::json!({"slug": "legacy-2026-05-08"}),
            ))
            .await;
        assert_eq!(r.content, "ok");
    }

    #[tokio::test]
    async fn reserved_slug_is_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        let r = exec
            .execute(&call(
                "write_state",
                serde_json::json!({"slug": "system", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "reserved_slug");
    }

    #[tokio::test]
    async fn state_full_when_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let caps = Caps {
            max_state_files: 1,
            ..Caps::default()
        };
        let store = MemoryStore::open(dir.path(), caps).await.unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        let r = exec
            .execute(&call(
                "write_state",
                serde_json::json!({"slug": "first", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "ok");
        let r = exec
            .execute(&call(
                "write_state",
                serde_json::json!({"slug": "second", "body": "x"}),
            ))
            .await;
        assert_eq!(r.content, "state_full");
    }

    #[tokio::test]
    async fn write_quota_exhausts_after_n_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 1).await;
        let r1 = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "users/12345.md", "body": "a"}),
            ))
            .await;
        assert_eq!(r1.content, "ok");
        let r2 = exec
            .execute(&call(
                "write_file",
                serde_json::json!({"path": "users/12345.md", "body": "b"}),
            ))
            .await;
        assert_eq!(r2.content, "write_quota_exhausted");
    }

    #[tokio::test]
    async fn delete_state_blocked_for_non_creator_regular() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        // Pre-seed state file owned by user 99.
        store
            .write_state(
                &crate::ai::memory::types::FileKind::State { slug: "qz".into() },
                "x",
                Some("99"),
            )
            .await
            .unwrap();
        let exec = make_executor(Role::Regular, store.clone(), 8).await;
        let r = exec
            .execute(&call("delete_state", serde_json::json!({"slug": "qz"})))
            .await;
        assert_eq!(r.content, "permission_denied");
    }

    #[tokio::test]
    async fn dreamer_can_write_soul_lore_any_user() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        let exec = DreamerExecutor::new(DreamerExecutorOpts {
            store: store.clone(),
            max_writes_per_turn: 32,
        });
        for path in ["SOUL.md", "LORE.md", "users/77.md"] {
            let r = exec
                .execute(&call(
                    "write_file",
                    serde_json::json!({"path": path, "body": "x"}),
                ))
                .await;
            assert_eq!(r.content, "ok", "dreamer write {path}");
        }
    }
}
