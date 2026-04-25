# Cacheable Memory Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render AI memories in the system prompt with full nuance (scope, confidence, resolved username) while keeping the system prompt prefix-stable across turns so OpenAI/Ollama prompt caching hits.

**Architecture:** Split the chat completion into a stable system message (bot personality + deterministic memory snapshot) and a volatile user message (chat history, current time, instruction). Memory rendering becomes a pure read — no `last_accessed`/`access_count` mutation on render — so the system prompt only changes when the store is mutated by `save_memory`/consolidation. Memories are grouped by scope with `subject_id → username` resolution from the existing `identities` map and confidence shown inline; per-scope cap reuses `Caps` (top-N by confidence then alphabetical, fully deterministic).

**Tech Stack:** Rust, `tokio::sync::RwLock`, `chrono`, `chrono-tz`, RON-serialized store at `$DATA_DIR/ai_memory.ron`. LLM clients are OpenAI-compatible and Ollama (`src/llm/`). No new deps.

**Issue:** GitHub #75 ("improve rendering of memories in prompt").

**Out of scope:** semantic/embedding relevance filtering, per-query top-N by score, exposing access bumps to the consolidator. Those are follow-ups.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/memory/store.rs` | modify | `format_for_prompt` becomes `&self`, pure, grouped, capped, resolves identities, includes confidence. Replace mutating tests. |
| `src/commands/ai.rs` | modify | Move `Current time:` line + chat history out of system prompt into the user message. Drop `&mut` lock on store. |
| `config.toml.example` | modify | Document that memory rendering is stable/cacheable; note `max_user`/`max_lore`/`max_pref` also gate render breadth. |
| `tests/` (existing integration tests under `tests/`) | modify only if a test pins the old prompt shape | Snapshot updates, if any. |

No new files. No new config keys. The existing per-scope `Caps` from `[ai.memory]` are reused as render caps so we don't introduce a second knob.

---

### Task 1: Pure, identity-resolving, confidence-aware `format_for_prompt`

**Files:**
- Modify: `src/memory/store.rs:217-232` (the function body)
- Modify: `src/memory/store.rs:1472-1537` (existing render tests — replace, don't extend)

This task changes the rendering contract. After this task, `format_for_prompt` takes `&self` (not `&mut self`), takes a `&Caps` parameter to bound output size per scope, ignores `now` (kept in signature for future relevance use → drop the parameter entirely; callers updated in Task 3), groups output by scope with usernames, and emits confidence inline. `last_accessed`/`access_count` are no longer touched by rendering at all.

- [ ] **Step 1: Replace the three render tests with the new contract**

Delete `format_for_prompt_bumps_access_for_rendered_memories` (lines 1472–1509), `format_for_prompt_empty_store_does_not_mutate` (1511–1519), and `format_for_prompt_repeated_calls_accumulate_count` (1521–1537). In their place add the following block in the same `mod tests` (preserve surrounding `use` statements):

```rust
fn caps_unbounded() -> Caps {
    Caps {
        max_user: 1000,
        max_lore: 1000,
        max_pref: 1000,
    }
}

#[test]
fn format_for_prompt_does_not_mutate_store() {
    use chrono::Duration;
    let t0 = Utc::now() - Duration::hours(1);
    let mut s = MemoryStore::default();
    s.memories.insert(
        "user::1::a".into(),
        Memory::new(
            "alice likes cats".into(),
            Scope::User { subject_id: "1".into() },
            "alice".into(),
            70,
            t0,
        ),
    );
    s.upsert_identity("1", "alice", t0);

    let snapshot_before = s.memories.clone();
    let _ = s.format_for_prompt(&caps_unbounded());
    assert_eq!(s.memories, snapshot_before, "render must not mutate");
}

#[test]
fn format_for_prompt_empty_returns_none() {
    let s = MemoryStore::default();
    assert!(s.format_for_prompt(&caps_unbounded()).is_none());
}

