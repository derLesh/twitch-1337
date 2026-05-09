//! 7TV emote catalog + manual glossary support for AI prompt grounding.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr as _, bail};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::{APP_USER_AGENT, config::AiEmotesConfigSection};

const DEFAULT_BASE_URL: &str = "https://7tv.io/v3";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Lazily refreshes the available 7TV catalog and builds an LLM prompt block
/// from the intersection with a manual glossary.
#[derive(Debug)]
pub struct SevenTvEmoteProvider {
    http: reqwest::Client,
    base_url: String,
    glossary_path: PathBuf,
    include_global: bool,
    refresh_interval: Duration,
    max_prompt_emotes: usize,
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
    /// Build a provider from `[ai.emotes]`. Relative glossary paths resolve
    /// under the bot data directory.
    pub fn new(config: AiEmotesConfigSection, data_dir: &Path) -> Result<Self> {
        let glossary_path = PathBuf::from(&config.glossary_path);
        let glossary_path = if glossary_path.is_absolute() {
            glossary_path
        } else {
            data_dir.join(glossary_path)
        };

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
            glossary_path,
            include_global: config.include_global,
            refresh_interval: Duration::from_secs(config.refresh_interval_secs),
            max_prompt_emotes: config.max_prompt_emotes,
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
        build_prompt_block(&emotes, self.max_prompt_emotes, instruction, recent_chat)
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
        let glossary = self.load_glossary().await?;
        if glossary.emotes.is_empty() {
            debug!(
                path = %self.glossary_path.display(),
                "7TV emote glossary is empty"
            );
            return Ok(None);
        }

        let available = self.fetch_available_emotes(twitch_channel_id).await?;
        let emotes = build_available_prompt_emotes(&glossary.emotes, &available);
        Ok(emotes)
    }

    async fn load_glossary(&self) -> Result<Glossary> {
        let text = tokio::fs::read_to_string(&self.glossary_path)
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to read 7TV emote glossary at {}",
                    self.glossary_path.display()
                )
            })?;
        toml::from_str(&text).wrap_err("Failed to parse 7TV emote glossary")
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
    instruction: &str,
    recent_chat: &str,
) -> Option<String> {
    let lines = rank_prompt_emotes(emotes, instruction, recent_chat)
        .into_iter()
        .take(max_prompt_emotes)
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

fn rank_prompt_emotes<'a>(
    emotes: &'a [PromptEmote],
    instruction: &str,
    recent_chat: &str,
) -> Vec<&'a PromptEmote> {
    let context_terms = searchable_terms(instruction);
    let mut ranked = emotes
        .iter()
        .enumerate()
        .map(|(index, emote)| {
            let recent_count = recent_emote_count(recent_chat, &emote.name);
            let context_score = context_match_score(emote, &context_terms);
            (index, recent_count, context_score, emote)
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });

    ranked.into_iter().map(|(_, _, _, emote)| emote).collect()
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
        let prompt = build_prompt_block(&emotes, 40, "", "").unwrap();

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
            let prompt = build_prompt_block(&emotes, 40, "", "").unwrap();
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
        let prompt = build_prompt_block(&emotes, 1, "", "").unwrap();

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

        let prompt = build_prompt_block(&emotes, 2, "sag etwas lustiges", "").unwrap();

        let local_pos = prompt.find("- LocalEmote:").unwrap();
        let kekw_pos = prompt.find("- KEKW:").unwrap();
        assert!(
            kekw_pos < local_pos,
            "context-matching emote should outrank TOML order:\n{prompt}"
        );
    }
}
