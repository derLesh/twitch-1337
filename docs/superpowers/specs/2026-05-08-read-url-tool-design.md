# `read_url` — Fetch + Interpret + Answer

**Status:** Spec
**Date:** 2026-05-08
**Author:** Nikolai Zimmermann (with Claude)

## Problem

The current AI tool surface exposes `fetch_url`, which downloads a URL and returns extracted readable text. It cannot:

- Read images (PNG/JPEG/WebP) — bytes go through `String::from_utf8_lossy` and become unusable.
- Read PDFs — same problem; no extraction.
- Read audio or video.

Users routinely link such media in chat. The configured chat model (`deepseek/deepseek-v4-flash` via OpenRouter) is text-only, so even if multimodal payloads were threaded into the request, the main model could not consume them.

## Goal

Replace `fetch_url` with a single `read_url(url, instruction?)` tool that handles every supported content type. Interpretation is delegated to a dedicated multimodal sub-agent model configured separately in `[ai.media]`. The main chat model stays text-only and consumes only the sub-agent's text answer.

## Non-goals

- Provider Files API uploads (stateful, provider-specific).
- Local PDF text extraction or OCR (the multimodal model handles it).
- Per-user media quotas (the existing `[cooldowns].ai` cooldown is sufficient).
- Streaming sub-agent answers back through the tool result.
- Backwards-compatible `fetch_url` alias. The main model discovers tools from the list each turn; no migration needed.

## Tool surface

| Field | Value |
|-------|-------|
| Name  | `read_url` |
| Args  | `{ url: string, instruction?: string }` |
| Result (success) | JSON: `{ url, content_type, cached: bool, answer: string }` |
| Result (error)   | JSON: `{ error, details }` |

`web_search` is unchanged.

`fetch_url` is removed entirely. `WEB_TOOL_NAMES` becomes `["web_search", "read_url"]`.

### Routing

Every `read_url` call (text and media) routes through the sub-agent. Rationale: a single, consistent code path is easier to reason about and tests don't need a "fast-path vs sub-agent-path" split. The instruction-aware sub-agent also produces tighter, query-targeted answers from long text pages, which the main model would otherwise have to summarize itself.

## Architecture

```
main model
   │  read_url(url, instruction?)
   ▼
ContentToolExecutor
   │
   ├─ SSRF guards (existing): host literal + DNS resolution
   ├─ Stream download with per-type byte cap (Content-Length pre-check + running total)
   ├─ Content-type detection: header → magic-byte fallback (`infer` crate)
   ├─ Build sub-agent request → MediaClient
   │     ├─ image / pdf / audio / video → base64 data URL content part
   │     └─ text / html / json → inline text part (`extract_readable_text` for HTML)
   │  POST to [ai.media] (OpenAI-compatible chat/completions, multimodal user message)
   │
   ├─ TtlCache: key = (norm_url, norm_instruction); value = answer text
   └─ ToolResultMessage → main model
```

The `llm` crate stays text-only. `MediaClient` emits raw OpenAI-compatible JSON with content parts directly. Justification: the multimodal request schema is provider-coupled and would force every `llm` provider impl to grow content-part variants for a feature only one consumer needs. Isolating it inside the media client keeps the shared trait surface narrow.

## Files

- `crates/twitch-1337/src/ai/web_search/` → renamed to `crates/twitch-1337/src/ai/content/`
  - `client.rs` — HTTP fetch, SSRF guards, content-type detection, per-type cap (extends current `SearchClient` body-handling logic).
  - `media.rs` — **new**. Multimodal sub-agent client. Owns the `[ai.media]` config, builds the OpenAI-compatible request with content parts, posts, returns the answer string.
  - `executor.rs` — renamed dispatch: `read_url` instead of `fetch_url`, accepts optional `instruction`, always routes through the media client.
  - `tools.rs` — `read_url` `ToolDefinition`; `WEB_TOOL_NAMES` updated.
  - `cache.rs` — unchanged shape; key now includes the instruction.
- `crates/twitch-1337/src/ai/mod.rs` — wiring update.
- `crates/twitch-1337/src/config.rs` — new `AiMediaConfig` (see below).
- `data/config.toml.example` — document `[ai.media]` and recommended models.

New dependencies (`crates/twitch-1337/Cargo.toml`):

- `infer` — magic-byte content-type sniffing (no_std-friendly, MIT).
- `bytesize` with `features = ["serde"]` — parses human-readable size strings ("10 MB", "1 GiB") into a typed `ByteSize` for the per-type caps.