#[test]
fn format_for_prompt_groups_and_resolves_usernames() {
    let now = Utc::now();
    let mut s = MemoryStore::default();
    s.upsert_identity("1", "alice", now);
    s.upsert_identity("2", "bob", now);
    s.memories.insert(
        "user::1::likes-cats".into(),
        Memory::new(
            "likes cats".into(),
            Scope::User { subject_id: "1".into() },
            "alice".into(),
            70,
            now,
        ),
    );
    s.memories.insert(
        "user::2::is-pilot".into(),
        Memory::new(
            "is a pilot".into(),
            Scope::User { subject_id: "2".into() },
            "bob".into(),
            85,
            now,
        ),
    );
    s.memories.insert(
        "pref::1::call-me-al".into(),
        Memory::new(
            "address as Al".into(),
            Scope::Pref { subject_id: "1".into() },
            "alice".into(),
            70,
            now,
        ),
    );
    s.memories.insert(
        "lore::channel-vibe".into(),
        Memory::new("channel is German-friendly".into(), Scope::Lore, "mod".into(), 90, now),
    );

    let out = s.format_for_prompt(&caps_unbounded()).unwrap();

    // Sections present, in canonical order: Lore, About <user>, <user>'s preferences.
    let lore_idx = out.find("### Channel lore").expect("lore section");
    let about_alice_idx = out.find("### About alice").expect("about-alice section");
    let about_bob_idx = out.find("### About bob").expect("about-bob section");
    let pref_alice_idx = out.find("### alice's preferences").expect("pref section");
    assert!(lore_idx < about_alice_idx);
    assert!(about_alice_idx < about_bob_idx, "users sorted alphabetically");
    assert!(about_bob_idx < pref_alice_idx);

    // Lines carry confidence inline.
    assert!(out.contains("- likes cats (conf 70)"));
    assert!(out.contains("- is a pilot (conf 85)"));
    assert!(out.contains("- channel is German-friendly (conf 90)"));
    assert!(out.contains("- address as Al (conf 70)"));

    // Users without a known username fall back to `user <id>`.
    let mut s2 = MemoryStore::default();
    s2.memories.insert(
        "user::99::x".into(),
        Memory::new("foo".into(), Scope::User { subject_id: "99".into() }, "ghost".into(), 50, now),
    );
    let out2 = s2.format_for_prompt(&caps_unbounded()).unwrap();
    assert!(out2.contains("### About user 99"), "got: {out2}");
}

#[test]
fn format_for_prompt_is_deterministic_across_calls() {
    let now = Utc::now();
    let mut s = MemoryStore::default();
    s.upsert_identity("1", "alice", now);
    for i in 0..10u32 {
        s.memories.insert(
            format!("user::1::fact-{i}"),
            Memory::new(
                format!("fact {i}"),
                Scope::User { subject_id: "1".into() },
                "alice".into(),
                70,
                now,
            ),
        );
    }
    let a = s.format_for_prompt(&caps_unbounded()).unwrap();
    let b = s.format_for_prompt(&caps_unbounded()).unwrap();
    let c = s.format_for_prompt(&caps_unbounded()).unwrap();
    assert_eq!(a, b);
    assert_eq!(b, c);
}

