use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::ai::llm::ToolCall;
use crate::ai::memory::scope::{is_write_allowed, seed_confidence, trust_level_for};
use crate::ai::memory::{Scope, UserRole};

const MEMORY_FILENAME: &str = "ai_memory.ron";

#[derive(Debug, Clone, Deserialize)]
struct LegacyMemory {
    fact: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyStore {
    memories: HashMap<String, LegacyMemory>,
}

/// Groups the live memory store handle, its on-disk path, the per-scope caps,
/// and the half-life used by the score/eviction policy.
pub struct MemoryConfig {
    pub store: Arc<RwLock<MemoryStore>>,
    pub path: PathBuf,
    pub caps: Caps,
    pub half_life_days: u32,
}

/// A single remembered fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub fact: String,
    pub scope: Scope,
    pub sources: Vec<String>,
    pub confidence: u8,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub access_count: u32,
}

impl Memory {
    pub fn new(
        fact: String,
        scope: Scope,
        source: String,
        confidence: u8,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            fact,
            scope,
            sources: vec![source],
            confidence,
            created_at: now,
            updated_at: now,
            last_accessed: now,
            access_count: 0,
        }
    }

    /// Relevance score in `[0, ~confidence]` space. Combines confidence,
    /// exponential decay on `last_accessed` (half-life `half_life_days`),
    /// and a sub-linear boost from `access_count`.
    pub fn score(&self, now: DateTime<Utc>, half_life_days: u32) -> f64 {
        // i64 → f64: precision loss only matters beyond ~285 million years;
        // age_days is bounded by clock skew in practice.
        let age_days = (now - self.last_accessed).num_seconds() as f64 / 86_400.0;
        let decay = (-(2f64.ln()) * age_days / f64::from(half_life_days)).exp();
        let hits = (1.0 + f64::from(self.access_count).ln_1p() / 5.0).max(1.0);
        (f64::from(self.confidence) / 100.0) * decay * hits
    }
}

/// A mapping from subject_id to the user's current display name, used to
/// present memories without leaking numeric user IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub username: String,
    pub updated_at: DateTime<Utc>,
}

/// Persistent store of AI memories, serialized to RON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStore {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub memories: HashMap<String, Memory>,
    #[serde(default)]
    pub identities: HashMap<String, Identity>,
}

fn default_version() -> u32 {
    2
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            version: 2,
            memories: HashMap::new(),
            identities: HashMap::new(),
        }
    }
}

