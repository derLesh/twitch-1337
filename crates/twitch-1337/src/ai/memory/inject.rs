//! Prompt composition: nonce-fenced inject of every memory + state file body,
//! plus prompt-file substitution.

use std::sync::Arc;

use chrono_tz::Europe::Berlin;
use eyre::Result;
use rand::Rng as _;
use tokio::sync::Mutex;

use crate::ai::chat_history::{ChatHistoryBuffer, ChatHistoryEntry};
use crate::ai::memory::store::MemoryStore;
use crate::ai::memory::types::{FileKind, MemoryFile};

const FENCE_OPEN: &str = "<<<FILE";
const FENCE_CLOSE: &str = "<<<ENDFILE";

/// Identifies what a fenced inject block represents. Renders into the FILE
/// header attrs so the model can map a block to its subject without parsing
/// a path. Path-style addressing only re-appears in the `write_file` tool's
/// `path` argument, where it's the canonical way to address a write target.
#[derive(Debug, Clone)]
pub enum FenceLabel<'a> {
    Soul,
    Lore,
    User {
        id: &'a str,
        login: Option<&'a str>,
        display_name: Option<&'a str>,
    },
    State {
        slug: &'a str,
    },
    Transcript {
        date: &'a str,
    },
}

/// Per-section byte caps for rolling chat injected into the v2 prompt.
/// Independent of `inject_byte_budget`, which covers SOUL/LORE/users/state.
pub const RECENT_CHAT_PRIMARY_BYTES: usize = 2048;
pub const RECENT_CHAT_AI_CHANNEL_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationChannel {
    Primary,
    AiChannel,
}

/// Generate a 16-hex-char nonce for one prompt build.
pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn fence_block(label: FenceLabel<'_>, nonce: &str, body: &str) -> String {
    let safe = scrub_for_inject(body);
    let attrs = render_label_attrs(&label);
    format!("<<<FILE {attrs} nonce={nonce}>>>\n{safe}\n<<<ENDFILE nonce={nonce}>>>")
}

fn render_label_attrs(label: &FenceLabel<'_>) -> String {
    match label {
        FenceLabel::Soul => "kind=soul".into(),
        FenceLabel::Lore => "kind=lore".into(),
        FenceLabel::State { slug } => format!("kind=state slug={slug}"),
        FenceLabel::Transcript { date } => format!("kind=transcript date={date}"),
        FenceLabel::User {
            id,
            login,
            display_name,
        } => {
            let mut s = format!("kind=user id={id}");
            if let Some(l) = login.filter(|l| !l.is_empty()) {
                s.push_str(&format!(" login={l}"));
            }
            if let Some(n) = display_name.filter(|n| !n.is_empty()) {
                s.push_str(&format!(" name=\"{}\"", sanitize_attr_value(n)));
            }
            s
        }
    }
}

/// Strip characters that would break the FILE header (quotes, newlines, `>`).
/// Display names are already control-stripped at write time, but we apply
/// the same hardening at render time to keep the marker syntactically clean.
fn sanitize_attr_value(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '>' && *c != '<')
        .collect()
}

/// If a body contains either fence sentinel, replace it wholesale.
pub fn scrub_for_inject(body: &str) -> String {
    if body.contains(FENCE_OPEN) || body.contains(FENCE_CLOSE) {
        tracing::error!("memory body contained fence sentinel at inject time, scrubbed");
        return "<corrupt: rejected>".to_string();
    }
    body.to_string()
}

#[derive(Clone, Copy)]
pub struct SubstitutionVars<'a> {
    pub speaker_username: &'a str,
    pub speaker_role: &'a str,
    pub channel: &'a str,
    pub date: &'a str,
}

pub fn substitute(template: &str, v: SubstitutionVars<'_>) -> String {
    template
        .replace("{speaker_username}", v.speaker_username)
        .replace("{speaker_role}", v.speaker_role)
        .replace("{channel}", v.channel)
        .replace("{date}", v.date)
}

pub struct BuildOpts {
    pub inject_byte_budget: usize,
    pub nonce: String,
    pub primary_history: Option<Arc<Mutex<ChatHistoryBuffer>>>,
    pub primary_login: String,
    pub ai_channel_history: Option<Arc<Mutex<ChatHistoryBuffer>>>,
    pub ai_channel_login: Option<String>,
    pub invocation_channel: InvocationChannel,
}

/// Result of [`build_chat_turn_context`]. `recent_chat` holds the per-turn rolling
/// chat sections (volatile, belongs in the user message for cache hygiene);
/// `memory` holds SOUL/LORE/users/state nonce-fenced blocks (less volatile,
/// belongs in the system message). Either may be empty.
pub struct ChatTurnContext {
    pub recent_chat: String,
    pub memory: String,
}