#[test]
fn format_for_prompt_respects_per_scope_caps() {
    let now = Utc::now();
    let mut s = MemoryStore::default();
    s.upsert_identity("1", "alice", now);
    // 5 user-scope memories, varying confidence.
    for (i, conf) in [10u8, 90, 50, 30, 70].iter().enumerate() {
        s.memories.insert(
            format!("user::1::f{i}"),
            Memory::new(
                format!("fact {i}"),
                Scope::User { subject_id: "1".into() },
                "alice".into(),
                *conf,
                now,
            ),
        );
    }
    let caps = Caps { max_user: 2, max_lore: 1000, max_pref: 1000 };
    let out = s.format_for_prompt(&caps).unwrap();

    // Only the top-2 by confidence (90, 70) survive the cap.
    assert!(out.contains("(conf 90)"));
    assert!(out.contains("(conf 70)"));
    assert!(!out.contains("(conf 50)"));
    assert!(!out.contains("(conf 30)"));
    assert!(!out.contains("(conf 10)"));
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

```bash
cargo test --lib memory::store -- format_for_prompt
```

Expected: compile error or failing assertions — current `format_for_prompt` takes `DateTime<Utc>` (not `&Caps`), is `&mut self`, and renders flat `slug: fact` lines.

- [ ] **Step 3: Rewrite `format_for_prompt`**

Replace `src/memory/store.rs:217-232` with:

```rust
/// Render a stable, prefix-cacheable snapshot of the store for injection
/// into the system prompt. Pure read — does NOT touch `last_accessed` or
/// `access_count`, so the same store state always produces byte-identical
/// output. Cache invalidation is therefore tied to *store mutation* (writes
/// via `save_memory` or consolidation), not to per-turn render calls.
///
/// Output structure, in this fixed order:
///   ## Known facts
///   ### Channel lore
///   - <fact> (conf <n>)
///   ### About <username>     (one section per user, alphabetical by username)
///   - <fact> (conf <n>)
///   ### <username>'s preferences
///   - <fact> (conf <n>)
///
/// Within each section, lines are sorted by `(-confidence, slug)` so the
/// highest-confidence facts come first and ties are deterministic. When a
/// scope's entry count exceeds the matching cap, only the top-cap rows by
/// that ordering are rendered.
///
/// `subject_id` is resolved to the current `username` via `identities`;
/// unknown ids fall back to the literal `user <id>` so output never leaks
/// raw numeric ids when a mapping exists.
///
/// Returns `None` only when the store is completely empty.
pub fn format_for_prompt(&self, caps: &Caps) -> Option<String> {
    if self.memories.is_empty() {
        return None;
    }

    // Bucket by section key.
    let mut lore: Vec<(&str, &Memory)> = Vec::new();
    // BTreeMap → alphabetical username iteration for deterministic ordering.
    let mut user_buckets: std::collections::BTreeMap<String, Vec<&Memory>> =
        std::collections::BTreeMap::new();
    let mut pref_buckets: std::collections::BTreeMap<String, Vec<&Memory>> =
        std::collections::BTreeMap::new();

    for (key, mem) in &self.memories {
        match &mem.scope {
            Scope::Lore => lore.push((key.as_str(), mem)),
            Scope::User { subject_id } => {
                let name = self.resolve_username(subject_id);
                user_buckets.entry(name).or_default().push(mem);
            }
            Scope::Pref { subject_id } => {
                let name = self.resolve_username(subject_id);
                pref_buckets.entry(name).or_default().push(mem);
            }
        }
    }

    let mut out = String::from("\n\n## Known facts");

    if !lore.is_empty() {
        out.push_str("\n### Channel lore");
        // Sort by (-confidence, slug); cap.
        let mut rows: Vec<&(&str, &Memory)> = lore.iter().collect();
        rows.sort_by(|a, b| {
            b.1.confidence
                .cmp(&a.1.confidence)
                .then_with(|| a.0.cmp(b.0))
        });
        for (_, mem) in rows.iter().take(caps.max_lore) {
            out.push_str(&format!("\n- {} (conf {})", mem.fact, mem.confidence));
        }
    }

    for (username, mems) in &user_buckets {
        out.push_str(&format!("\n### About {username}"));
        let mut rows: Vec<&&Memory> = mems.iter().collect();
        rows.sort_by(|a, b| {
            b.confidence
                .cmp(&a.confidence)
                .then_with(|| a.fact.cmp(&b.fact))
        });
        for mem in rows.iter().take(caps.max_user) {
            out.push_str(&format!("\n- {} (conf {})", mem.fact, mem.confidence));
        }
    }

    for (username, mems) in &pref_buckets {
        out.push_str(&format!("\n### {username}'s preferences"));
        let mut rows: Vec<&&Memory> = mems.iter().collect();
        rows.sort_by(|a, b| {
            b.confidence
                .cmp(&a.confidence)
                .then_with(|| a.fact.cmp(&b.fact))
        });
        for mem in rows.iter().take(caps.max_pref) {
            out.push_str(&format!("\n- {} (conf {})", mem.fact, mem.confidence));
        }
    }

    Some(out)
}

fn resolve_username(&self, subject_id: &str) -> String {
    self.identities
        .get(subject_id)
        .map(|i| i.username.clone())
        .unwrap_or_else(|| format!("user {subject_id}"))
}
```

- [ ] **Step 4: Run the rewritten tests**

```bash
cargo test --lib memory::store -- format_for_prompt
```

Expected: PASS — all five new tests green.

- [ ] **Step 5: Run full memory suite to catch collateral damage**

```bash
cargo test --lib memory
```

Expected: PASS. If a different test referenced the old signature, fix it now (it should not — `format_for_prompt` is only called from `commands/ai.rs`, addressed in Task 3).

- [ ] **Step 6: Commit**

```bash
git add src/memory/store.rs
git commit -m "refactor(memory): make format_for_prompt pure, grouped, capped, identity-resolved

Render is now &self with no access_count/last_accessed mutation, so the
same store state produces byte-identical output across calls. Output is
grouped by scope (Lore / About <user> / <user>'s preferences) with
confidence inline, deterministic sort, and per-scope Caps applied at
render time. Sets up the system prompt to be prefix-cacheable across
turns when no store write occurs in between (refs #75)."
```

---

### Task 2: Split system vs user message in the `!ai` handler for cacheability

**Files:**
- Modify: `src/commands/ai.rs:134-185` (system/user message construction)
- (No test file changes here — handler tests live in `tests/` under integration; cacheability check lives in Task 4 against the unit-level renderer.)

After this task, the system prompt is `{prompts.system}{facts_block_or_empty}` — nothing time-dependent. The user message holds the volatile bits in this order: a `Current time:` line, the optional `## Recent chat\n...` block, the rendered instruction template. This way OpenAI's automatic prefix cache and Ollama's KV cache hit on every turn until either (a) the operator edits `[ai].system_prompt` or (b) a memory write changes the store.

- [ ] **Step 1: Read the current handler block to confirm starting state**

```bash
sed -n '130,190p' src/commands/ai.rs
```

Expected: matches the structure shown in the prior investigation (system prompt concatenates personality + `facts` + `Current time:` line; user message is `instruction_template` with `{chat_history}` substitution).

- [ ] **Step 2: Replace the prompt construction**

In `src/commands/ai.rs`, replace the block from `let now = Utc::now();` through the `Message { role: "user", content: user_message }` construction (currently lines 134–185) with:

```rust
let now = Utc::now();
let facts = if let Some(ref mem) = self.memory {
    let store_guard = mem.config.store.read().await;
    store_guard
        .format_for_prompt(&mem.config.caps)
        .unwrap_or_default()
} else {
    String::new()
};

// System prompt: personality + stable memory snapshot only. Must NOT
// contain time, chat history, or anything else that varies per turn,
// so OpenAI/Ollama prefix caching can reuse it across turns until the
// store mutates.
let system_prompt = format!("{}{}", self.prompts.system, facts);

let chat_history_text = if let Some(ref chat) = self.chat_ctx {
    let buf = chat.history.lock().await;
    if buf.is_empty() {
        String::new()
    } else {
        buf.iter()
            .map(|(user, msg, ts)| {
                let ts_berlin = ts.with_timezone(&chrono_tz::Europe::Berlin);
                format!("[{}] {user}: {msg}", ts_berlin.format("%H:%M"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
} else {
    String::new()
};

// User message: all volatile per-turn context lives here. Time first
// so the model anchors before reading history/instruction.
let now_berlin = now
    .with_timezone(&chrono_tz::Europe::Berlin)
    .format("%Y-%m-%d %H:%M %Z");
let instruction_rendered = self
    .prompts
    .instruction_template
    .replace("{message}", &instruction)
    .replace("{chat_history}", &chat_history_text);
let user_message = format!("Current time: {now_berlin}\n\n{instruction_rendered}");

let request = ChatCompletionRequest {
    model: self.model.clone(),
    messages: vec![
        Message { role: "system".to_string(), content: system_prompt },
        Message { role: "user".to_string(), content: user_message },
    ],
};
```

Note the lock change: `mem.config.store.write().await` becomes `read().await` because `format_for_prompt` is now `&self`.

- [ ] **Step 3: Build to confirm it compiles**

```bash
cargo build
```

Expected: clean build. If a downstream caller of `format_for_prompt` exists outside `commands/ai.rs`, the compiler will surface it; there should be none — verify with:

```bash
rg -n "format_for_prompt" src/
```

Expected: only `src/memory/store.rs` (definition + tests) and `src/commands/ai.rs` (the call updated above).

- [ ] **Step 4: Run the full test suite**

```bash
cargo test
```

Expected: PASS. Integration tests that prefilled chat history into the user message via `{chat_history}` placeholders will continue to work — the substitution still happens, only the surrounding scaffold changed.

- [ ] **Step 5: Commit**

```bash
git add src/commands/ai.rs
git commit -m "refactor(ai): split system vs user message for prompt-cache stability

System prompt now holds only personality + stable memory snapshot.
Current time and chat history move to the user message so the system
prefix is byte-identical across turns until the memory store mutates,
letting OpenAI prefix caching and Ollama KV reuse hit on every turn.
Switches the store lock to read() since format_for_prompt is now &self
(refs #75)."
```

---

### Task 3: Cacheability regression test — same store state, byte-identical render

**Files:**
- Modify: `src/memory/store.rs` test module (add one focused test alongside the Task-1 tests)

This is the load-bearing guarantee for #75. We pin it with a unit test so a future change that re-introduces per-render mutation, time-dependent output, or non-deterministic iteration order fails CI immediately.

- [ ] **Step 1: Add the regression test**

Append inside `mod tests` in `src/memory/store.rs`:

```rust
#[test]
fn format_for_prompt_byte_identical_across_unrelated_clock_motion() {
    // Cacheability guarantee: with the store untouched, two renders separated
    // by wall-clock time must produce the exact same bytes. If this fails,
    // the system prompt's prefix has drifted and prompt caching will miss.
    use chrono::Duration;
    let t0 = Utc::now() - Duration::hours(2);
    let mut s = MemoryStore::default();
    s.upsert_identity("1", "alice", t0);
    s.upsert_identity("2", "bob", t0);
    s.memories.insert(
        "user::1::a".into(),
        Memory::new(
            "fact a".into(),
            Scope::User { subject_id: "1".into() },
            "alice".into(),
            70,
            t0,
        ),
    );
    s.memories.insert(
        "user::2::b".into(),
        Memory::new(
            "fact b".into(),
            Scope::User { subject_id: "2".into() },
            "bob".into(),
            70,
            t0,
        ),
    );
    s.memories.insert(
        "lore::c".into(),
        Memory::new("lore c".into(), Scope::Lore, "mod".into(), 80, t0),
    );

    let caps = Caps { max_user: 50, max_lore: 50, max_pref: 50 };
    let first = s.format_for_prompt(&caps).expect("non-empty");
    // Simulate "another turn happens" — no store mutation, just elapsed time
    // and an unrelated hashmap insertion-order shuffle via a clone round-trip.
    let s2: MemoryStore = ron::from_str(&ron::to_string(&s).unwrap()).unwrap();
    let second = s2.format_for_prompt(&caps).expect("non-empty");
    assert_eq!(first, second, "render must be deterministic post-serde");
}
```

- [ ] **Step 2: Run it**

```bash
cargo test --lib memory::store::tests::format_for_prompt_byte_identical_across_unrelated_clock_motion
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/memory/store.rs
git commit -m "test(memory): pin byte-identical render across clock motion + serde

Regression guard for #75: a future change that reintroduces non-pure
or time-dependent rendering will fail CI immediately."
```

---

### Task 4: Document the new contract in `config.toml.example`

**Files:**
- Modify: `config.toml.example:88-110` (the `# AI memory` block)

The schema doesn't change, but the comments need to reflect (a) that `max_*` caps now also bound rendered breadth in the system prompt and (b) that rendering is stable / cache-friendly.

- [ ] **Step 1: Update the comment block**

In `config.toml.example`, replace the paragraph above `[ai.memory]` (currently lines around 105–110, beginning "Caps are channel-wide totals…") with:

```toml
# Caps are channel-wide totals per scope, NOT per individual user.
# When a scope is full, the lowest-scoring entry (across all users in that
# scope) is evicted. Score combines confidence × exp-decay × hit-boost.
#
# The same caps also bound how many entries from each scope are rendered
# into the !ai system prompt. Within a scope, rendering picks the top-N by
# (-confidence, slug) so the highest-confidence facts always reach the LLM.
# Rendering is pure: it does not mutate access counters, so the system
# prompt is byte-identical across turns until the store is written. That
# preserves OpenAI prefix-cache and Ollama KV-cache hits on every turn.
```

- [ ] **Step 2: Sanity-check the file still parses**

```bash
cargo run -- --help >/dev/null 2>&1 || true
cargo build
```

Expected: clean build (the example file is not parsed at build time, but this confirms nothing else regressed).

- [ ] **Step 3: Commit**

```bash
git add config.toml.example
git commit -m "docs(config): note that memory caps also gate rendered breadth

Render is pure and deterministic, so the system prompt is stable across
turns until the store is mutated — preserves prompt-cache hits."
```

---

### Task 5: Pre-merge gate

- [ ] **Step 1: Run the full CI gate locally**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: all three clean. Fix any clippy nag inline; do not `#[allow]` without a one-line reason.

- [ ] **Step 2: Manual smoke (only if a dev twitch creds + LLM endpoint are available)**

```bash
RUST_LOG=debug cargo run
```

In two consecutive `!ai` turns with no `save_memory` writes between them, expect identical system-prompt bytes in the debug logs (the LLM client logs request payloads at debug level).

- [ ] **Step 3: Open the PR**

```bash
git push -u origin <branch>
gh pr create --title "feat(ai): cacheable, nuance-rich memory rendering (#75)" --body "$(cat <<'EOF'
## Summary
- `format_for_prompt` is now pure, grouped by scope, identity-resolved, and confidence-aware.
- Per-scope `Caps` gate render breadth (top-N by `(-confidence, slug)`).
- System prompt = personality + stable memory snapshot. Time + chat history move to the user message so the system prefix is byte-identical across turns until the store mutates — preserves OpenAI prefix-cache and Ollama KV-cache hits.

Closes #75.

## Test plan
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] Manual: two back-to-back `!ai` turns produce identical system-prompt bytes in debug logs (no store write between them).
EOF
)"
```

---

## Self-Review

**Spec coverage:** Issue #75 asks for (a) relevance and (b) confidence in rendered memories. Confidence: Task 1 inlines `(conf N)`. Relevance: addressed via per-scope `Caps` cap + confidence-first sort, intentionally avoiding per-query relevance because that breaks the cache (called out as out-of-scope, not silently dropped). Cacheability requirement from the user's directive: Tasks 2 + 3 enforce it (split layout + regression test).

**Placeholder scan:** none. Every code step shows the exact code; every command is concrete; expected outputs are stated.

**Type consistency:** new `format_for_prompt(&self, caps: &Caps) -> Option<String>` is referenced identically in Tasks 1, 2, 3. `Caps` field names (`max_user`, `max_lore`, `max_pref`) match `src/config.rs:91-96`. `resolve_username` is defined in Task 1 Step 3 and used inside the same function in the same step. `Scope::User { subject_id }` / `Scope::Pref { subject_id }` / `Scope::Lore` match `src/memory/scope.rs:4-8`.
