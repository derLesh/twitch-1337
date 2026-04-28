use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, MappedLocalTime, NaiveTime, Utc};
use eyre::{Result, WrapErr as _};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, warn};

use crate::ai::llm::{
    self, Message, ToolCallRound, ToolChatCompletionRequest, ToolChatCompletionResponse,
    ToolResultMessage,
};
use crate::ai::memory::Memory;
use crate::ai::memory::store::MemoryStore;
use crate::ai::memory::tools::consolidator_tools;

#[derive(Clone)]
pub struct ConsolidationLlmConfig {
    pub model: String,
    pub reasoning_effort: Option<String>,
}

/// Returns a list of `(key, reason)` tuples for memories flagged by the
/// deterministic pre-filter: confidence below 10 and last access older than
/// 60 days. Reported by the consolidation pass to the caller before any
/// LLM-driven curation runs.
pub fn hard_drop_candidates(store: &MemoryStore, now: DateTime<Utc>) -> Vec<(String, String)> {
    store
        .memories
        .iter()
        .filter_map(|(k, m)| {
            let stale_days = (now - m.last_accessed).num_days();
            if m.confidence < 10 && stale_days > 60 {
                Some((
                    k.clone(),
                    format!("confidence={} stale_days={}", m.confidence, stale_days),
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Boost `confidence` by `+5 * (distinct_sources - 1)`, where `distinct_sources`
/// counts non-sentinel entries in `sources` (`"legacy"` and `"__identity__"`
/// are excluded). Clamps the result to `[0, 100]`.
pub fn corroboration_boost(m: &Memory) -> u8 {
    let distinct: usize = m
        .sources
        .iter()
        .filter(|s| s.as_str() != "legacy" && s.as_str() != "__identity__")
        .count();
    // usize → u32: sources are bounded per memory; saturating cast is safe.
    let bonus = u32::try_from(distinct.saturating_sub(1))
        .unwrap_or(u32::MAX)
        .saturating_mul(5);
    u8::try_from(u32::from(m.confidence).saturating_add(bonus).min(100)).unwrap_or(100)
}

/// Run a single consolidation pass end-to-end:
///   1. Snapshot the store under a read lock.
///   2. Apply the deterministic hard-drop pre-filter and persist.
///   3. Apply the corroboration boost to every remaining memory and persist.
///   4. For each scope tag (`user`, `lore`, `pref`) prompt the consolidator
///      LLM with the current listing and apply any returned tool calls in a
///      fixed order (`drop_memory` before `merge_memories` before
///      `edit_memory`) so drops and merges cannot be clobbered by an earlier
///      edit in the same round. Persist after every round. Round cap of 5
///      per scope prevents runaway loops.
///
/// Each LLM call is wrapped in `timeout`; a timeout or transport error aborts
/// the pass (the caller in `spawn_consolidation` logs and retries tomorrow).
/// Per-op failures are surfaced to the LLM via the tool result payload, so a
/// malformed call does not kill the pass.
///
/// Retry behavior: the corroboration boost (step 3) is applied in-place before
/// the LLM pass. If the run is interrupted after this step and retried
/// tomorrow, the boost compounds until confidence saturates at 100.
/// Acceptable: the ceiling bounds the damage and the retry produces a valid
/// consolidated state.
pub async fn run_consolidation(
    llm: Arc<dyn llm::LlmClient>,
    llm_config: ConsolidationLlmConfig,
    store: Arc<RwLock<MemoryStore>>,
    store_path: PathBuf,
    timeout: Duration,
) -> Result<()> {
    let now = Utc::now();

    // 1. Snapshot
    let snapshot: MemoryStore = {
        let r = store.read().await;
        r.clone()
    };

    // 2+3. Hard-drops + corroboration boost (both deterministic; single write lock)
    let hard_drops = hard_drop_candidates(&snapshot, now);
    {
        let save_snapshot = {
            let mut w = store.write().await;
            for (k, reason) in &hard_drops {
                w.memories.remove(k);
                debug!(%k, %reason, "pre-filter hard-drop");
            }
            for m in w.memories.values_mut() {
                m.confidence = corroboration_boost(m);
            }
            w.clone()
        };
        save_snapshot.save(&store_path)?;
    }

    // 4. LLM pass per scope (user, lore, pref)
    let mut merged = 0usize;
    let mut dropped = 0usize;
    let mut edited = 0usize;
    for tag in ["user", "lore", "pref"] {
        let scope_snapshot: Vec<(String, Memory)> = {
            let r = store.read().await;
            r.memories
                .iter()
                .filter(|(_, m)| m.scope.tag() == tag)
                .map(|(k, m)| (k.clone(), m.clone()))
                .collect()
        };
        if scope_snapshot.is_empty() {
            continue;
        }
        let listing: String = scope_snapshot
            .iter()
            .map(|(k, m)| {
                format!(
                    "- {}: {} (confidence={}, sources={:?})",
                    k, m.fact, m.confidence, m.sources
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let sys = format!(
            "You are curating the AI memory store for scope '{tag}'. \
             Goals: merge duplicates, drop contradictions or hallucinations, refine with edit_memory. \
             Priority: merge > drop weaker on contradiction > edit confidence_delta > drop_source > edit fact wording. \
             Never change a fact's subject or core claim via edit — use merge or drop for that. \
             Respond with tool calls only; stop when done."
        );
        let user = format!("Current memories in scope {tag}:\n{listing}");
        let mut prior: Vec<ToolCallRound> = Vec::new();
        loop {
            let req = ToolChatCompletionRequest {
                model: llm_config.model.clone(),
                messages: vec![
                    Message {
                        role: "system".into(),
                        content: sys.clone(),
                    },
                    Message {
                        role: "user".into(),
                        content: user.clone(),
                    },
                ],
                tools: consolidator_tools(),
                reasoning_effort: llm_config.reasoning_effort.clone(),
                prior_rounds: prior.clone(),
            };
            let resp = tokio::time::timeout(timeout, llm.chat_completion_with_tools(req))
                .await
                .wrap_err("consolidation LLM timed out")?
                .wrap_err("consolidation LLM call failed")?;
            match resp {
                ToolChatCompletionResponse::Message(_) => break,
                ToolChatCompletionResponse::ToolCalls {
                    calls,
                    reasoning_content,
                } => {
                    let mut results = Vec::with_capacity(calls.len());
                    let save_snapshot = {
                        let mut w = store.write().await;
                        // Apply ops in order: drop → merge → edit (sort once).
                        let mut sorted = calls.clone();
                        sorted.sort_by_key(|c| match c.name.as_str() {
                            "drop_memory" => 0,
                            "merge_memories" => 1,
                            "edit_memory" => 2,
                            _ => 3,
                        });
                        for call in &sorted {
                            let out = w.execute_consolidator_tool(call, now);
                            match call.name.as_str() {
                                "merge_memories" if out.starts_with("Merged") => merged += 1,
                                "drop_memory" if out.starts_with("Dropped") => dropped += 1,
                                "edit_memory" if out.starts_with("Edited") => edited += 1,
                                _ => {}
                            }
                            info!(tool = %call.name, result = %out, "consolidation tool executed");
                            results.push(ToolResultMessage {
                                tool_call_id: call.id.clone(),
                                tool_name: call.name.clone(),
                                content: out,
                            });
                        }
                        w.clone()
                    };
                    save_snapshot.save(&store_path)?;
                    prior.push(ToolCallRound {
                        calls,
                        results,
                        reasoning_content,
                    });
                    if prior.len() > 5 {
                        warn!("consolidation exceeded 5 rounds; breaking");
                        break;
                    }
                }
            }
        }
    }

    info!(
        merged,
        dropped,
        edited,
        pre_filter_drops = hard_drops.len(),
        "consolidation pass complete"
    );
    Ok(())
}

/// Spawn the daily consolidation loop on a Tokio task. Sleeps until the next
/// Berlin-local occurrence of `run_at` and then invokes `run_consolidation`.
/// If the next occurrence has already passed for today, it is bumped by one
/// day so that restarts don't immediately re-trigger the pass. The task exits
/// cleanly when `shutdown` is notified. Errors inside `run_consolidation` are
/// logged at `warn!` and do not kill the loop.
pub fn spawn_consolidation(
    llm: Arc<dyn llm::LlmClient>,
    llm_config: ConsolidationLlmConfig,
    store: Arc<RwLock<MemoryStore>>,
    store_path: PathBuf,
    run_at: NaiveTime,
    timeout: Duration,
    shutdown: Arc<Notify>,
) {
    tokio::spawn(async move {
        let tz = chrono_tz::Europe::Berlin;
        loop {
            let now_local = chrono::Utc::now().with_timezone(&tz);
            let today = now_local.date_naive();
            // DST handling: Berlin spring-forward (e.g. 02:30 on the last
            // Sunday in March) maps to `None`; fall-back (02:30 on the last
            // Sunday in October) maps to `Ambiguous`. `.single()` previously
            // discarded both, falling back to `now_local` which scheduled the
            // run ~23h early. Prefer the earliest valid instant on ambiguity
            // and skip to tomorrow's occurrence on gap.
            let mut next = match today.and_time(run_at).and_local_timezone(tz) {
                MappedLocalTime::Single(t) => t,
                MappedLocalTime::Ambiguous(earliest, _latest) => earliest,
                MappedLocalTime::None => {
                    warn!(
                        %run_at,
                        "consolidation run_at falls in a DST gap today; picking tomorrow"
                    );
                    // DST gaps span ~1 hour; two consecutive days would require
                    // two transitions, which Berlin does not do. `expect` is safe.
                    (today + chrono::Duration::days(1))
                        .and_time(run_at)
                        .and_local_timezone(tz)
                        .earliest()
                        .expect("consecutive DST gaps are not possible in Europe/Berlin")
                }
            };
            if next <= now_local {
                next += chrono::Duration::days(1);
            }
            let wait = (next - now_local)
                .to_std()
                .unwrap_or(Duration::from_secs(60));
            tokio::select! {
                () = tokio::time::sleep(wait) => {
                    if let Err(e) = run_consolidation(
                        llm.clone(),
                        llm_config.clone(),
                        store.clone(),
                        store_path.clone(),
                        timeout,
                    )
                    .await
                    {
                        warn!(error = ?e, "consolidation run failed; will retry tomorrow");
                    }
                }
                () = shutdown.notified() => {
                    info!("consolidation task shutting down");
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::llm::{ChatCompletionRequest, ToolCall};
    use crate::ai::memory::Scope;
    use crate::ai::memory::store::Memory;
    use tempfile::TempDir;

    fn m_with(conf: u8, stale_days: i64, sources: Vec<String>) -> Memory {
        use chrono::Duration as CD;
        let now = Utc::now();
        Memory {
            fact: "x".into(),
            scope: Scope::Lore,
            sources,
            confidence: conf,
            created_at: now,
            updated_at: now,
            last_accessed: now - CD::days(stale_days),
            access_count: 0,
        }
    }

    #[test]
    fn hard_drop_catches_low_conf_stale() {
        let mut s = MemoryStore::default();
        s.memories
            .insert("a".into(), m_with(5, 90, vec!["alice".into()]));
        s.memories
            .insert("b".into(), m_with(50, 90, vec!["alice".into()]));
        s.memories
            .insert("c".into(), m_with(5, 30, vec!["alice".into()]));
        let drops = hard_drop_candidates(&s, Utc::now());
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].0, "a");
    }

    #[test]
    fn corroboration_boost_ignores_sentinels() {
        let m = m_with(
            70,
            0,
            vec!["alice".into(), "legacy".into(), "__identity__".into()],
        );
        // only alice counts; distinct=1 → bonus=0
        assert_eq!(corroboration_boost(&m), 70);
    }

    #[test]
    fn corroboration_boost_rewards_multiple_sources() {
        let m = m_with(70, 0, vec!["alice".into(), "bob".into(), "carol".into()]);
        // 3 distinct → +10
        assert_eq!(corroboration_boost(&m), 80);
    }

    #[test]
    fn corroboration_boost_clamps_at_100() {
        let m = m_with(
            95,
            0,
            vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "e".into(),
                "f".into(),
            ],
        );
        assert_eq!(corroboration_boost(&m), 100);
    }

    /// Minimal scripted LLM for unit-testing `run_consolidation`. The queue is
    /// populated once by the test; once drained, subsequent calls (the `lore`
    /// and `pref` scope passes) return the default `Message("done")` and the
    /// per-scope loop exits immediately.
    #[derive(Default)]
    struct ScriptedLlm {
        next: std::sync::Mutex<Vec<ToolChatCompletionResponse>>,
    }

    #[async_trait::async_trait]
    impl llm::LlmClient for ScriptedLlm {
        async fn chat_completion(&self, _r: ChatCompletionRequest) -> Result<String> {
            Ok(String::new())
        }
        async fn chat_completion_with_tools(
            &self,
            _r: ToolChatCompletionRequest,
        ) -> Result<ToolChatCompletionResponse> {
            let mut g = self.next.lock().unwrap();
            if g.is_empty() {
                Ok(ToolChatCompletionResponse::Message("done".into()))
            } else {
                Ok(g.remove(0))
            }
        }
    }

    #[tokio::test]
    async fn run_consolidation_applies_scripted_merge() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ai_memory.ron");
        let mut s = MemoryStore::default();
        let now = Utc::now();
        s.memories.insert(
            "user:1:a".into(),
            Memory::new(
                "plays tarkov".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "alice".into(),
                70,
                now,
            ),
        );
        s.memories.insert(
            "user:1:b".into(),
            Memory::new(
                "tarkov player".into(),
                Scope::User {
                    subject_id: "1".into(),
                },
                "bob".into(),
                60,
                now,
            ),
        );
        s.save(&path).unwrap();
        let store = Arc::new(RwLock::new(s));

        let fake = Arc::new(ScriptedLlm::default());
        fake.next
            .lock()
            .unwrap()
            .push(ToolChatCompletionResponse::ToolCalls {
                calls: vec![ToolCall {
                    id: "c".into(),
                    name: "merge_memories".into(),
                    arguments: serde_json::json!({
                        "keys": ["user:1:a", "user:1:b"],
                        "new_slug": "tarkov",
                        "new_fact": "plays Tarkov",
                    }),
                    arguments_parse_error: None,
                }],
                reasoning_content: None,
            });

        run_consolidation(
            fake,
            ConsolidationLlmConfig {
                model: "fake-model".into(),
                reasoning_effort: None,
            },
            store.clone(),
            path.clone(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        let r = store.read().await;
        assert!(r.memories.contains_key("user:1:tarkov"));
        assert!(!r.memories.contains_key("user:1:a"));
        assert!(!r.memories.contains_key("user:1:b"));
    }
}
