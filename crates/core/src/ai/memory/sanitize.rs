//! Pure validation helpers for memory paths, state slugs, and file bodies.
//!
//! Centralised so the store and the tool dispatcher share one source of truth.
//! Errors round-trip back to the LLM as the canonical strings the spec
//! mandates (`invalid_path`, `invalid_slug`, `reserved_slug`, `invalid_body`).

use thiserror::Error;

use crate::ai::memory::types::FileKind;

const RESERVED: &[&str] = &[
    "soul",
    "lore",
    "system",
    "admin",
    "assistant",
    "user",
    "tool",
    "dreamer",
    "prompt",
    "instructions",
    // Dashboard route literals: a state note named `new` would shadow the
    // create form, `delete` would shadow the per-slug delete URL.
    "new",
    "delete",
];
const FENCE_OPEN: &str = "<<<FILE ";
const FENCE_CLOSE: &str = "<<<ENDFILE ";

/// Paths the LLM may write to via `write_file`. Excludes `state/<slug>.md`
/// because state has its own pair of tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritePath {
    Soul,
    Lore,
    User { user_id: String },
}

#[derive(Debug, Error)]
#[error("invalid_path")]
pub struct PathError;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SlugError {
    #[error("invalid_slug")]
    Invalid,
    #[error("reserved_slug")]
    Reserved,
}

#[derive(Debug, Error)]
#[error("invalid_body")]
pub struct BodyError;

pub fn parse_write_path(path: &str) -> Result<WritePath, PathError> {
    if path == "SOUL.md" {
        return Ok(WritePath::Soul);
    }
    if path == "LORE.md" {
        return Ok(WritePath::Lore);
    }
    if let Some(rest) = path.strip_prefix("users/")
        && let Some(id) = rest.strip_suffix(".md")
        && !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
    {
        return Ok(WritePath::User {
            user_id: id.to_string(),
        });
    }
    Err(PathError)
}

pub fn parse_slug(slug: &str) -> Result<String, SlugError> {
    // Check reserved first so that case-insensitive matches (e.g. "SOUL") return
    // `Reserved` rather than `Invalid` regardless of casing.
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(slug)) {
        return Err(SlugError::Reserved);
    }
    let valid_first = slug
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let valid_rest = slug
        .bytes()
        .skip(1)
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid_first || !valid_rest || slug.len() > 64 {
        return Err(SlugError::Invalid);
    }
    Ok(slug.to_string())
}

/// True iff `slug` ends in `-YYYY-MM-DD`. Forbidden on writes so memory
/// entries get stable slugs the dreamer can rewrite in place, rather than
/// one slug per day piling up indefinitely. Deletes still accept dated
/// slugs so the dreamer can drain the existing backlog.
pub fn has_trailing_iso_date(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.len() < 11 {
        return false;
    }
    let tail = &bytes[bytes.len() - 11..];
    tail[0] == b'-'
        && tail[1..5].iter().all(u8::is_ascii_digit)
        && tail[5] == b'-'
        && tail[6..8].iter().all(u8::is_ascii_digit)
        && tail[8] == b'-'
        && tail[9..11].iter().all(u8::is_ascii_digit)
}

pub fn check_body(body: &str) -> Result<(), BodyError> {
    // Reject CR outright. CRLF would otherwise let `\r\n---\r\n` slip past the
    // LF-only frontmatter-reopen check below; markdown bodies have no need for
    // CR, so the easiest hardening is to ban it.
    if body.contains('\r') {
        return Err(BodyError);
    }
    if body.starts_with("---") {
        return Err(BodyError);
    }
    if body.contains("\n---\n") || body.ends_with("\n---") {
        return Err(BodyError);
    }
    for line in body.lines() {
        if line.starts_with("# SOUL")
            || line.starts_with("# LORE")
            || line.starts_with("# users/")
            || line.starts_with("# state/")
        {
            return Err(BodyError);
        }
    }
    if body.contains(FENCE_OPEN) || body.contains(FENCE_CLOSE) {
        return Err(BodyError);
    }
    Ok(())
}

/// Strip ASCII control chars, zero-width chars, and bidi overrides; cap at 64 chars.
pub fn normalize_display_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .filter(|&c| {
            !matches!(c,
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}')
        })
        .take(64)
        .collect()
}

pub fn write_path_to_kind(p: WritePath) -> FileKind {
    match p {
        WritePath::Soul => FileKind::Soul,
        WritePath::Lore => FileKind::Lore,
        WritePath::User { user_id } => FileKind::User { user_id },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_path_accepts_canonical() {
        assert_eq!(parse_write_path("SOUL.md").unwrap(), WritePath::Soul);
        assert_eq!(parse_write_path("LORE.md").unwrap(), WritePath::Lore);
        assert_eq!(
            parse_write_path("users/12345.md").unwrap(),
            WritePath::User {
                user_id: "12345".into()
            }
        );
    }

    #[test]
    fn write_path_rejects_traversal_and_typos() {
        for bad in [
            "soul.md",
            "users/abc.md",
            "users/",
            "users/12.md/",
            "../etc/passwd",
            "users/12345.md/",
            "state/foo.md",
            "users/12345.markdown",
        ] {
            assert!(parse_write_path(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn slug_accepts_canonical_and_rejects_others() {
        assert!(parse_slug("quiz-night").is_ok());
        assert!(parse_slug("a").is_ok());
        for bad in ["", "-foo", "Foo", "fo o", "a".repeat(65).as_str(), "../x"] {
            assert!(parse_slug(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn has_trailing_iso_date_matches_yyyy_mm_dd_suffix() {
        for s in [
            "av-depot-2026-05-08",
            "hantavirus-lockdown-2026-05-06",
            "x-1999-12-31",
        ] {
            assert!(has_trailing_iso_date(s), "expected dated: {s}");
        }
        for s in [
            "magie-023-schufa",
            "quiz-2026",
            "av-depot",
            "av-depot-202-05-08",
            "2026-05-08",
        ] {
            assert!(!has_trailing_iso_date(s), "expected not dated: {s}");
        }
    }

    #[test]
    fn reserved_slug_blocked_case_insensitive() {
        for s in [
            "soul",
            "SOUL",
            "lore",
            "system",
            "admin",
            "assistant",
            "user",
            "tool",
            "dreamer",
            "prompt",
            "instructions",
        ] {
            assert!(
                matches!(parse_slug(s), Err(SlugError::Reserved)),
                "expected reserved for {s}"
            );
        }
    }

    #[test]
    fn body_sanitizer_blocks_frontmatter_reopen_and_path_headers_and_fences() {
        for bad in [
            "---\nfoo: bar\n---\n",
            "ok\n---\nfoo: bar\n---\nmore",
            "intro\n# SOUL\n",
            "intro\n# users/12.md\n",
            "intro\n# state/quiz.md\n",
            "leak <<<FILE path=x nonce=y>>> stuff",
            "leak <<<ENDFILE nonce=y>>> stuff",
            // CRLF variants must not slip past the LF-only checks above.
            "ok\r\n---\r\nfoo: bar\r\n---\r\nmore",
            "lone \r in body",
        ] {
            assert!(check_body(bad).is_err(), "should reject {bad:?}");
        }
        assert!(check_body("plain text\nlines\n# heading\n").is_ok());
    }

    #[test]
    fn display_name_strips_control_chars_zwsp_bidi_and_caps_64() {
        let raw = "alice\u{200B}\u{202E}\nbob";
        let s = normalize_display_name(raw);
        assert_eq!(s, "alicebob");
        let long = "x".repeat(200);
        assert_eq!(normalize_display_name(&long).chars().count(), 64);
    }
}
