//! 7TV emote catalog + manual glossary support for AI prompt grounding.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr as _, bail};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::{APP_USER_AGENT, config::AiEmotesConfigSection};

const DEFAULT_BASE_URL: &str = "https://7tv.io/v3";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Manual glossary baked into the binary at build time. Curated alongside the
/// rest of the codebase; updates ship with the binary, not as a runtime file.
pub const BAKED_GLOSSARY_TOML: &str = include_str!("../../data/7tv_emotes.toml");

/// Lazily refreshes the available 7TV catalog and builds an LLM prompt block
/// from the intersection with a manual glossary.
#[derive(Debug)]
pub struct SevenTvEmoteProvider {
    http: reqwest::Client,
    base_url: String,
    glossary: Vec<GlossaryEmote>,
    include_global: bool,
    refresh_interval: Duration,
    max_prompt_emotes: usize,
    min_baseline_emotes: usize,
    cache: Mutex<PromptCache>,
}

#[derive(Debug, Default)]
struct PromptCache {
    last_refresh: Option<Instant>,
    emotes: Option<Vec<PromptEmote>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Glossary {
    #[serde(default)]
    emotes: Vec<GlossaryEmote>,
}

#[derive(Debug, Clone, Deserialize)]
struct GlossaryEmote {
    name: String,
    meaning: String,
    #[serde(default)]
    usage: Option<String>,
    #[serde(default)]
    avoid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptEmote {
    name: String,
    meaning: String,
    usage: Option<String>,
    avoid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SevenTvUser {
    #[serde(default)]
    emote_set: Option<SevenTvEmoteSet>,
}

#[derive(Debug, Clone, Deserialize)]
struct SevenTvEmoteSet {
    #[serde(default)]
    emotes: Vec<SevenTvEmote>,
}

#[derive(Debug, Clone, Deserialize)]
struct SevenTvEmote {
    name: String,
}

impl SevenTvEmoteProvider {
    /// Build a provider from `[ai.emotes]` and a TOML glossary string.
    ///
    /// Production code passes [`BAKED_GLOSSARY_TOML`]; integration tests pass
    /// a custom fixture. The glossary is parsed eagerly so malformed TOML
    /// fails the bot at startup instead of silently disabling emotes.
    pub fn new(config: AiEmotesConfigSection, glossary_toml: &str) -> Result<Self> {
        let glossary: Glossary =
            toml::from_str(glossary_toml).wrap_err("Failed to parse 7TV emote glossary")?;

        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .wrap_err("Failed to build 7TV HTTP client")?;

        Ok(Self {
            http,
            base_url: config
                .base_url
                .as_deref()
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
            glossary: glossary.emotes,
            include_global: config.include_global,
            refresh_interval: Duration::from_secs(config.refresh_interval_secs),
            max_prompt_emotes: config.max_prompt_emotes,
            min_baseline_emotes: config.min_baseline_emotes.min(config.max_prompt_emotes),
            cache: Mutex::new(PromptCache::default()),
        })
    }

    /// Return a turn-specific prompt block for the Twitch channel id.
    ///
    /// The backing catalog + glossary are refreshed at most once per
    /// configured interval, but ranking happens per turn so current chat emotes
    /// and the user instruction can influence which entries the model sees.
    pub async fn prompt_block_for_turn(
        &self,
        twitch_channel_id: &str,
        instruction: &str,
        recent_chat: &str,
    ) -> Option<String> {
        let emotes = self.prompt_emotes(twitch_channel_id).await?;
        build_prompt_block(
            &emotes,
            self.max_prompt_emotes,
            self.min_baseline_emotes,
            instruction,
            recent_chat,
        )
    }

    async fn prompt_emotes(&self, twitch_channel_id: &str) -> Option<Vec<PromptEmote>> {
        let mut cache = self.cache.lock().await;
        let now = Instant::now();

        if cache
            .last_refresh
            .is_some_and(|last| now.duration_since(last) < self.refresh_interval)
        {
            return cache.emotes.clone();
        }

        match self.refresh_prompt_emotes(twitch_channel_id).await {
            Ok(emotes) => {
                cache.last_refresh = Some(now);
                cache.emotes = emotes;
            }
            Err(e) => {
                cache.last_refresh = Some(now);
                warn!(
                    error = ?e,
                    "Failed to refresh 7TV emote glossary; using cached entries if available"
                );
            }
        }

        cache.emotes.clone()
    }

    async fn refresh_prompt_emotes(
        &self,
        twitch_channel_id: &str,
    ) -> Result<Option<Vec<PromptEmote>>> {
        if self.glossary.is_empty() {
            debug!("7TV emote glossary is empty");
            return Ok(None);
        }

        let available = self.fetch_available_emotes(twitch_channel_id).await?;
        let emotes = build_available_prompt_emotes(&self.glossary, &available);
        Ok(emotes)
    }

    async fn fetch_available_emotes(&self, twitch_channel_id: &str) -> Result<HashSet<String>> {
        let mut global = Vec::new();
        let mut channel = Vec::new();
        let mut had_error = false;

        if self.include_global {
            match self.fetch_global_emotes().await {
                Ok(emotes) => global = emotes,
                Err(e) => {
                    warn!(error = ?e, "Failed to fetch global 7TV emotes");
                    had_error = true;
                }
            }
        }

        match self.fetch_channel_emotes(twitch_channel_id).await {
            Ok(emotes) => channel = emotes,
            Err(e) => {
                warn!(
                    error = ?e,
                    twitch_channel_id,
                    "Failed to fetch channel 7TV emotes"
                );
                had_error = true;
            }
        }

        if global.is_empty() && channel.is_empty() && had_error {
            bail!("all configured 7TV catalog fetches failed");
        }

        Ok(merge_emote_sets(global, channel))
    }

    async fn fetch_global_emotes(&self) -> Result<Vec<SevenTvEmote>> {
        let url = format!("{}/emote-sets/global", self.base_url);
        let set: SevenTvEmoteSet = self.get_json(&url).await?;
        Ok(set.emotes)
    }

    async fn fetch_channel_emotes(&self, twitch_channel_id: &str) -> Result<Vec<SevenTvEmote>> {
        let url = format!("{}/users/twitch/{}", self.base_url, twitch_channel_id);
        let user: SevenTvUser = self.get_json(&url).await?;
        Ok(user.emote_set.map(|set| set.emotes).unwrap_or_default())
    }

    async fn get_json<T>(&self, url: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .wrap_err_with(|| format!("Failed to send 7TV request to {url}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("7TV request failed with status {status}: {body}");
        }

        response
            .json()
            .await
            .wrap_err_with(|| format!("Failed to parse 7TV response from {url}"))
    }
}

fn merge_emote_sets(global: Vec<SevenTvEmote>, channel: Vec<SevenTvEmote>) -> HashSet<String> {
    let mut available = HashSet::new();

    for emote in global {
        insert_available(&mut available, emote);
    }
    for emote in channel {
        insert_available(&mut available, emote);
    }

    available
}

fn insert_available(available: &mut HashSet<String>, emote: SevenTvEmote) {
    if emote.name.trim().is_empty() {
        return;
    }

    available.insert(emote.name);
}

fn build_available_prompt_emotes(
    glossary: &[GlossaryEmote],
    available: &HashSet<String>,
) -> Option<Vec<PromptEmote>> {
    let mut seen = HashSet::new();
    let mut emotes = Vec::new();
    let mut stale_count = 0usize;

    for emote in glossary {
        let name = emote.name.trim();
        if name.is_empty() || !seen.insert(name.to_string()) {
            continue;
        }

        if !available.contains(name) {
            stale_count += 1;
            continue;
        }

        let meaning = normalize_prompt_field(&emote.meaning);
        if meaning.is_empty() {
            warn!(
                emote = name,
                "Skipping 7TV emote glossary entry with empty meaning"
            );
            continue;
        }

        let usage = emote
            .usage
            .as_deref()
            .map(normalize_prompt_field)
            .filter(|s| !s.is_empty());
        let avoid = emote
            .avoid
            .as_deref()
            .map(normalize_prompt_field)
            .filter(|s| !s.is_empty());
        emotes.push(PromptEmote {
            name: name.to_string(),
            meaning,
            usage,
            avoid,
        });
    }

    if stale_count > 0 {
        debug!(
            missing_count = stale_count,
            "7TV emote glossary contains entries not present in the loaded catalog"
        );
    }

    if emotes.is_empty() {
        return None;
    }

    Some(emotes)
}

fn build_prompt_block(
    emotes: &[PromptEmote],
    max_prompt_emotes: usize,
    min_baseline_emotes: usize,
    instruction: &str,
    recent_chat: &str,
) -> Option<String> {
    let lines = select_prompt_emotes(
        emotes,
        max_prompt_emotes,
        min_baseline_emotes,
        instruction,
        recent_chat,
    )
    .into_iter()
    .map(format_prompt_emote_line)
    .collect::<Vec<_>>();

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "\n\n7TV emotes available in this channel:\nUse only these exact emote codes. In normal casual Twitch-chat replies, include exactly one fitting emote by default. Use zero emotes only for extremely serious, administrative, fact-sensitive, or clearly unsuitable topics. Use two emotes only when the chat moment is obviously hype, chaotic, or spammy. Prefer emotes recently used by chat when they fit. Do not invent or explain emotes.\n{}",
        lines.join("\n")
    ))
}

/// Pick which emotes to inject this turn. Scoring emotes (anything seen in
/// recent chat, or whose meaning/usage shares 4+ char terms with the current
/// instruction) come first, capped by `max_prompt_emotes`. If fewer than
/// `min_baseline_emotes` made the cut, fill the gap with glossary-order
/// fallbacks so the model always has a baseline vocabulary. The whole list
/// stays capped by `max_prompt_emotes`.
fn select_prompt_emotes<'a>(
    emotes: &'a [PromptEmote],
    max_prompt_emotes: usize,
    min_baseline_emotes: usize,
    instruction: &str,
    recent_chat: &str,
) -> Vec<&'a PromptEmote> {
    if max_prompt_emotes == 0 {
        return Vec::new();
    }
    let context_terms = searchable_terms(instruction);
    let scored: Vec<(usize, usize, usize, &PromptEmote)> = emotes
        .iter()
        .enumerate()
        .map(|(index, emote)| {
            let recent_count = recent_emote_count(recent_chat, &emote.name);
            let context_score = context_match_score(emote, &context_terms);
            (index, recent_count, context_score, emote)
        })
        .collect();