impl MemoryStore {
    /// Load from disk. Tries the v2 format first; on parse failure falls back
    /// to the pre-rework legacy shape and migrates it to v2 in-place (a
    /// timestamped `ai_memory.ron.bak-<unix_ts>` backup is written before the
    /// rewrite). Returns empty store if the file doesn't exist.
    pub fn load(data_dir: &Path) -> Result<(Self, PathBuf)> {
        let path = data_dir.join(MEMORY_FILENAME);
        if !path.exists() {
            info!("No ai_memory.ron found, starting with empty memory store");
            return Ok((Self::default(), path));
        }
        let data = std::fs::read_to_string(&path).wrap_err("Failed to read ai_memory.ron")?;
        // Try v2 first
        if let Ok(store) = ron::from_str::<MemoryStore>(&data) {
            info!(
                count = store.memories.len(),
                identities = store.identities.len(),
                "Loaded AI memories (v2)"
            );
            return Ok((store, path));
        }
        // Fall back to legacy
        let legacy: LegacyStore = ron::from_str(&data)
            .wrap_err("Failed to parse ai_memory.ron (neither v2 nor legacy)")?;
        let backup = path.with_file_name(format!(
            "ai_memory.ron.bak-{}",
            chrono::Utc::now().timestamp()
        ));
        std::fs::copy(&path, &backup).wrap_err("Failed to write legacy backup")?;
        info!(
            backup = %backup.display(),
            count = legacy.memories.len(),
            "Migrating legacy AI memories to v2"
        );

        let mut out = Self::default();
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (old_key, lm) in legacy.memories {
            let base = sanitize_slug(&old_key);
            let mut slug = base.clone();
            let mut suffix = 0u32;
            while used.contains(&format!("lore::{slug}")) {
                suffix += 1;
                slug = format!("{base}-{suffix}");
            }
            let new_key = format!("lore::{slug}");
            used.insert(new_key.clone());
            let parsed = chrono::DateTime::parse_from_rfc3339(&lm.updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let created = chrono::DateTime::parse_from_rfc3339(&lm.created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or(parsed);
            out.memories.insert(
                new_key,
                Memory {
                    fact: lm.fact,
                    scope: Scope::Lore,
                    sources: vec!["legacy".to_string()],
                    confidence: 40,
                    created_at: created,
                    updated_at: parsed,
                    last_accessed: parsed,
                    access_count: 0,
                },
            );
        }
        // Persist new format immediately so subsequent starts skip this path.
        out.save(&path)?;
        Ok((out, path))
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

    /// Pure read; output is byte-identical for identical store state, enabling prefix caching.
    pub fn format_for_prompt(&self, caps: &Caps) -> Option<String> {
        fn by_conf_then_slug(a: &(&str, &Memory), b: &(&str, &Memory)) -> std::cmp::Ordering {
            b.1.confidence
                .cmp(&a.1.confidence)
                .then_with(|| a.0.cmp(b.0))
        }

        // Render a capped, grouped section for User or Pref scope.
        //
        // Sort ALL entries across all subject_ids by (-confidence, slug), take
        // the scope-level cap once, then group survivors by subject_id for the
        // per-user headers. This keeps per-scope budget honest regardless of how
        // many distinct users have memories. Distinct subject_ids that happen to
        // share a username are disambiguated with a `(user <id>)` suffix.
        let render_keyed_section = |entries: &mut Vec<(&str, &str, &Memory)>,
                                    cap: usize,
                                    header_fn: &dyn Fn(&str, bool, &str) -> String,
                                    out: &mut String| {
            if entries.is_empty() {
                return;
            }
            entries.sort_by(|a, b| by_conf_then_slug(&(a.1, a.2), &(b.1, b.2)));

            // Group cap-limited survivors by subject_id.
            let mut by_subject: std::collections::BTreeMap<&str, Vec<&Memory>> =
                std::collections::BTreeMap::new();
            for &(subject_id, _, mem) in entries.iter().take(cap) {
                by_subject.entry(subject_id).or_default().push(mem);
            }

            // Detect username collisions so we can disambiguate in headers.
            let mut name_count: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for &subject_id in by_subject.keys() {
                *name_count
                    .entry(self.resolve_username(subject_id))
                    .or_default() += 1;
            }

            // Emit sections in alphabetical username order for determinism.
            let mut subject_ids: Vec<&&str> = by_subject.keys().collect();
            subject_ids.sort_by_key(|&&id| self.resolve_username(id));

            for &&subject_id in &subject_ids {
                let name = self.resolve_username(subject_id);
                let collision = name_count.get(&name).copied().unwrap_or(0) > 1;
                out.push_str(&header_fn(&name, collision, subject_id));
                for mem in &by_subject[subject_id] {
                    out.push_str(&format!("\n- {} (conf {})", mem.fact, mem.confidence));
                }
            }
        };

        if self.memories.is_empty() {
            return None;
        }

        let mut lore: Vec<(&str, &Memory)> = Vec::new();
        // (subject_id, slug, &Memory) — keyed by subject_id to avoid username-collision merges
        let mut user_entries: Vec<(&str, &str, &Memory)> = Vec::new();
        let mut pref_entries: Vec<(&str, &str, &Memory)> = Vec::new();

        for (key, mem) in &self.memories {
            match &mem.scope {
                Scope::Lore => lore.push((key.as_str(), mem)),
                Scope::User { subject_id } => {
                    user_entries.push((subject_id.as_str(), key.as_str(), mem));
                }
                Scope::Pref { subject_id } => {
                    pref_entries.push((subject_id.as_str(), key.as_str(), mem));
                }
            }
        }

        let mut out = String::from("\n\n## Known facts");

        if !lore.is_empty() {
            out.push_str("\n### Channel lore");
            lore.sort_by(by_conf_then_slug);
            for (_, mem) in lore.iter().take(caps.max_lore) {
                out.push_str(&format!("\n- {} (conf {})", mem.fact, mem.confidence));
            }
        }

        render_keyed_section(
            &mut user_entries,
            caps.max_user,
            &|name, collision, id| {
                if collision {
                    format!("\n### About {name} (user {id})")
                } else {
                    format!("\n### About {name}")
                }
            },
            &mut out,
        );

        render_keyed_section(
            &mut pref_entries,
            caps.max_pref,
            &|name, collision, id| {
                if collision {
                    format!("\n### {name}'s preferences (user {id})")
                } else {
                    format!("\n### {name}'s preferences")
                }
            },
            &mut out,
        );

        Some(out)
    }

    fn resolve_username(&self, subject_id: &str) -> String {
        self.identities
            .get(subject_id)
            .map(|i| i.username.clone())
            .unwrap_or_else(|| format!("user {subject_id}"))
    }

    /// Evict the lowest-scoring memory whose scope matches `tag` when the
    /// scope is already at capacity. Returns the evicted key, if any.
    pub fn evict_lowest_in_scope(
        &mut self,
        tag: &str,
        now: DateTime<Utc>,
        half_life_days: u32,
    ) -> Option<String> {
        let candidates: Vec<(String, f64, DateTime<Utc>)> = self
            .memories
            .iter()
            .filter(|(_, m)| m.scope.tag() == tag)
            .map(|(k, m)| (k.clone(), m.score(now, half_life_days), m.last_accessed))
            .collect();
        let (key, _, _) = candidates.into_iter().min_by(|a, b| {
            // score() is finite (no NaN-producing paths); unwrap_or is a defensive fallback.
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.2.cmp(&b.2)) // older last_accessed wins the tie
        })?;
        self.memories.remove(&key);
        Some(key)
    }

    /// Insert or refresh the `(user_id -> username)` mapping. Returns `true`
    /// when the stored username changed (new entry or rename) and `false`
    /// when only the observation timestamp would move — the caller can skip
    /// persisting in the latter case to avoid per-turn fsync storms.
    pub fn upsert_identity(&mut self, user_id: &str, username: &str, now: DateTime<Utc>) -> bool {
        match self.identities.get_mut(user_id) {
            Some(existing) if existing.username == username => {
                existing.updated_at = now;
                false
            }
            Some(existing) => {
                existing.username = username.to_string();
                existing.updated_at = now;
                true
            }
            None => {
                self.identities.insert(
                    user_id.to_string(),
                    Identity {
                        username: username.to_string(),
                        updated_at: now,
                    },
                );
                true
            }
        }
    }

    /// Execute a single extractor tool call against the store. Routes
    /// `save_memory` through the permission matrix and `get_memories` for
    /// read-only inspection. Other tool names return an error string.
    pub fn execute_tool_call(&mut self, call: &ToolCall, ctx: &DispatchContext<'_>) -> String {
        if let Some(err) = &call.arguments_parse_error {
            return format!(
                "Error: tool '{name}' arguments were not valid JSON ({error}). \
                 Raw text: {raw}. Resend with a valid JSON object.",
                name = call.name,
                error = err.error,
                raw = err.raw,
            );
        }
        match call.name.as_str() {
            "save_memory" => self.handle_save_memory(call, ctx),
            "get_memories" => self.handle_get_memories(call, ctx),
            other => format!("Unknown tool: {other}"),
        }
    }

    fn handle_save_memory(&mut self, call: &ToolCall, ctx: &DispatchContext<'_>) -> String {
        let args = &call.arguments;
        let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");
        let slug = args.get("slug").and_then(|v| v.as_str()).unwrap_or("");
        let fact = args.get("fact").and_then(|v| v.as_str()).unwrap_or("");
        let subject_id = args
            .get("subject_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        if slug.is_empty() || fact.is_empty() {
            return "Error: save_memory requires non-empty 'slug' and 'fact'".into();
        }
        let scope = match (scope_str, subject_id) {
            ("user", Some(s)) => Scope::User { subject_id: s },
            ("pref", Some(s)) => Scope::Pref { subject_id: s },
            ("lore", None) => Scope::Lore,
            ("user" | "pref", None) => {
                return "Error: save_memory requires 'subject_id' for user/pref scope".into();
            }
            ("lore", Some(_)) => {
                return "Error: save_memory must NOT include 'subject_id' for lore scope".into();
            }
            _ => return format!("Error: unknown scope '{scope_str}' (expected user|lore|pref)"),
        };
        if !is_write_allowed(ctx.speaker_role, &scope, ctx.speaker_id) {
            return format!(
                "Error: not authorized to save {} for subject={:?} — speaker role is {:?}. \
                 Regular users may write User/Pref only with subject_id == speaker_id. \
                 Prefs are always self-only. Lore is moderator/broadcaster-only.",
                scope.tag(),
                scope.subject_id(),
                ctx.speaker_role
            );
        }

        let key = build_key(&scope, slug);
        let level = trust_level_for(ctx.speaker_role, &scope, ctx.speaker_id);
        let seed_conf = seed_confidence(level);
        let now = ctx.now;

        if let Some(existing) = self.memories.get_mut(&key) {
            // Confidence is deliberately NOT touched on extractor collision —
            // that lane is adversarial (prompt injection, untrusted speakers).
            // Confidence adjustments go through the consolidator, which runs
            // daily and applies `corroboration_boost` from distinct sources.
            existing.fact = fact.to_string();
            existing.updated_at = now;
            if !existing.sources.iter().any(|s| s == ctx.speaker_username) {
                existing.sources.push(ctx.speaker_username.to_string());
            }
            return format!("Updated memory '{key}'");
        }

        let cap = match &scope {
            Scope::User { .. } => ctx.caps.max_user,
            Scope::Lore => ctx.caps.max_lore,
            Scope::Pref { .. } => ctx.caps.max_pref,
        };
        let count = self.count_scope(scope.tag());
        if count >= cap {
            if let Some(evicted) = self.evict_lowest_in_scope(scope.tag(), now, ctx.half_life_days)
            {
                info!(%evicted, "Evicted to make room");
            } else {
                return format!(
                    "Memory full ({count}/{cap}) and no evictable entry in scope {}",
                    scope.tag()
                );
            }
        }

        self.memories.insert(
            key.clone(),
            Memory::new(
                fact.to_string(),
                scope,
                ctx.speaker_username.to_string(),
                seed_conf,
                now,
            ),
        );
        format!("Saved memory '{key}' (confidence {seed_conf})")
    }

    fn count_scope(&self, tag: &str) -> usize {
        self.memories
            .values()
            .filter(|m| m.scope.tag() == tag)
            .count()
    }

    fn handle_get_memories(&self, call: &ToolCall, _ctx: &DispatchContext<'_>) -> String {
        let scope_str = call
            .arguments
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject_id = call.arguments.get("subject_id").and_then(|v| v.as_str());
        let mut out: Vec<String> = self
            .memories
            .iter()
            .filter(|(_, m)| {
                m.scope.tag() == scope_str
                    && match subject_id {
                        Some(s) => m.scope.subject_id() == Some(s),
                        None => true,
                    }
            })
            .map(|(k, m)| {
                format!(
                    "- {}: {} (confidence={}, sources={:?})",
                    k, m.fact, m.confidence, m.sources
                )
            })
            .collect();
        out.sort();
        if out.is_empty() {
            "(none)".into()
        } else {
            out.join("\n")
        }
    }

    /// Execute a consolidator tool call. Dispatches on `call.name` to the
    /// bounded metadata-preserving ops that the consolidation pass may issue.
    /// Malformed arguments are surfaced as a string (same pattern as
    /// `execute_tool_call`) so the LLM can correct and retry.
    pub fn execute_consolidator_tool(&mut self, call: &ToolCall, now: DateTime<Utc>) -> String {
        if let Some(err) = &call.arguments_parse_error {
            return format!(
                "Error: tool '{name}' arguments were not valid JSON ({error}). \
                 Raw text: {raw}. Resend with a valid JSON object.",
                name = call.name,
                error = err.error,
                raw = err.raw,
            );
        }
        match call.name.as_str() {
            "drop_memory" => self.handle_drop_memory(call),
            "merge_memories" => self.handle_merge_memories(call, now),
            "edit_memory" => self.handle_edit_memory(call),
            "get_memory" => self.handle_get_memory(call),
            other => format!("Unknown consolidator tool: {other}"),
        }
    }

    fn handle_drop_memory(&mut self, call: &ToolCall) -> String {
        let key = call
            .arguments
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if self.memories.remove(key).is_some() {
            format!("Dropped memory '{key}'")
        } else {
            format!("No memory with key '{key}'")
        }
    }

    fn handle_get_memory(&self, call: &ToolCall) -> String {
        let key = call
            .arguments
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match self.memories.get(key) {
            Some(m) => format!(
                "{}: {} (confidence={}, sources={:?}, last_accessed={}, hits={})",
                key, m.fact, m.confidence, m.sources, m.last_accessed, m.access_count
            ),
            None => format!("No memory with key '{key}'"),
        }
    }

    fn handle_merge_memories(&mut self, call: &ToolCall, now: DateTime<Utc>) -> String {
        let keys: Vec<String> = call
            .arguments
            .get("keys")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let new_slug = call
            .arguments
            .get("new_slug")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_fact = call
            .arguments
            .get("new_fact")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if keys.len() < 2 {
            return "Error: merge_memories requires at least 2 keys".into();
        }
        if new_slug.is_empty() || new_fact.is_empty() {
            return "Error: new_slug and new_fact must be non-empty".into();
        }

        // Collect inputs; bail if any missing so we don't partially mutate.
        let inputs: Vec<Memory> = match keys
            .iter()
            .map(|k| self.memories.get(k).cloned())
            .collect::<Option<Vec<_>>>()
        {
            Some(v) => v,
            None => return "Error: one or more keys not found; aborting merge".into(),
        };

        let scope = inputs[0].scope.clone();
        // Split the "same-scope" check into tag (category) and subject_id so
        // we can surface the more specific error message. `Scope` derives
        // structural equality, so a direct `==` would collapse both diagnostics
        // into "scope mismatch".
        if !inputs.iter().all(|m| m.scope.tag() == scope.tag()) {
            return "Error: scope mismatch across merge inputs".into();
        }
        if matches!(scope, Scope::User { .. } | Scope::Pref { .. }) {
            let sid = scope.subject_id();
            if !inputs.iter().all(|m| m.scope.subject_id() == sid) {
                return "Error: subject_id mismatch across merge inputs".into();
            }
        }

        // Spec (Corroboration Boost / Merge Metadata) filters BOTH sentinels
        // — "__identity__" and "legacy" — when counting distinct sources.
        let is_real = |s: &str| s != "__identity__" && s != "legacy";
        let max_single_sources = inputs
            .iter()
            .map(|m| m.sources.iter().filter(|s| is_real(s)).count())
            .max()
            .unwrap_or(0);
        let mut sources: Vec<String> = Vec::new();
        for m in &inputs {
            for s in &m.sources {
                if is_real(s) && !sources.contains(s) {
                    sources.push(s.clone());
                }
            }
        }
        let distinct_after = sources.len();
        let max_conf = inputs.iter().map(|m| m.confidence).max().unwrap_or(0);
        // usize → u32: source counts are bounded by per-memory cap; saturating
        // cast is safe. `bonus` is then `5 * (distinct_after - max_single)`.
        let bonus = u32::try_from(distinct_after.saturating_sub(max_single_sources))
            .unwrap_or(u32::MAX)
            .saturating_mul(5);
        let confidence =
            u8::try_from(u32::from(max_conf).saturating_add(bonus).min(100)).unwrap_or(100);
        let created_at = inputs.iter().map(|m| m.created_at).min().unwrap_or(now);
        let last_accessed = inputs.iter().map(|m| m.last_accessed).max().unwrap_or(now);
        let access_count = inputs.iter().map(|m| m.access_count).sum();

        let new_key = build_key(&scope, new_slug);
        if self.memories.contains_key(&new_key) && !keys.contains(&new_key) {
            return format!(
                "Error: new_key '{new_key}' collides with existing non-merged memory; choose another slug"
            );
        }

        let msg = format!(
            "Merged {} memories into '{new_key}' (confidence={confidence})",
            keys.len()
        );
        // Remove old keys first so an overlapping new_key slot is free.
        for k in &keys {
            self.memories.remove(k);
        }
        self.memories.insert(
            new_key,
            Memory {
                fact: new_fact.to_string(),
                scope,
                sources,
                confidence,
                created_at,
                updated_at: now,
                last_accessed,
                access_count,
            },
        );
        msg
    }

    fn handle_edit_memory(&mut self, call: &ToolCall) -> String {
        let key = call
            .arguments
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let fact_opt = call.arguments.get("fact").and_then(|v| v.as_str());
        let delta_opt = call
            .arguments
            .get("confidence_delta")
            .and_then(serde_json::Value::as_i64);
        let drop_source_opt = call.arguments.get("drop_source").and_then(|v| v.as_str());

        if let Some(f) = fact_opt {
            if f.is_empty() {
                return "Error: 'fact' must be non-empty".into();
            }
            let len = f.chars().count();
            if len > 500 {
                return format!("Error: 'fact' too long ({len} chars > 500)");
            }
        }
        if let Some(d) = delta_opt
            && !(-50..=30).contains(&d)
        {
            return format!("Error: confidence_delta {d} out of range [-50, +30]");
        }

        let mem = match self.memories.get_mut(key) {
            Some(m) => m,
            None => return format!("No memory with key '{key}'"),
        };
        if let Some(f) = fact_opt {
            mem.fact = f.to_string();
        }
        if let Some(d) = delta_opt {
            // `d` is validated in `[-50, 30]`; clamp also guards against any
            // pre-existing out-of-band `confidence` values.
            let new_conf = (i64::from(mem.confidence) + d).clamp(0, 100);
            mem.confidence = u8::try_from(new_conf).unwrap_or(0);
        }
        if let Some(src) = drop_source_opt {
            let before = mem.sources.len();
            mem.sources.retain(|s| s != src);
            if mem.sources.len() == before {
                debug!(key, src, "drop_source target not in sources; no-op");
            }
        }
        format!("Edited memory '{key}' (confidence={})", mem.confidence)
    }
}

/// Per-scope maximum memory counts. Enforced by the extractor dispatcher.
#[derive(Debug, Clone)]
pub struct Caps {
    pub max_user: usize,
    pub max_lore: usize,
    pub max_pref: usize,
}

/// Context threaded through the extractor dispatcher: who's speaking, their
/// role, the caps that apply, and the clock.
pub struct DispatchContext<'a> {
    pub speaker_id: &'a str,
    pub speaker_username: &'a str,
    pub speaker_role: UserRole,
    pub caps: Caps,
    pub half_life_days: u32,
    pub now: DateTime<Utc>,
}

/// Turn a human-readable label into a lowercase ASCII slug. Runs of
/// non-alphanumeric characters collapse into a single `-`; leading and
/// trailing dashes are trimmed. Non-ASCII input (e.g. emoji) is dropped
/// the same way.
pub(crate) fn sanitize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true; // suppress leading dashes
    for c in s.chars() {
        let norm = c.to_ascii_lowercase();
        if norm.is_ascii_alphanumeric() {
            out.push(norm);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Compose the canonical key for a memory: `user:<uid>:<slug>`, `lore::<slug>`,
/// or `pref:<uid>:<slug>`. The slug is sanitized before composition.
pub(crate) fn build_key(scope: &Scope, slug: &str) -> String {
    let slug = sanitize_slug(slug);
    match scope {
        Scope::User { subject_id } => format!("user:{}:{}", subject_id, slug),
        Scope::Lore => format!("lore::{}", slug),
        Scope::Pref { subject_id } => format!("pref:{}:{}", subject_id, slug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::llm;

    fn empty_store() -> MemoryStore {
        MemoryStore::default()
    }

    #[test]
    fn memory_new_seeds_all_fields() {
        use chrono::Utc;
        let mem = Memory::new(
            "alice plays Tarkov".to_string(),
            Scope::User {
                subject_id: "1".to_string(),
            },
            "alice".to_string(),
            70,
            Utc::now(),
        );
        assert_eq!(mem.confidence, 70);
        assert_eq!(mem.sources, vec!["alice".to_string()]);
        assert_eq!(mem.access_count, 0);
        assert_eq!(mem.created_at, mem.updated_at);
        assert_eq!(mem.created_at, mem.last_accessed);
    }

    fn mem_at(confidence: u8, last_accessed_days_ago: i64, access_count: u32) -> Memory {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        Memory {
            fact: "f".to_string(),
            scope: Scope::Lore,
            sources: vec!["x".to_string()],
            confidence,
            created_at: now,
            updated_at: now,
            last_accessed: now - Duration::days(last_accessed_days_ago),
            access_count,
        }
    }

    #[test]
    fn score_monotone_in_confidence() {
        use chrono::Utc;
        let a = mem_at(90, 0, 0);
        let b = mem_at(50, 0, 0);
        assert!(a.score(Utc::now(), 30) > b.score(Utc::now(), 30));
    }

    #[test]
    fn score_decays_with_age() {
        use chrono::Utc;
        let fresh = mem_at(70, 0, 0);
        let stale = mem_at(70, 60, 0);
        assert!(fresh.score(Utc::now(), 30) > stale.score(Utc::now(), 30));
    }

    #[test]
    fn score_boosts_with_access_count() {
        use chrono::Utc;
        let cold = mem_at(70, 0, 0);
        let hot = mem_at(70, 0, 20);
        assert!(hot.score(Utc::now(), 30) > cold.score(Utc::now(), 30));
    }

    #[test]
    fn sanitize_slug_basic() {
        assert_eq!(sanitize_slug("Favorite Game"), "favorite-game");
        assert_eq!(sanitize_slug("alice's cat!!"), "alice-s-cat");
        assert_eq!(sanitize_slug("---weird---"), "weird");
        assert_eq!(sanitize_slug("emoji 🙂 drop"), "emoji-drop");
    }

    #[test]
    fn build_key_per_scope() {
        assert_eq!(
            build_key(
                &Scope::User {
                    subject_id: "42".into()
                },
                "plays-tarkov"
            ),
            "user:42:plays-tarkov"
        );
        assert_eq!(
            build_key(&Scope::Lore, "channel-emote"),
            "lore::channel-emote"
        );
        assert_eq!(
            build_key(
                &Scope::Pref {
                    subject_id: "42".into()
                },
                "speaks-german"
            ),
            "pref:42:speaks-german"
        );
    }

    #[test]
    fn store_default_is_v2_and_empty() {
        let s = MemoryStore::default();
        assert_eq!(s.version, 2);
        assert!(s.memories.is_empty());
        assert!(s.identities.is_empty());
    }

    #[test]
    fn store_save_load_round_trip() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ai_memory.ron");
        let s = MemoryStore {
            version: 2,
            memories: HashMap::new(),
            identities: HashMap::new(),
        };
        s.save(&path).unwrap();
        let (loaded, _) = MemoryStore::load(dir.path()).unwrap();
        assert_eq!(loaded.version, 2);
    }

    #[test]
    fn load_legacy_ron_migrates_to_v2() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ai_memory.ron");
        // Legacy shape: no version, memories keyed by arbitrary slug, Memory uses String timestamps
        let legacy = r#"(
            memories: {
                "alice likes tarkov": (
                    fact: "alice plays tarkov",
                    created_at: "2026-01-01T00:00:00Z",
                    updated_at: "2026-01-02T00:00:00Z",
                ),
            }
        )"#;
        std::fs::write(&path, legacy).unwrap();
        let (store, _) = MemoryStore::load(dir.path()).unwrap();
        assert_eq!(store.version, 2);
        let migrated_key = store.memories.keys().next().unwrap().clone();
        assert!(migrated_key.starts_with("lore::"));
        let m = store.memories.get(&migrated_key).unwrap();
        assert_eq!(m.scope, Scope::Lore);
        assert_eq!(m.sources, vec!["legacy".to_string()]);
        assert_eq!(m.confidence, 40);
        // backup file exists
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert!(entries.iter().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("ai_memory.ron.bak-")
        }));
    }

