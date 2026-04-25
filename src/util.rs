use std::path::PathBuf;

use chrono::MappedLocalTime;
use eyre::Result;

/// Application user-agent string used in HTTP requests.
pub static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

/// Maximum response length for Twitch chat (to stay within limits).
pub const MAX_RESPONSE_LENGTH: usize = 500;

/// Returns the data directory path, resolved from `$DATA_DIR` env var.
///
/// Defaults to `/var/lib/twitch-1337` when the env var is not set.
pub fn get_data_dir() -> PathBuf {
    std::env::var("DATA_DIR")
        .unwrap_or_else(|_| "/var/lib/twitch-1337".to_string())
        .into()
}

/// Returns the path to the config file within the data directory.
pub fn get_config_path() -> PathBuf {
    get_data_dir().join("config.toml")
}

/// Create the data directory if it does not exist.
pub async fn ensure_data_dir() -> Result<()> {
    tokio::fs::create_dir_all(get_data_dir()).await?;
    Ok(())
}

/// Resolves a naive datetime to Berlin local time, handling DST transitions.
///
/// During spring-forward (gap), interprets as UTC to land just after the gap.
/// During fall-back (ambiguous), picks the later occurrence.
pub fn resolve_berlin_time(naive: chrono::NaiveDateTime) -> chrono::DateTime<chrono_tz::Tz> {
    match naive.and_local_timezone(chrono_tz::Europe::Berlin) {
        MappedLocalTime::Single(t) => t,
        MappedLocalTime::Ambiguous(_, latest) => latest,
        MappedLocalTime::None => naive.and_utc().with_timezone(&chrono_tz::Europe::Berlin),
    }
}

/// Truncates a string to the maximum number of characters at a word boundary.
pub fn truncate_response(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");

    let byte_limit = match collapsed.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => byte_idx,
        None => return collapsed,
    };

    let truncated = &collapsed[..byte_limit];
    if let Some(last_space) = truncated.rfind(' ') {
        format!("{}...", &truncated[..last_space])
    } else {
        format!("{}...", truncated)
    }
}

/// Parse a compact duration string like "1h", "30m", "2h30m" into a Duration.
pub fn parse_flight_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else {
            match ch.to_ascii_lowercase() {
                'h' => {
                    let hours: u64 = current_num.parse().ok()?;
                    total_secs += hours * 3600;
                    current_num.clear();
                }
                'm' => {
                    let minutes: u64 = current_num.parse().ok()?;
                    total_secs += minutes * 60;
                    current_num.clear();
                }
                _ => return None,
            }
        }
    }

    if !current_num.is_empty() || total_secs == 0 {
        return None;
    }

    Some(std::time::Duration::from_secs(total_secs))
}