    let mut scoring: Vec<&(usize, usize, usize, &PromptEmote)> = scored
        .iter()
        .filter(|(_, r, c, _)| *r > 0 || *c > 0)
        .collect();
    scoring.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });

    let baseline_floor = min_baseline_emotes.min(max_prompt_emotes);
    let mut picked: Vec<&PromptEmote> = Vec::with_capacity(max_prompt_emotes);
    let mut picked_indexes: HashSet<usize> = HashSet::new();
    for entry in scoring.iter().take(max_prompt_emotes) {
        picked.push(entry.3);
        picked_indexes.insert(entry.0);
    }
    if picked.len() < baseline_floor {
        for (index, _, _, emote) in &scored {
            if picked.len() >= baseline_floor {
                break;
            }
            if picked_indexes.insert(*index) {
                picked.push(emote);
            }
        }
    }
    picked
}

fn format_prompt_emote_line(emote: &PromptEmote) -> String {
    let mut line = format!("- {}: meaning={}", emote.name, emote.meaning);
    if let Some(usage) = emote.usage.as_deref() {
        line.push_str("; use=");
        line.push_str(usage);
    }
    if let Some(avoid) = emote.avoid.as_deref() {
        line.push_str("; avoid=");
        line.push_str(avoid);
    }
    line
}