    #[test]
    fn identity_round_trip_ron() {
        use chrono::Utc;
        let id = Identity {
            username: "alice".to_string(),
            updated_at: Utc::now(),
        };
        let s = ron::ser::to_string_pretty(&id, ron::ser::PrettyConfig::default()).unwrap();
        let back: Identity = ron::from_str(&s).unwrap();
        assert_eq!(back.username, id.username);
    }

    #[test]
    fn upsert_identity_new() {
        let mut s = MemoryStore::default();
        let now = Utc::now();
        assert!(s.upsert_identity("42", "alice", now));
        assert_eq!(s.identities.get("42").unwrap().username, "alice");
        assert_eq!(s.identities.get("42").unwrap().updated_at, now);
    }

    #[test]
    fn upsert_identity_rename_overwrites() {
        let mut s = MemoryStore::default();
        let now = Utc::now();
        s.upsert_identity("42", "alice", now);
        assert!(s.upsert_identity("42", "alicette", now));
        assert_eq!(s.identities.get("42").unwrap().username, "alicette");
        assert_eq!(s.identities.len(), 1);
    }

    #[test]
    fn upsert_identity_same_username_returns_false() {
        use chrono::Duration;
        let mut s = MemoryStore::default();
        let t0 = Utc::now();
        s.upsert_identity("42", "alice", t0);
        let t1 = t0 + Duration::seconds(30);
        assert!(!s.upsert_identity("42", "alice", t1));
        assert_eq!(s.identities.get("42").unwrap().updated_at, t1);
    }