## Configuration

New section `[ai.media]`. All fields are optional. The `[ai]` provider
(`base_url`, `api_key`) is reused; only the `model` and per-type caps
differ.

```toml
[ai.media]
# Optional. Defaults to "~google/gemini-flash-latest".
model = "~google/gemini-flash-latest"

# Sub-agent request timeout (seconds). Default: 60.
timeout = 60

# Per-type size caps. Parsed via the `bytesize` crate (accepts "10 MB",
# "25 MiB", "1 GB", etc.). Defaults shown.
max_image_size = "10 MB"
max_pdf_size   = "25 MB"
max_audio_size = "25 MB"
max_video_size = "50 MB"
max_text_size  = "1 MB"
```

`[ai.media]` is always available when `[ai]` is configured: it inherits the provider, and every field has a sensible default. There is no separate "disabled" state.

## Content-type detection

Two-layer detection:

1. `Content-Type` header → primary signal.
2. Magic bytes on the first ~16 bytes via the `infer` crate → confirm or override (servers lie, especially for binary blobs served as `application/octet-stream`).

If neither layer maps to a known supported type, return `{"error": "unsupported_content_type"}`. Supported buckets:

| Bucket | MIME prefixes |
|--------|---------------|
| image  | `image/png`, `image/jpeg`, `image/webp`, `image/gif` |
| pdf    | `application/pdf` |
| audio  | `audio/*` (mp3, wav, ogg, flac, m4a) |
| video  | `video/*` (mp4, webm) |
| text   | `text/html`, `text/plain`, `application/json`, `application/xml`, `text/xml` |

## Sub-agent request shape

OpenAI-compatible chat completion. One system message, one user message with content parts.

```json
{
  "model": "<from [ai.media].model>",
  "messages": [
    {
      "role": "system",
      "content": "You analyze URLs on behalf of a Twitch chat bot. Answer the user's instruction strictly from the provided content. Be concise. If the instruction is empty, describe the contents."
    },
    {
      "role": "user",
      "content": [
        { "type": "text", "text": "<instruction or 'Describe the contents.'>" },
        { "type": "image_url", "image_url": { "url": "data:image/png;base64,..." } }
      ]
    }
  ]
}
```

For text-bucket payloads the second content part is `{ "type": "text", "text": "<extracted text>" }` instead of a media data URL.

For PDFs, content part type is provider-dependent. Initial implementation uses `image_url` with the PDF data URL (`data:application/pdf;base64,...`); Gemini and Claude over OpenRouter accept this. If the configured model rejects it, the bot returns `analysis_failed` with the provider error; operators can switch to a model that supports it.

## Errors (JSON returned to the main model)

| Code | Cause |
|------|-------|
| `fetch_blocked` | SSRF guard tripped (existing) |
| `fetch_timeout` | HTTP timeout (existing) |
| `fetch_failed` | Other HTTP error (existing) |
| `payload_too_large` | Per-type cap exceeded |
| `unsupported_content_type` | Header + magic bytes did not match a known bucket |
| `analysis_failed` | Sub-agent provider returned an error |
| `analysis_timeout` | Sub-agent request timed out |

## Caching

Reuse `TtlCache<String>` on the **answer text**. Key:

```rust
format!("{}::{}", normalize_url(url), instruction.unwrap_or("").trim().to_lowercase())
```

TTL stays at the current value (5 min). Cache hits return `cached: true` without a sub-agent call.

## Tests

Unit:
- Content-type detection: header-only, magic-only, conflict (header lies, magic correct), unknown.
- Per-type cap enforcement: pre-check via `Content-Length`, streaming cut-off when no header.
- Sub-agent request shape: image → data URL part, text → text part, system message present, instruction passthrough.
- Cache key normalization: instruction whitespace and case fold; URL normalization unchanged.
- Error mapping for each failure mode.

Integration (`crates/twitch-1337/tests/ai.rs`):
- Mock media endpoint + mock SearXNG; full turn round-trip with a `read_url` call returns the sub-agent's answer to the main model.
- `[ai.media]` absent → tool falls back to default model + caps and still serves requests.

## Open questions

- **Audio/video coverage.** Not every OpenRouter route accepts inline audio. The default `~google/gemini-flash-latest` covers all four media types as of 2026-05. If the operator overrides to a model that rejects audio, the bot surfaces `analysis_failed` with the provider error rather than silently degrading.