/// Build the chat-turn injected context split into a recent-chat section and a
/// memory section. Callers place each in the message that maximizes prompt
/// caching: memory in the system message, recent chat in the user message.
pub async fn build_chat_turn_context(
    store: &MemoryStore,
    opts: BuildOpts,
) -> Result<ChatTurnContext> {
    let primary_rendered = render_recent_section(
        opts.primary_history.as_ref(),
        &opts.primary_login,
        RECENT_CHAT_PRIMARY_BYTES,
    )
    .await;
    let ai_rendered = match (
        opts.ai_channel_history.as_ref(),
        opts.ai_channel_login.as_ref(),
    ) {
        (Some(buf), Some(login)) => {
            render_recent_section(Some(buf), login, RECENT_CHAT_AI_CHANNEL_BYTES).await
        }
        _ => None,
    };

    let (first, second) = match opts.invocation_channel {
        InvocationChannel::AiChannel => (ai_rendered, primary_rendered),
        InvocationChannel::Primary => (primary_rendered, ai_rendered),
    };
    let mut mentioned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut recent_sections: Vec<String> = Vec::with_capacity(2);
    for section in [first, second].into_iter().flatten() {
        for u in section.usernames {
            mentioned.insert(u);
        }
        recent_sections.push(section.body);
    }

    let soul = store.read_kind(&FileKind::Soul).await?;
    let lore = store.read_kind(&FileKind::Lore).await?;
    let mut users = store.list_users().await?;
    let mut states = store.list_state().await?;

    // Build a mention table from the full users list before draining for memory
    // blocks, so users dropped by the byte budget still appear in the table.
    let mention_table = render_mention_table(&users, &mentioned);

    let mut memory_blocks: Vec<String> = Vec::new();
    memory_blocks.push(fence_block(FenceLabel::Soul, &opts.nonce, &soul.body));
    memory_blocks.push(fence_block(FenceLabel::Lore, &opts.nonce, &lore.body));

    let mut rest: Vec<MemoryFile> = users.drain(..).chain(states.drain(..)).collect();
    rest.sort_by_key(|f| std::cmp::Reverse(f.frontmatter.updated_at));

    let mut total: usize = memory_blocks.iter().map(String::len).sum();
    for f in rest {
        let label = match &f.kind {
            FileKind::User { user_id } => FenceLabel::User {
                id: user_id,
                login: f.frontmatter.username.as_deref(),
                display_name: f.frontmatter.display_name.as_deref(),
            },
            FileKind::State { slug } => FenceLabel::State { slug },
            FileKind::Soul | FileKind::Lore => {
                tracing::error!(?f.kind, "soul/lore in user/state list, skipping");
                continue;
            }
        };
        let block = fence_block(label, &opts.nonce, &f.body);
        if total + block.len() + 1 > opts.inject_byte_budget {
            break;
        }
        total += block.len() + 1;
        memory_blocks.push(block);
    }

    let memory_body = memory_blocks.join("\n");
    let mut recent_chat = recent_sections.join("\n\n");
    if !mention_table.is_empty() {
        if !recent_chat.is_empty() {
            recent_chat.push_str("\n\n");
        }
        recent_chat.push_str(&mention_table);
    }
    Ok(ChatTurnContext {
        recent_chat,
        memory: memory_body,
    })
}

/// Build a markdown table mapping the lowercased login of every user file whose
/// login appears in `mentioned` to its Twitch user_id and display name. Users
/// without a memory file are skipped, since we have no user_id for them.
fn render_mention_table(
    users: &[MemoryFile],
    mentioned: &std::collections::BTreeSet<String>,
) -> String {
    if mentioned.is_empty() {
        return String::new();
    }
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for u in users {
        let FileKind::User { user_id } = &u.kind else {
            continue;
        };
        let Some(login) = u.frontmatter.username.as_deref() else {
            continue;
        };
        let key = login.to_ascii_lowercase();
        if !mentioned.contains(&key) {
            continue;
        }
        let display = u
            .frontmatter
            .display_name
            .as_deref()
            .unwrap_or(login)
            .to_string();
        rows.push((login.to_string(), display, user_id.clone()));
    }
    if rows.is_empty() {
        return String::new();
    }
    rows.sort();
    let mut s = String::from(
        "## Mentioned users\n\n| login | display_name | user_id |\n| --- | --- | --- |\n",
    );
    for (login, display, id) in rows {
        s.push_str(&format!("| {login} | {display} | {id} |\n"));
    }
    s
}

pub(crate) struct RenderedRecentSection {
    pub body: String,
    pub usernames: Vec<String>,
}