fn recent_emote_count(recent_chat: &str, emote_name: &str) -> usize {
    recent_chat
        .split_whitespace()
        .filter(|token| token_matches_emote(token, emote_name))
        .count()
}

fn token_matches_emote(token: &str, emote_name: &str) -> bool {
    token == emote_name || trim_wrapping_chat_punctuation(token) == emote_name
}

fn trim_wrapping_chat_punctuation(token: &str) -> &str {
    token.trim_matches(|c| {
        matches!(
            c,
            ',' | '.' | '!' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    })
}

fn context_match_score(emote: &PromptEmote, context_terms: &[String]) -> usize {
    if context_terms.is_empty() {
        return 0;
    }

    let mut fields = emote.meaning.clone();
    if let Some(usage) = emote.usage.as_deref() {
        fields.push(' ');
        fields.push_str(usage);
    }
    let emote_terms = searchable_terms(&fields);

    context_terms
        .iter()
        .filter(|query| emote_terms.iter().any(|term| terms_match(query, term)))
        .count()
}

fn searchable_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|term| term.len() >= 4 && !is_context_stopword(term))
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

fn is_context_stopword(term: &str) -> bool {
    matches!(
        term,
        "eine"
            | "einer"
            | "einem"
            | "einen"
            | "etwas"
            | "wenn"
            | "oder"
            | "nicht"
            | "bitte"
            | "reply"
            | "message"
            | "author"
            | "parent"
            | "user"
            | "chat"
            | "channel"
    )
}

fn terms_match(query: &str, term: &str) -> bool {
    query == term
        || (query.len() >= 5
            && term.len() >= 5
            && (query.starts_with(term) || term.starts_with(query)))
}

