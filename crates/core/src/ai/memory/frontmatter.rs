//! Hand-rolled YAML-ish frontmatter parser. Schema is fixed:
//! `updated_at` (RFC3339, required), `username` (optional), `display_name`
//! (optional), `created_by` (optional).

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::ai::memory::types::Frontmatter;

#[derive(Debug, Error)]
pub enum FrontmatterError {
    #[error("missing or malformed frontmatter delimiters")]
    Delimiters,
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid timestamp: {0}")]
    BadTimestamp(String),
}

pub fn parse(raw: &str) -> Result<(Frontmatter, String), FrontmatterError> {
    let body_start;
    let yaml = if let Some(rest) = raw.strip_prefix("---\n") {
        let end = rest.find("\n---\n").ok_or(FrontmatterError::Delimiters)?;
        body_start = "---\n".len() + end + "\n---\n".len();
        &rest[..end]
    } else {
        return Err(FrontmatterError::Delimiters);
    };

    let mut updated_at = None;
    let mut username = None;
    let mut display_name = None;
    let mut created_by = None;
    for line in yaml.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "updated_at" => {
                let dt = DateTime::parse_from_rfc3339(v)
                    .map_err(|e| FrontmatterError::BadTimestamp(e.to_string()))?
                    .with_timezone(&Utc);
                updated_at = Some(dt);
            }
            "username" if !v.is_empty() => username = Some(v.to_string()),
            "display_name" if !v.is_empty() => display_name = Some(v.to_string()),
            "created_by" if !v.is_empty() => created_by = Some(v.to_string()),
            _ => {}
        }
    }

    Ok((
        Frontmatter {
            updated_at: updated_at.ok_or(FrontmatterError::MissingField("updated_at"))?,
            username,
            display_name,
            created_by,
        },
        raw[body_start..].to_string(),
    ))
}

pub fn emit(fm: &Frontmatter, body: &str) -> String {
    let mut s = String::with_capacity(body.len() + 128);
    s.push_str("---\n");
    s.push_str(&format!(
        "updated_at: {}\n",
        fm.updated_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    ));
    if let Some(ref u) = fm.username {
        s.push_str(&format!("username: {u}\n"));
    }
    if let Some(ref n) = fm.display_name {
        s.push_str(&format!("display_name: {n}\n"));
    }
    if let Some(ref c) = fm.created_by {
        s.push_str(&format!("created_by: {c}\n"));
    }
    s.push_str("---\n");
    s.push_str(body);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::memory::types::Frontmatter;
    use chrono::TimeZone as _;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(2026, 4, 28, 18, 42, 0)
            .unwrap()
    }

    #[test]
    fn split_returns_frontmatter_and_body() {
        let raw = "---\nupdated_at: 2026-04-28T18:42:00Z\nusername: alicepleb\ndisplay_name: AlicePleb\n---\nhello\nworld\n";
        let (fm, body) = parse(raw).unwrap();
        assert_eq!(fm.updated_at, ts());
        assert_eq!(fm.username.as_deref(), Some("alicepleb"));
        assert_eq!(fm.display_name.as_deref(), Some("AlicePleb"));
        assert_eq!(body, "hello\nworld\n");
    }

    #[test]
    fn missing_frontmatter_errors() {
        assert!(parse("hello\n").is_err());
        assert!(parse("---\nno-end\n").is_err());
    }

    #[test]
    fn emit_round_trips() {
        let fm = Frontmatter {
            updated_at: ts(),
            username: Some("alice".into()),
            display_name: Some("Alice".into()),
            created_by: None,
        };
        let raw = emit(&fm, "body line\n");
        let (fm2, body2) = parse(&raw).unwrap();
        assert_eq!(fm, fm2);
        assert_eq!(body2, "body line\n");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let raw = "---\nupdated_at: 2026-04-28T18:42:00Z\nfoo: bar\n---\nbody\n";
        let (fm, body) = parse(raw).unwrap();
        assert_eq!(fm.updated_at, ts());
        assert_eq!(body, "body\n");
    }
}
