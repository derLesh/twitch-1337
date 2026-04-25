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
    prompt: Option<String>,
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

    /// Return the current prompt block for the Twitch channel id, refreshing
    /// the backing catalog + glossary at most once per configured interval.
    pub async fn prompt_block(&self, twitch_channel_id: &str) -> Option<String> {
        let mut cache = self.cache.lock().await;
        let now = Instant::now();

        if cache
            .last_refresh
            .is_some_and(|last| now.duration_since(last) < self.refresh_interval)
        {
            return cache.prompt.clone();
        }

        match self.refresh_prompt(twitch_channel_id).await {
            Ok(prompt) => {
                cache.last_refresh = Some(now);
                cache.prompt = prompt;
            }
            Err(e) => {
                cache.last_refresh = Some(now);
                warn!(
                    error = ?e,
                    "Failed to refresh 7TV emote prompt; using cached prompt if available"
                );
            }
        }

        cache.prompt.clone()
    }

    async fn refresh_prompt(&self, twitch_channel_id: &str) -> Result<Option<String>> {
        let glossary = self.load_glossary().await?;
        if glossary.emotes.is_empty() {
            debug!(
                path = %self.glossary_path.display(),
                "7TV emote glossary is empty"
            );
            return Ok(None);
        }

        let available = self.fetch_available_emotes(twitch_channel_id).await?;
        let prompt = build_prompt_block(&glossary.emotes, &available, self.max_prompt_emotes);
        Ok(prompt)
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

fn build_prompt_block(
    glossary: &[GlossaryEmote],
    available: &HashSet<String>,
    max_prompt_emotes: usize,
) -> Option<String> {
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
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

        let mut line = format!("- {name}: meaning={meaning}");
        if let Some(usage) = emote
            .usage
            .as_deref()
            .map(normalize_prompt_field)
            .filter(|s| !s.is_empty())
        {
            line.push_str("; use=");
            line.push_str(&usage);
        }
        if let Some(avoid) = emote
            .avoid
            .as_deref()
            .map(normalize_prompt_field)
            .filter(|s| !s.is_empty())
        {
            line.push_str("; avoid=");
            line.push_str(&avoid);
        }
        lines.push(line);

        if lines.len() >= max_prompt_emotes {
            break;
        }
    }

    if stale_count > 0 {
        debug!(
            missing_count = stale_count,
            "7TV emote glossary contains entries not present in the loaded catalog"
        );
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "\n\n7TV emotes available in this channel:\nUse only these exact emote codes, only when they naturally match the tone. Do not explain emotes. Use at most one or two emotes per response, and skip emotes for serious topics.\n{}",
        lines.join("\n")
    ))
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

        let prompt = build_prompt_block(&glossary, &available, 40).unwrap();

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
            let prompt = build_prompt_block(&glossary, &available, 40).unwrap();
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

        let prompt = build_prompt_block(&glossary, &available, 1).unwrap();

        assert!(prompt.contains("A"));
        assert!(!prompt.contains("B"));
    }
}