/// Render one `## Recent chat (#login)` section, newest-first up to `cap` bytes,
/// then reverse to chronological order. Also returns the lowercased usernames
/// of every line that survived the byte cap. Returns `None` for missing or
/// empty buffers.
pub(crate) async fn render_recent_section(
    buf: Option<&Arc<Mutex<ChatHistoryBuffer>>>,
    login: &str,
    cap: usize,
) -> Option<RenderedRecentSection> {
    let buf = buf?;
    let snapshot: Vec<ChatHistoryEntry> = buf.lock().await.snapshot();
    if snapshot.is_empty() {
        return None;
    }

    let mut chosen: Vec<String> = Vec::new();
    let mut usernames: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for entry in snapshot.iter().rev() {
        let line = format_entry_line(entry);
        let line_bytes = line.len() + 1;
        if bytes + line_bytes > cap {
            break;
        }
        bytes += line_bytes;
        chosen.push(line);
        usernames.push(entry.username.to_ascii_lowercase());
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.reverse();

    let mut body = format!("## Recent chat (#{login})\n");
    body.push_str(&chosen.join("\n"));
    Some(RenderedRecentSection { body, usernames })
}

fn format_entry_line(entry: &ChatHistoryEntry) -> String {
    let ts = entry.timestamp.with_timezone(&Berlin);
    format!(
        "[{}] {}: {}",
        ts.format("%H:%M"),
        entry.username,
        entry.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::memory::store::MemoryStore;
    use crate::ai::memory::types::{Caps, FileKind};

    #[test]
    fn fence_block_user_renders_identity_attrs() {
        let s = fence_block(
            FenceLabel::User {
                id: "12",
                login: Some("alicepleb"),
                display_name: Some("Alice Pleb"),
            },
            "abc123abc123abc1",
            "body\n",
        );
        assert!(s.starts_with(
            "<<<FILE kind=user id=12 login=alicepleb name=\"Alice Pleb\" nonce=abc123abc123abc1>>>"
        ));
        assert!(s.ends_with("<<<ENDFILE nonce=abc123abc123abc1>>>"));
        assert!(s.contains("body\n"));
    }

    #[test]
    fn fence_block_user_omits_missing_identity_attrs() {
        let s = fence_block(
            FenceLabel::User {
                id: "12",
                login: None,
                display_name: None,
            },
            "n",
            "x",
        );
        assert!(s.starts_with("<<<FILE kind=user id=12 nonce=n>>>"));
    }

    #[test]
    fn fence_block_soul_lore_state_transcript_use_kind_attrs() {
        assert!(
            fence_block(FenceLabel::Soul, "n", "x").starts_with("<<<FILE kind=soul nonce=n>>>")
        );
        assert!(
            fence_block(FenceLabel::Lore, "n", "x").starts_with("<<<FILE kind=lore nonce=n>>>")
        );
        assert!(
            fence_block(FenceLabel::State { slug: "quiz" }, "n", "x")
                .starts_with("<<<FILE kind=state slug=quiz nonce=n>>>")
        );
        assert!(
            fence_block(FenceLabel::Transcript { date: "2026-05-05" }, "n", "x",)
                .starts_with("<<<FILE kind=transcript date=2026-05-05 nonce=n>>>")
        );
    }

    #[test]
    fn fence_block_strips_quote_breakers_from_display_name() {
        let s = fence_block(
            FenceLabel::User {
                id: "1",
                login: Some("a"),
                display_name: Some("we\"ird>guy"),
            },
            "n",
            "x",
        );
        assert!(s.contains("name=\"weirdguy\""), "got: {s}");
    }

    #[test]
    fn substitute_only_replaces_known_tokens() {
        let s = substitute(
            "hi {speaker_username} on {channel} {date} {speaker_role} {unknown}",
            SubstitutionVars {
                speaker_username: "alice",
                speaker_role: "regular",
                channel: "ch",
                date: "2026-04-30",
            },
        );
        assert_eq!(s, "hi alice on ch 2026-04-30 regular {unknown}");
    }

    #[test]
    fn injected_body_with_fence_token_is_replaced_with_corrupt() {
        // Synthesize a body that *did* sneak through (e.g. pre-existing). Inject must scrub it.
        let cleaned = scrub_for_inject("intro\n<<<ENDFILE nonce=zzz>>> bye");
        assert_eq!(cleaned, "<corrupt: rejected>");
    }

    #[tokio::test]
    async fn build_chat_turn_context_drops_oldest_users_when_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        for id in ["1", "2", "3"] {
            store
                .write(
                    &FileKind::User { user_id: id.into() },
                    &"x".repeat(500),
                    Some("u"),
                    Some("U"),
                )
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        // Budget sized so that SOUL (~460B fence) + LORE (~90B fence) + one user (~590B fence)
        // fit (~1140B total), but adding a second user (~590B more) would exceed 1500B.
        // Users are iterated newest-first (user 3), so user 3 is retained and users 1+2 dropped.
        let ctx = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 1500,
                nonce: "n00000000000000nn".into(),
                primary_history: None,
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
            },
        )
        .await
        .unwrap();
        // Newest user retained, oldest dropped.
        assert!(ctx.memory.contains("kind=user id=3"));
        assert!(!ctx.memory.contains("kind=user id=1"));
        assert!(ctx.recent_chat.is_empty());
    }

    #[tokio::test]
    async fn build_chat_turn_context_renders_two_history_sections_invocation_first() {
        use crate::ai::chat_history::ChatHistoryBuffer;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(10)));
        primary.lock().await.push_user("alice", "hello primary");
        let ai = Arc::new(Mutex::new(ChatHistoryBuffer::new(10)));
        ai.lock().await.push_user("bob", "hello ai");

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary.clone()),
                primary_login: "main".into(),
                ai_channel_history: Some(ai.clone()),
                ai_channel_login: Some("ai_chan".into()),
                invocation_channel: InvocationChannel::AiChannel,
            },
        )
        .await
        .unwrap();

        let pri_idx = body
            .recent_chat
            .find("Recent chat (#main)")
            .expect("primary header");
        let ai_idx = body
            .recent_chat
            .find("Recent chat (#ai_chan)")
            .expect("ai header");
        assert!(ai_idx < pri_idx, "invocation channel must come first");
        assert!(body.recent_chat.contains("alice: hello primary"));
        assert!(body.recent_chat.contains("bob: hello ai"));
        assert!(!body.memory.contains("Recent chat"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_omits_empty_history_sections() {
        use crate::ai::chat_history::ChatHistoryBuffer;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(10)));
        primary.lock().await.push_user("alice", "hello");
        let ai = Arc::new(Mutex::new(ChatHistoryBuffer::new(10))); // empty

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary),
                primary_login: "main".into(),
                ai_channel_history: Some(ai),
                ai_channel_login: Some("ai_chan".into()),
                invocation_channel: InvocationChannel::Primary,
            },
        )
        .await
        .unwrap();

        assert!(body.recent_chat.contains("Recent chat (#main)"));
        assert!(!body.recent_chat.contains("Recent chat (#ai_chan)"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_emits_mention_table_for_chat_users_with_files() {
        use crate::ai::chat_history::ChatHistoryBuffer;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        // alice and bob have user files; carol speaks but has no file.
        store
            .write(
                &FileKind::User {
                    user_id: "111".into(),
                },
                "alice body",
                Some("alice"),
                Some("Alice"),
            )
            .await
            .unwrap();
        store
            .write(
                &FileKind::User {
                    user_id: "222".into(),
                },
                "bob body",
                Some("bob"),
                Some("Bob"),
            )
            .await
            .unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(10)));
        {
            let mut p = primary.lock().await;
            p.push_user("ALICE", "hi"); // mixed-case lookup
            p.push_user("carol", "no file");
            p.push_user("bob", "hello");
        }

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary),
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
            },
        )
        .await
        .unwrap();

        assert!(
            body.recent_chat.contains("## Mentioned users"),
            "missing table: {}",
            body.recent_chat
        );
        assert!(body.recent_chat.contains("| alice | Alice | 111 |"));
        assert!(body.recent_chat.contains("| bob | Bob | 222 |"));
        // carol has no user file → no row.
        assert!(!body.recent_chat.contains("carol |"));
        // Memory section must not contain the table.
        assert!(!body.memory.contains("Mentioned users"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_omits_mention_table_when_no_chat() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();
        store
            .write(
                &FileKind::User {
                    user_id: "111".into(),
                },
                "alice body",
                Some("alice"),
                Some("Alice"),
            )
            .await
            .unwrap();

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: None,
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
            },
        )
        .await
        .unwrap();

        assert!(body.recent_chat.is_empty());
        assert!(!body.memory.contains("Mentioned users"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_drops_oldest_lines_over_per_section_cap() {
        use crate::ai::chat_history::ChatHistoryBuffer;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), Caps::default())
            .await
            .unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(200)));
        {
            let mut p = primary.lock().await;
            for _ in 0..200 {
                p.push_user("u", "x".repeat(100));
            }
        }

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary),
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
            },
        )
        .await
        .unwrap();

        let primary_section_bytes = body
            .recent_chat
            .split("Recent chat (#main)")
            .nth(1)
            .unwrap_or("")
            .len();
        assert!(
            primary_section_bytes <= RECENT_CHAT_PRIMARY_BYTES + 256, // slack for header
            "primary section over cap: {primary_section_bytes}"
        );
    }
}