    #[test]
    fn evict_tie_breaks_by_older_last_accessed() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        let mut s = MemoryStore::default();
        // identical score inputs, different last_accessed
        let mut m_old = mem_at(70, 5, 0);
        m_old.last_accessed = now - Duration::days(5);
        let mut m_new = mem_at(70, 5, 0);
        m_new.last_accessed = now - Duration::days(3);
        s.memories.insert("lore::old".into(), m_old);
        s.memories.insert("lore::new".into(), m_new);
        let evicted = s.evict_lowest_in_scope("lore", now, 30).unwrap();
        assert_eq!(evicted, "lore::old");
    }

    fn ctx_for(speaker_id: &'static str, role: UserRole) -> DispatchContext<'static> {
        // Used by dispatcher tests that don't care about speaker_username or
        // cap exhaustion; defaults are spacious.
        DispatchContext {
            speaker_id,
            speaker_username: speaker_id,
            speaker_role: role,
            caps: Caps {
                max_user: 50,
                max_lore: 50,
                max_pref: 50,
            },
            half_life_days: 30,
            now: Utc::now(),
        }
    }

    #[test]
    fn execute_tool_call_surfaces_parse_error() {
        let mut store = empty_store();
        let call = ToolCall {
            id: "c1".to_string(),
            name: "save_memory".to_string(),
            arguments: serde_json::Value::Null,
            arguments_parse_error: Some(llm::ToolCallArgsError {
                error: "expected `,` or `}` at line 1 column 17".to_string(),
                raw: "{\"slug\":\"k\" \"fact\":\"f\"}".to_string(),
            }),
        };
        let ctx = ctx_for("42", UserRole::Regular);
        let result = store.execute_tool_call(&call, &ctx);
        assert!(result.contains("not valid JSON"), "got: {result}");
        assert!(result.contains("save_memory"), "got: {result}");
        assert!(result.contains("{\"slug\":\"k\""), "got: {result}");
        assert!(store.memories.is_empty());
    }

    #[test]
    fn execute_tool_call_missing_args_is_distinct_from_parse_error() {
        let mut store = empty_store();
        let call = ToolCall {
            id: "c2".to_string(),
            name: "save_memory".to_string(),
            arguments: serde_json::Value::Null,
            arguments_parse_error: None,
        };
        let ctx = ctx_for("42", UserRole::Regular);
        let result = store.execute_tool_call(&call, &ctx);
        assert!(
            result.contains("requires non-empty 'slug' and 'fact'"),
            "got: {result}"
        );
        assert!(!result.contains("not valid JSON"), "got: {result}");
    }

    #[test]
    fn save_memory_self_claim_creates_user_scope() {
        let mut store = MemoryStore::default();
        let call = ToolCall {
            id: "c1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "scope": "user",
                "subject_id": "42",
                "slug": "plays-tarkov",
                "fact": "alice plays tarkov",
            }),
            arguments_parse_error: None,
        };
        let ctx = DispatchContext {
            speaker_id: "42",
            speaker_username: "alice",
            speaker_role: UserRole::Regular,
            caps: Caps {
                max_user: 50,
                max_lore: 50,
                max_pref: 50,
            },
            half_life_days: 30,
            now: Utc::now(),
        };
        let out = store.execute_tool_call(&call, &ctx);
        assert!(out.contains("Saved"), "got: {out}");
        assert!(store.memories.contains_key("user:42:plays-tarkov"));
    }

    #[test]
    fn save_memory_third_party_rejected() {
        let mut store = MemoryStore::default();
        let call = ToolCall {
            id: "c1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "scope": "user",
                "subject_id": "99",
                "slug": "drinks-coffee",
                "fact": "bob drinks coffee",
            }),
            arguments_parse_error: None,
        };
        let ctx = DispatchContext {
            speaker_id: "42", // != subject_id 99
            speaker_username: "alice",
            speaker_role: UserRole::Regular,
            caps: Caps {
                max_user: 50,
                max_lore: 50,
                max_pref: 50,
            },
            half_life_days: 30,
            now: Utc::now(),
        };
        let out = store.execute_tool_call(&call, &ctx);
        assert!(out.contains("not authorized"), "got: {out}");
        assert!(store.memories.is_empty());
    }

    #[test]
    fn save_memory_pref_self_only_even_for_mod() {
        let mut store = MemoryStore::default();
        let call = ToolCall {
            id: "c1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "scope": "pref",
                "subject_id": "99",
                "slug": "language",
                "fact": "speaks German",
            }),
            arguments_parse_error: None,
        };
        let ctx = DispatchContext {
            speaker_id: "42",
            speaker_username: "modguy",
            speaker_role: UserRole::Moderator,
            caps: Caps {
                max_user: 50,
                max_lore: 50,
                max_pref: 50,
            },
            half_life_days: 30,
            now: Utc::now(),
        };
        let out = store.execute_tool_call(&call, &ctx);
        assert!(out.contains("not authorized"), "got: {out}");
    }

    #[test]
    fn save_memory_collision_same_speaker_keeps_single_source() {
        // Invariant under test: re-saving the same (scope, subject_id, slug)
        // from the same speaker must not duplicate `sources`. The plan's
        // original test also covered a second-speaker append, but that half
        // requires Task 10's identity wiring; kept simple and focused here.
        use chrono::Duration;
        let now = Utc::now();
        let mut store = MemoryStore::default();
        store.memories.insert(
            "user:42:plays-tarkov".into(),
            Memory::new(
                "alice plays tarkov".into(),
                Scope::User {
                    subject_id: "42".into(),
                },
                "alice".into(),
                70,
                now - Duration::minutes(5),
            ),
        );
        let call = ToolCall {
            id: "c1".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "scope": "user",
                "subject_id": "42",
                "slug": "plays-tarkov",
                "fact": "alice plays tarkov on wipes",
            }),
            arguments_parse_error: None,
        };
        let ctx = DispatchContext {
            speaker_id: "42",
            speaker_username: "alice",
            speaker_role: UserRole::Regular,
            caps: Caps {
                max_user: 50,
                max_lore: 50,
                max_pref: 50,
            },
            half_life_days: 30,
            now,
        };
        let _ = store.execute_tool_call(&call, &ctx);
        let mem = store.memories.get("user:42:plays-tarkov").unwrap();
        assert_eq!(mem.sources, vec!["alice".to_string()]);
        assert_eq!(mem.fact, "alice plays tarkov on wipes");
    }

    #[test]
    fn save_memory_collision_appends_new_speaker_source() {
        // Companion to the single-speaker test: a second, distinct speaker
        // corroborating the same key must append to `sources`.
        let now = Utc::now();
        let mut store = MemoryStore::default();
        store.memories.insert(
            "user:42:plays-tarkov".into(),
            Memory::new(
                "alice plays tarkov".into(),
                Scope::User {
                    subject_id: "42".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        let call = ToolCall {
            id: "c2".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "scope": "user",
                "subject_id": "42",
                "slug": "plays-tarkov",
                "fact": "alice plays tarkov",
            }),
            arguments_parse_error: None,
        };
        let ctx = DispatchContext {
            speaker_id: "100",
            speaker_username: "modguy",
            speaker_role: UserRole::Moderator,
            caps: Caps {
                max_user: 50,
                max_lore: 50,
                max_pref: 50,
            },
            half_life_days: 30,
            now,
        };
        let _ = store.execute_tool_call(&call, &ctx);
        let mem = store.memories.get("user:42:plays-tarkov").unwrap();
        assert_eq!(mem.sources, vec!["alice".to_string(), "modguy".to_string()]);
    }

    #[test]
    fn drop_memory_removes() {
        let mut s = MemoryStore::default();
        let now = Utc::now();
        s.memories.insert(
            "lore::x".into(),
            Memory::new("x".into(), Scope::Lore, "alice".into(), 50, now),
        );
        let call = ToolCall {
            id: "c".into(),
            name: "drop_memory".into(),
            arguments: serde_json::json!({"key": "lore::x"}),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("Dropped"));
        assert!(s.memories.is_empty());
    }

    #[test]
    fn drop_memory_missing_key_is_noop_warn() {
        let mut s = MemoryStore::default();
        let call = ToolCall {
            id: "c".into(),
            name: "drop_memory".into(),
            arguments: serde_json::json!({"key": "missing"}),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, Utc::now());
        assert!(out.contains("No memory"));
    }

    #[test]
    fn consolidator_parse_error_surfaced() {
        let mut s = MemoryStore::default();
        let call = ToolCall {
            id: "c".into(),
            name: "drop_memory".into(),
            arguments: serde_json::Value::Null,
            arguments_parse_error: Some(llm::ToolCallArgsError {
                error: "boom".into(),
                raw: "{".into(),
            }),
        };
        let out = s.execute_consolidator_tool(&call, Utc::now());
        assert!(out.contains("not valid JSON"), "got: {out}");
    }

    #[test]
    fn get_memory_returns_entry() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "lore::x".into(),
            Memory::new("a fact".into(), Scope::Lore, "alice".into(), 65, now),
        );
        let call = ToolCall {
            id: "c".into(),
            name: "get_memory".into(),
            arguments: serde_json::json!({"key": "lore::x"}),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("lore::x"), "got: {out}");
        assert!(out.contains("a fact"), "got: {out}");
        assert!(out.contains("confidence=65"), "got: {out}");
    }

    #[test]
    fn get_memory_missing_key_returns_no_memory() {
        let s = MemoryStore::default();
        let call = ToolCall {
            id: "c".into(),
            name: "get_memory".into(),
            arguments: serde_json::json!({"key": "missing"}),
            arguments_parse_error: None,
        };
        // get_memory is a read-only dispatch; we need a mut ref only because
        // execute_consolidator_tool takes &mut self (shared with mutators).
        let mut s = s;
        let out = s.execute_consolidator_tool(&call, Utc::now());
        assert!(out.contains("No memory"), "got: {out}");
    }

    #[test]
    fn edit_memory_missing_key_returns_no_memory() {
        let mut s = MemoryStore::default();
        let call = ToolCall {
            id: "c".into(),
            name: "edit_memory".into(),
            arguments: serde_json::json!({"key": "missing", "confidence_delta": 10}),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, Utc::now());
        assert!(out.contains("No memory"), "got: {out}");
    }

    #[test]
    fn merge_memories_unions_sources_dedup() {
        use chrono::Duration;
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "user:1:a".into(),
            Memory {
                fact: "plays tarkov".into(),
                scope: Scope::User {
                    subject_id: "1".into(),
                },
                sources: vec!["alice".into(), "bob".into()],
                confidence: 70,
                created_at: now - Duration::days(5),
                updated_at: now - Duration::days(2),
                last_accessed: now - Duration::days(1),
                access_count: 2,
            },
        );
        s.memories.insert(
            "user:1:b".into(),
            Memory {
                fact: "tarkov player".into(),
                scope: Scope::User {
                    subject_id: "1".into(),
                },
                sources: vec!["bob".into(), "carol".into()],
                confidence: 60,
                created_at: now - Duration::days(10),
                updated_at: now - Duration::days(1),
                last_accessed: now,
                access_count: 5,
            },
        );
        let call = ToolCall {
            id: "c".into(),
            name: "merge_memories".into(),
            arguments: serde_json::json!({
                "keys": ["user:1:a", "user:1:b"],
                "new_slug": "tarkov-player",
                "new_fact": "plays Escape from Tarkov",
            }),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("Merged"), "got: {out}");
        let merged = s.memories.get("user:1:tarkov-player").unwrap();
        assert_eq!(merged.sources, vec!["alice", "bob", "carol"]);
        // max(70, 60) + 5 × (distinct_after=3 − max_single=2) = 75
        assert_eq!(merged.confidence, 75);
        assert_eq!(merged.access_count, 7);
        assert_eq!(merged.last_accessed, now);
        assert_eq!(merged.created_at, now - Duration::days(10));
        assert!(!s.memories.contains_key("user:1:a"));
        assert!(!s.memories.contains_key("user:1:b"));
    }

    #[test]
    fn merge_memories_rejects_scope_mismatch() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "user:1:a".into(),
            Memory::new(
                "x".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "lore::b".into(),
            Memory::new("y".into(), Scope::Lore, "alice".into(), 70, now),
        );
        let call = ToolCall {
            id: "c".into(),
            name: "merge_memories".into(),
            arguments: serde_json::json!({
                "keys": ["user:1:a", "lore::b"],
                "new_slug": "mix",
                "new_fact": "mixed",
            }),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("scope mismatch"), "got: {out}");
        assert!(s.memories.contains_key("user:1:a"));
        assert!(s.memories.contains_key("lore::b"));
    }

    #[test]
    fn merge_memories_rejects_subject_mismatch() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "user:1:a".into(),
            Memory::new(
                "x".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "user:2:b".into(),
            Memory::new(
                "y".into(),
                Scope::User {
                    subject_id: "2".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        let call = ToolCall {
            id: "c".into(),
            name: "merge_memories".into(),
            arguments: serde_json::json!({
                "keys": ["user:1:a", "user:2:b"],
                "new_slug": "mix",
                "new_fact": "mixed",
            }),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("subject_id mismatch"), "got: {out}");
        // originals untouched on reject
        assert!(s.memories.contains_key("user:1:a"));
        assert!(s.memories.contains_key("user:2:b"));
    }

    #[test]
    fn edit_memory_confidence_delta_applied_clamped() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "lore::x".into(),
            Memory::new("f".into(), Scope::Lore, "alice".into(), 50, now),
        );
        let call = ToolCall {
            id: "c".into(),
            name: "edit_memory".into(),
            arguments: serde_json::json!({"key": "lore::x", "confidence_delta": 30}),
            arguments_parse_error: None,
        };
        let _ = s.execute_consolidator_tool(&call, now);
        assert_eq!(s.memories.get("lore::x").unwrap().confidence, 80);
    }

    #[test]
    fn edit_memory_confidence_delta_out_of_range_rejected() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "lore::x".into(),
            Memory::new("f".into(), Scope::Lore, "alice".into(), 50, now),
        );
        let call = ToolCall {
            id: "c".into(),
            name: "edit_memory".into(),
            arguments: serde_json::json!({"key": "lore::x", "confidence_delta": 99}),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("out of range"), "got: {out}");
        assert_eq!(s.memories.get("lore::x").unwrap().confidence, 50);
    }

    #[test]
    fn edit_memory_drop_source_removes() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        let mut m = Memory::new("f".into(), Scope::Lore, "alice".into(), 50, now);
        m.sources.push("bob".into());
        s.memories.insert("lore::x".into(), m);
        let call = ToolCall {
            id: "c".into(),
            name: "edit_memory".into(),
            arguments: serde_json::json!({"key": "lore::x", "drop_source": "bob"}),
            arguments_parse_error: None,
        };
        let _ = s.execute_consolidator_tool(&call, now);
        assert_eq!(
            s.memories.get("lore::x").unwrap().sources,
            vec!["alice".to_string()]
        );
    }

    #[test]
    fn edit_memory_fact_too_long_rejected() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "lore::x".into(),
            Memory::new("f".into(), Scope::Lore, "alice".into(), 50, now),
        );
        let long = "a".repeat(600);
        let call = ToolCall {
            id: "c".into(),
            name: "edit_memory".into(),
            arguments: serde_json::json!({"key": "lore::x", "fact": long}),
            arguments_parse_error: None,
        };
        let out = s.execute_consolidator_tool(&call, now);
        assert!(out.contains("too long"));
    }

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
            "user:1:likes-cats".into(),
            Memory::new(
                "alice likes cats".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
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
            "user:1:likes-cats".into(),
            Memory::new(
                "likes cats".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "user:2:is-pilot".into(),
            Memory::new(
                "is a pilot".into(),
                Scope::User {
                    subject_id: "2".into(),
                },
                "bob".into(),
                85,
                now,
            ),
        );
        s.memories.insert(
            "pref:1:call-me-al".into(),
            Memory::new(
                "address as Al".into(),
                Scope::Pref {
                    subject_id: "1".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "lore::channel-vibe".into(),
            Memory::new(
                "channel is German-friendly".into(),
                Scope::Lore,
                "mod".into(),
                90,
                now,
            ),
        );

        let out = s.format_for_prompt(&caps_unbounded()).unwrap();

        let lore_idx = out.find("### Channel lore").expect("lore section");
        let about_alice_idx = out.find("### About alice").expect("about-alice section");
        let about_bob_idx = out.find("### About bob").expect("about-bob section");
        let pref_alice_idx = out.find("### alice's preferences").expect("pref section");
        assert!(lore_idx < about_alice_idx);
        assert!(
            about_alice_idx < about_bob_idx,
            "users sorted alphabetically"
        );
        assert!(about_bob_idx < pref_alice_idx);

        assert!(out.contains("- likes cats (conf 70)"));
        assert!(out.contains("- is a pilot (conf 85)"));
        assert!(out.contains("- channel is German-friendly (conf 90)"));
        assert!(out.contains("- address as Al (conf 70)"));

        let mut s2 = MemoryStore::default();
        s2.memories.insert(
            "user:99:x".into(),
            Memory::new(
                "foo".into(),
                Scope::User {
                    subject_id: "99".into(),
                },
                "ghost".into(),
                50,
                now,
            ),
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
                format!("user:1:fact-{i}"),
                Memory::new(
                    format!("fact {i}"),
                    Scope::User {
                        subject_id: "1".into(),
                    },
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
        for (i, conf) in [10u8, 90, 50, 30, 70].iter().enumerate() {
            s.memories.insert(
                format!("user:1:f{i}"),
                Memory::new(
                    format!("fact {i}"),
                    Scope::User {
                        subject_id: "1".into(),
                    },
                    "alice".into(),
                    *conf,
                    now,
                ),
            );
        }
        let caps = Caps {
            max_user: 2,
            max_lore: 1000,
            max_pref: 1000,
        };
        let out = s.format_for_prompt(&caps).unwrap();

        assert!(out.contains("(conf 90)"));
        assert!(out.contains("(conf 70)"));
        assert!(!out.contains("(conf 50)"));
        assert!(!out.contains("(conf 30)"));
        assert!(!out.contains("(conf 10)"));
    }

    #[test]
    fn format_for_prompt_cap_is_per_scope_not_per_user_bucket() {
        // max_user=2 with 3 facts for alice and 3 for bob → only the top-2
        // across the whole User scope should render, not top-2 per user (6 total).
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.upsert_identity("1", "alice", now);
        s.upsert_identity("2", "bob", now);
        for (slug, subj, conf) in [
            ("a", "1", 90u8), // alice — highest
            ("b", "1", 60),   // alice
            ("c", "1", 40),   // alice
            ("d", "2", 85),   // bob — second highest
            ("e", "2", 55),   // bob
            ("f", "2", 30),   // bob
        ] {
            s.memories.insert(
                format!("user:{subj}:{slug}"),
                Memory::new(
                    format!("fact {slug}"),
                    Scope::User {
                        subject_id: subj.into(),
                    },
                    if subj == "1" { "alice" } else { "bob" }.into(),
                    conf,
                    now,
                ),
            );
        }
        let caps = Caps {
            max_user: 2,
            max_lore: 1000,
            max_pref: 1000,
        };
        let out = s.format_for_prompt(&caps).unwrap();
        // Top-2 across scope: alice/a (90) and bob/d (85)
        assert!(out.contains("(conf 90)"), "got: {out}");
        assert!(out.contains("(conf 85)"), "got: {out}");
        assert!(!out.contains("(conf 60)"), "got: {out}");
        assert!(!out.contains("(conf 55)"), "got: {out}");
        assert!(!out.contains("(conf 40)"), "got: {out}");
        assert!(!out.contains("(conf 30)"), "got: {out}");
        // Sections for both users still present (survivors from distinct subject_ids)
        assert!(out.contains("### About alice"), "got: {out}");
        assert!(out.contains("### About bob"), "got: {out}");
    }

    #[test]
    fn format_for_prompt_disambiguates_username_collision() {
        // Two distinct subject_ids that both resolve to "alice" must not have
        // their memories silently merged — headers get a `(user <id>)` suffix.
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.upsert_identity("1", "alice", now); // original alice
        s.upsert_identity("5", "alice", now); // username reclaimed by another user
        s.memories.insert(
            "user:1:secret".into(),
            Memory::new(
                "hates spinach".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                80,
                now,
            ),
        );
        s.memories.insert(
            "user:5:secret".into(),
            Memory::new(
                "loves spinach".into(),
                Scope::User {
                    subject_id: "5".into(),
                },
                "alice".into(),
                80,
                now,
            ),
        );
        let out = s.format_for_prompt(&caps_unbounded()).unwrap();
        assert!(out.contains("(user 1)"), "got: {out}");
        assert!(out.contains("(user 5)"), "got: {out}");
        assert!(out.contains("hates spinach"), "got: {out}");
        assert!(out.contains("loves spinach"), "got: {out}");
        // No un-disambiguated "### About alice" header should appear
        assert!(
            !out.contains("\n### About alice\n"),
            "plain header must not appear on collision, got: {out}"
        );
    }

    #[test]
    fn format_for_prompt_byte_identical_across_unrelated_clock_motion() {
        // Two stores: identical facts but memories created/accessed at different
        // wall-clock times. Output must be byte-identical — proof that render
        // encodes no time-varying metadata (cache-stable across turns).
        use chrono::Duration;
        let t0 = Utc::now() - Duration::hours(24);
        let t1 = Utc::now(); // "later" wall clock

        let mut s0 = MemoryStore::default();
        s0.upsert_identity("1", "alice", t0);
        s0.memories.insert(
            "user:1:likes-cats".into(),
            Memory::new(
                "likes cats".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                80,
                t0,
            ),
        );
        s0.memories.insert(
            "lore::vibe".into(),
            Memory::new("channel is chill".into(), Scope::Lore, "mod".into(), 90, t0),
        );

        let mut s1 = MemoryStore::default();
        s1.upsert_identity("1", "alice", t1);
        s1.memories.insert(
            "user:1:likes-cats".into(),
            Memory::new(
                "likes cats".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                80,
                t1,
            ),
        );
        s1.memories.insert(
            "lore::vibe".into(),
            Memory::new("channel is chill".into(), Scope::Lore, "mod".into(), 90, t1),
        );

        let caps = caps_unbounded();
        assert_eq!(
            s0.format_for_prompt(&caps),
            s1.format_for_prompt(&caps),
            "render must not encode timestamps — output must be byte-identical regardless of when memories were created/accessed"
        );
    }

    #[test]
    fn get_memories_filters_by_scope_and_subject() {
        let now = Utc::now();
        let mut s = MemoryStore::default();
        s.memories.insert(
            "user:1:a".into(),
            Memory::new(
                "alice fact".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "user:2:b".into(),
            Memory::new(
                "bob fact".into(),
                Scope::User {
                    subject_id: "2".into(),
                },
                "bob".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "lore::c".into(),
            Memory::new("channel fact".into(), Scope::Lore, "mod".into(), 90, now),
        );
        let call = ToolCall {
            id: "g".into(),
            name: "get_memories".into(),
            arguments: serde_json::json!({"scope": "user", "subject_id": "1"}),
            arguments_parse_error: None,
        };
        let ctx = ctx_for("1", UserRole::Regular);
        let out = s.execute_tool_call(&call, &ctx);
        assert!(out.contains("user:1:a"), "got: {out}");
        assert!(!out.contains("user:2:b"), "got: {out}");
        assert!(!out.contains("lore::c"), "got: {out}");
    }
}