fn normalize_prompt_field(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_global_emote_set_response() {
        let json = serde_json::json!({
            "id": "global",
            "emotes": [
                {"id": "1", "name": "KEKW", "data": {"name": "ignored"}},
                {"id": "2", "name": "peepoHappy"}
            ]
        });

        let parsed: SevenTvEmoteSet = serde_json::from_value(json).unwrap();

        assert_eq!(parsed.emotes.len(), 2);
        assert_eq!(parsed.emotes[0].name, "KEKW");
        assert_eq!(parsed.emotes[1].name, "peepoHappy");
    }

    #[test]
    fn parses_user_response_with_missing_emote_set() {
        let json = serde_json::json!({
            "id": "user",
            "emote_set": null
        });

        let parsed: SevenTvUser = serde_json::from_value(json).unwrap();

        assert!(parsed.emote_set.is_none());
    }

    #[test]
    fn merge_deduplicates_global_and_channel_emotes() {
        let global = vec![SevenTvEmote {
            name: "KEKW".into(),
        }];
        let channel = vec![SevenTvEmote {
            name: "KEKW".into(),
        }];

        let merged = merge_emote_sets(global, channel);

        assert_eq!(merged.len(), 1);
        assert!(merged.contains("KEKW"));
    }

    #[test]
    fn prompt_contains_only_glossary_entries_available_in_catalog() {
        let glossary = vec![
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "lachen".into(),
                usage: Some("wenn etwas lustig ist".into()),
                avoid: Some("ernste Themen".into()),
            },
            GlossaryEmote {
                name: "Missing".into(),
                meaning: "not available".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![SevenTvEmote {
                name: "KEKW".into(),
            }],
            Vec::new(),
        );

        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();
        let prompt = build_prompt_block(&emotes, 40, 40, "", "").unwrap();

        assert!(prompt.contains("KEKW"));
        assert!(prompt.contains("meaning=lachen"));
        assert!(!prompt.contains("Missing"));
    }

    #[test]
    fn stale_glossary_entries_emit_one_debug_summary() {
        use std::sync::{Arc, Mutex};
        use tracing::{
            Event, Level, Subscriber,
            field::{Field, Visit},
        };
        use tracing_subscriber::{
            layer::{Context, Layer},
            prelude::*,
        };

        #[derive(Clone, Default)]
        struct CaptureLayer {
            events: Arc<Mutex<Vec<CapturedEvent>>>,
        }

        #[derive(Debug)]
        struct CapturedEvent {
            level: Level,
            fields: Vec<(String, String)>,
        }

        #[derive(Default)]
        struct FieldVisitor {
            fields: Vec<(String, String)>,
        }

        impl Visit for FieldVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.fields
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }

        impl<S> Layer<S> for CaptureLayer
        where
            S: Subscriber,
        {
            fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
                let mut visitor = FieldVisitor::default();
                event.record(&mut visitor);
                self.events.lock().unwrap().push(CapturedEvent {
                    level: *event.metadata().level(),
                    fields: visitor.fields,
                });
            }
        }

        let glossary = vec![
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "laughter".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "MissingA".into(),
                meaning: "missing".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "MissingB".into(),
                meaning: "missing".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![SevenTvEmote {
                name: "KEKW".into(),
            }],
            Vec::new(),
        );
        let capture = CaptureLayer::default();
        let events = Arc::clone(&capture.events);
        let subscriber = tracing_subscriber::registry().with(capture);

        tracing::subscriber::with_default(subscriber, || {
            let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();
            let prompt = build_prompt_block(&emotes, 40, 40, "", "").unwrap();
            assert!(prompt.contains("KEKW"));
            assert!(!prompt.contains("MissingA"));
            assert!(!prompt.contains("MissingB"));
        });

        let events = events.lock().unwrap();
        let stale_events = events
            .iter()
            .filter(|event| {
                event.fields.iter().any(|(name, value)| {
                    name == "message"
                        && value.contains(
                            "7TV emote glossary contains entries not present in the loaded catalog",
                        )
                })
            })
            .collect::<Vec<_>>();

        assert_eq!(stale_events.len(), 1);
        assert_eq!(stale_events[0].level, Level::DEBUG);
        assert!(
            stale_events[0]
                .fields
                .iter()
                .any(|(name, value)| name == "missing_count" && value == "2")
        );
    }

    #[test]
    fn prompt_respects_max_prompt_emotes() {
        let glossary = vec![
            GlossaryEmote {
                name: "A".into(),
                meaning: "first".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "B".into(),
                meaning: "second".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote { name: "A".into() },
                SevenTvEmote { name: "B".into() },
            ],
            Vec::new(),
        );

        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();
        let prompt = build_prompt_block(&emotes, 1, 1, "", "").unwrap();

        assert!(prompt.contains("A"));
        assert!(!prompt.contains("B"));
    }

    #[test]
    fn prompt_prioritizes_emotes_seen_in_recent_chat() {
        let glossary = vec![
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "lachen".into(),
                usage: Some("wenn etwas lustig ist".into()),
                avoid: None,
            },
            GlossaryEmote {
                name: "LocalEmote".into(),
                meaning: "lokaler Channel-Insider".into(),
                usage: Some("wenn der Chat den Insider anspricht".into()),
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote {
                    name: "KEKW".into(),
                },
                SevenTvEmote {
                    name: "LocalEmote".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        let prompt = build_prompt_block(
            &emotes,
            2,
            2,
            "sag etwas lustiges",
            "## Recent chat (#main)\n[13:37] bob: LocalEmote",
        )
        .unwrap();

        let local_pos = prompt.find("- LocalEmote:").unwrap();
        let kekw_pos = prompt.find("- KEKW:").unwrap();
        assert!(
            local_pos < kekw_pos,
            "recent chat emote should rank first:\n{prompt}"
        );
    }

    #[test]
    fn prompt_prioritizes_context_matches_when_chat_is_neutral() {
        let glossary = vec![
            GlossaryEmote {
                name: "LocalEmote".into(),
                meaning: "lokaler Channel-Insider".into(),
                usage: Some("wenn der Chat den Insider anspricht".into()),
                avoid: None,
            },
            GlossaryEmote {
                name: "KEKW".into(),
                meaning: "lachen, etwas ist lustig".into(),
                usage: Some("bei Witzen oder Fail-Momenten".into()),
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote {
                    name: "LocalEmote".into(),
                },
                SevenTvEmote {
                    name: "KEKW".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        let prompt = build_prompt_block(&emotes, 2, 2, "sag etwas lustiges", "").unwrap();

        let local_pos = prompt.find("- LocalEmote:").unwrap();
        let kekw_pos = prompt.find("- KEKW:").unwrap();
        assert!(
            kekw_pos < local_pos,
            "context-matching emote should outrank TOML order:\n{prompt}"
        );
    }

    #[test]
    fn prompt_drops_zero_score_emotes_when_baseline_is_zero() {
        let glossary = vec![
            GlossaryEmote {
                name: "Hit".into(),
                meaning: "lustig".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "Idle".into(),
                meaning: "nichts dergleichen".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote { name: "Hit".into() },
                SevenTvEmote {
                    name: "Idle".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        // "lustig" matches the instruction terms; "Idle" scores zero.
        let prompt = build_prompt_block(&emotes, 8, 0, "etwas lustiges", "").unwrap();

        assert!(prompt.contains("- Hit:"));
        assert!(
            !prompt.contains("- Idle:"),
            "zero-score emote leaked through:\n{prompt}"
        );
    }

    #[test]
    fn prompt_baseline_floor_fills_in_glossary_order_when_no_scoring_hits() {
        let glossary = vec![
            GlossaryEmote {
                name: "First".into(),
                meaning: "platzhalter".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "Second".into(),
                meaning: "platzhalter".into(),
                usage: None,
                avoid: None,
            },
            GlossaryEmote {
                name: "Third".into(),
                meaning: "platzhalter".into(),
                usage: None,
                avoid: None,
            },
        ];
        let available = merge_emote_sets(
            vec![
                SevenTvEmote {
                    name: "First".into(),
                },
                SevenTvEmote {
                    name: "Second".into(),
                },
                SevenTvEmote {
                    name: "Third".into(),
                },
            ],
            Vec::new(),
        );
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        // No instruction terms ≥4 chars and no recent chat → nothing scores.
        let prompt = build_prompt_block(&emotes, 8, 2, "hi", "").unwrap();

        // Exactly the baseline floor, in glossary order.
        assert!(prompt.contains("- First:"));
        assert!(prompt.contains("- Second:"));
        assert!(
            !prompt.contains("- Third:"),
            "baseline floor exceeded:\n{prompt}"
        );
    }

    #[test]
    fn prompt_block_returns_none_when_max_prompt_emotes_is_zero() {
        let glossary = vec![GlossaryEmote {
            name: "A".into(),
            meaning: "x".into(),
            usage: None,
            avoid: None,
        }];
        let available = merge_emote_sets(vec![SevenTvEmote { name: "A".into() }], Vec::new());
        let emotes = build_available_prompt_emotes(&glossary, &available).unwrap();

        assert!(build_prompt_block(&emotes, 0, 0, "hi", "").is_none());
    }
}
