# Dönerpreisindex Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `!dpi` chat command and `doener_index` AI tool, both backed by a shared `DoenerClient` against https://xn--dnerindex-07a.com (`stats.php` + `cities.php`). Fixes issue #178.

**Architecture:** New `crates/core/src/doener/` module holds the HTTP client, deserializable types, and chat formatters. A thin `DoenerCommand` consumes the client via the existing `Command` trait. A new `crates/core/src/ai/doener_tool.rs` module exposes the same client to `!ai` through `V2Executor`, routed unconditionally so the tool ships even when `[ai.web]` is disabled.

**Tech Stack:** Rust async (tokio), reqwest, serde, eyre, wiremock for tests, async-trait. All deps already in tree.

**Spec:** `docs/superpowers/specs/2026-05-11-donerpreisindex-design.md` — read it before starting if you have not.

---

## File structure

New:
- `crates/core/src/doener/mod.rs`
- `crates/core/src/doener/client.rs`
- `crates/core/src/doener/types.rs`
- `crates/core/src/doener/format.rs`
- `crates/core/src/commands/doener.rs`
- `crates/core/src/ai/doener_tool.rs`

Modified:
- `crates/core/src/lib.rs`
- `crates/core/src/ai/mod.rs`
- `crates/core/src/commands/mod.rs`
- `crates/core/src/config.rs`
- `crates/core/src/twitch/handlers/commands.rs`
- `crates/core/src/twitch/handlers/spawn.rs`
- `crates/core/src/ai/command.rs`
- `crates/twitch-1337/src/main.rs`
- `crates/twitch-1337/config.toml.example`

No new Cargo dependencies. Use `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail` per project memory.

---

## Task 1: Module skeleton + `pub mod doener;`

Stand up the module hierarchy with empty files so later tasks compile incrementally.

**Files:**
- Create: `crates/core/src/doener/mod.rs`
- Create: `crates/core/src/doener/types.rs`
- Create: `crates/core/src/doener/format.rs`
- Create: `crates/core/src/doener/client.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Create `crates/core/src/doener/mod.rs`**

```rust
pub mod client;
pub mod format;
pub mod types;

pub use client::DoenerClient;
pub use types::{CityHit, GlobalStats};
```

- [ ] **Step 2: Create the three sub-files as empty placeholders**

`crates/core/src/doener/types.rs`:

```rust
// Filled in Task 2.
```

`crates/core/src/doener/format.rs`:

```rust
// Filled in Task 3.
```

`crates/core/src/doener/client.rs`:

```rust
// Filled in Task 4.
```

- [ ] **Step 3: Register the module in `crates/core/src/lib.rs`**

Find the existing `pub mod` block (currently `pub mod ai; pub mod aviation; pub mod commands; ...`) and insert `pub mod doener;` alphabetically between `database` and `llm_factory`:

```rust
pub mod database;
pub mod doener;
pub mod llm_factory;
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p core --quiet`
Expected: success (warnings about empty modules are fine).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/doener crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(doener): scaffold doener module

Empty types/format/client placeholders; lib.rs export.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Types + price deserializer

The upstream API returns prices as either JSON strings (`"6.00"`) or `null`. Encode the conversion in one place.

**Files:**
- Modify: `crates/core/src/doener/types.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/doener/types.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GlobalStats {
    pub total_locations: u32,
    pub total_cities: u32,
    pub min_price: f64,
    pub max_price: f64,
    pub avg_price: f64,
    pub locations_no_price_pct: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CityHit {
    pub city: String,
    pub location_count: u32,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub min_price: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub max_price: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_price")]
    pub avg_price: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CitiesResponse {
    pub cities: Vec<CityHit>,
}

fn deserialize_optional_price<'de, D>(de: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(f64),
        Text(String),
        Null,
    }
    Ok(match Option::<Raw>::deserialize(de)? {
        None | Some(Raw::Null) => None,
        Some(Raw::Number(n)) => Some(n),
        Some(Raw::Text(s)) => s.trim().parse::<f64>().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_stats_parses_canonical_payload() {
        let raw = r#"{"total_locations":6092,"total_cities":2202,"min_price":5.5,"max_price":9,"avg_price":6.1,"locations_no_price":5304,"locations_no_price_pct":87.1}"#;
        let s: GlobalStats = serde_json::from_str(raw).unwrap();
        assert_eq!(s.total_locations, 6092);
        assert_eq!(s.total_cities, 2202);
        assert!((s.avg_price - 6.1).abs() < 1e-9);
        assert!((s.locations_no_price_pct - 87.1).abs() < 1e-9);
    }

    #[test]
    fn city_hit_parses_string_prices() {
        let raw = r#"{"city":"Hannover","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert_eq!(c.city, "Hannover");
        assert_eq!(c.location_count, 51);
        assert_eq!(c.avg_price, Some(6.0));
    }

    #[test]
    fn city_hit_treats_null_prices_as_none() {
        let raw = r#"{"city":"Handewitt","location_count":1,"min_price":null,"max_price":null,"avg_price":null}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert!(c.min_price.is_none());
        assert!(c.max_price.is_none());
        assert!(c.avg_price.is_none());
    }

    #[test]
    fn city_hit_accepts_numeric_prices() {
        let raw = r#"{"city":"Berlin","location_count":324,"min_price":6.0,"max_price":7.5,"avg_price":6.2}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert_eq!(c.min_price, Some(6.0));
        assert_eq!(c.max_price, Some(7.5));
    }

    #[test]
    fn city_hit_unparseable_price_string_becomes_none() {
        let raw = r#"{"city":"X","location_count":0,"min_price":"abc","max_price":null,"avg_price":null}"#;
        let c: CityHit = serde_json::from_str(raw).unwrap();
        assert!(c.min_price.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail doener::types`
Expected: 5 tests passed.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/doener/types.rs
git commit -m "$(cat <<'EOF'
feat(doener): GlobalStats + CityHit with optional-price deserializer

Upstream encodes prices as either JSON strings or null. One custom
deserializer normalises both forms (plus tolerates numeric and malformed
strings) so callers get Option<f64>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Chat formatters

Pure functions that turn data into German chat strings. Tests freeze the exact output for each branch so future edits cannot silently change what users see.

**Files:**
- Modify: `crates/core/src/doener/format.rs`

- [ ] **Step 1: Write the failing tests + implementation**

Replace the placeholder in `crates/core/src/doener/format.rs`:

```rust
use crate::doener::types::{CityHit, GlobalStats};

const API_DOWN_MESSAGE: &str = "FeelsDankMan döner-index API down";

pub fn format_global(s: &GlobalStats) -> String {
    format!(
        "Döner-Index DE: {locations} Buden in {cities} Städten, ⌀ {avg:.2}€ ({min:.2}–{max:.2}€). {no_price}% ohne Preis.",
        locations = s.total_locations,
        cities = s.total_cities,
        avg = s.avg_price,
        min = s.min_price,
        max = s.max_price,
        no_price = format_pct(s.locations_no_price_pct),
    )
}

pub fn format_city(c: &CityHit) -> String {
    let bude = if c.location_count == 1 { "Bude" } else { "Buden" };
    match (c.avg_price, c.min_price, c.max_price) {
        (Some(avg), Some(min), Some(max)) => format!(
            "{name}: {count} {bude}, ⌀ {avg:.2}€ ({min:.2}–{max:.2}€).",
            name = c.city,
            count = c.location_count,
        ),
        _ => format!(
            "{name}: {count} {bude}, noch keine Preise.",
            name = c.city,
            count = c.location_count,
        ),
    }
}

pub fn format_did_you_mean(hits: &[CityHit]) -> String {
    let parts: Vec<String> = hits
        .iter()
        .take(3)
        .map(|h| format!("{} ({})", h.city, h.location_count))
        .collect();
    format!("Meintest du: {}?", parts.join(", "))
}

pub fn format_not_found(query: &str) -> String {
    format!("FeelsDankMan keine Stadt für '{query}' gefunden.")
}

pub fn api_down_message() -> &'static str {
    API_DOWN_MESSAGE
}

fn format_pct(p: f64) -> String {
    if (p.round() - p).abs() < f64::EPSILON {
        format!("{}", p.round() as i64)
    } else {
        format!("{p:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> GlobalStats {
        GlobalStats {
            total_locations: 6092,
            total_cities: 2202,
            min_price: 5.5,
            max_price: 9.0,
            avg_price: 6.1,
            locations_no_price_pct: 87.0,
        }
    }

    fn hit(city: &str, n: u32, prices: Option<(f64, f64, f64)>) -> CityHit {
        CityHit {
            city: city.into(),
            location_count: n,
            min_price: prices.map(|(min, _, _)| min),
            max_price: prices.map(|(_, _, max)| max),
            avg_price: prices.map(|(_, avg, _)| avg),
        }
    }

    #[test]
    fn global_matches_golden_string() {
        assert_eq!(
            format_global(&stats()),
            "Döner-Index DE: 6092 Buden in 2202 Städten, ⌀ 6.10€ (5.50–9.00€). 87% ohne Preis."
        );
    }

    #[test]
    fn global_keeps_decimal_in_no_price_pct_when_non_integer() {
        let mut s = stats();
        s.locations_no_price_pct = 87.1;
        assert!(format_global(&s).contains("87.1% ohne Preis"));
    }

    #[test]
    fn city_with_prices_uses_plural_buden() {
        let c = hit("Hannover", 51, Some((6.0, 6.0, 6.0)));
        assert_eq!(
            format_city(&c),
            "Hannover: 51 Buden, ⌀ 6.00€ (6.00–6.00€)."
        );
    }

    #[test]
    fn city_with_one_location_uses_singular_bude() {
        let c = hit("Handewitt", 1, None);
        assert_eq!(format_city(&c), "Handewitt: 1 Bude, noch keine Preise.");
    }

    #[test]
    fn city_with_one_location_with_price_uses_singular_bude() {
        let c = hit("X", 1, Some((7.0, 7.0, 7.0)));
        assert_eq!(format_city(&c), "X: 1 Bude, ⌀ 7.00€ (7.00–7.00€).");
    }

    #[test]
    fn did_you_mean_lists_top_three() {
        let hits = vec![
            hit("Hannover", 51, None),
            hit("Hanau", 3, None),
            hit("Handewitt", 1, None),
            hit("Hannover-Land", 0, None),
        ];
        assert_eq!(
            format_did_you_mean(&hits),
            "Meintest du: Hannover (51), Hanau (3), Handewitt (1)?"
        );
    }

    #[test]
    fn not_found_quotes_query() {
        assert_eq!(format_not_found("xyz"), "FeelsDankMan keine Stadt für 'xyz' gefunden.");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail doener::format`
Expected: 7 tests passed.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/doener/format.rs
git commit -m "$(cat <<'EOF'
feat(doener): chat formatters with golden-string tests

Pure German formatters for global/city/did-you-mean/not-found responses.
Singular Bude vs plural Buden, percent without trailing .0 — frozen in
tests so cosmetic regressions are caught at compile time.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `DoenerClient` skeleton

Set up the struct, constants, and constructors before adding methods. Keep tests compiling.

**Files:**
- Modify: `crates/core/src/doener/client.rs`

- [ ] **Step 1: Replace the placeholder with the skeleton**

```rust
use std::time::Duration;

use eyre::{Result, WrapErr};

use crate::APP_USER_AGENT;
use crate::doener::types::{CitiesResponse, CityHit, GlobalStats};

const BASE_URL: &str = "https://xn--dnerindex-07a.com";
const TIMEOUT: Duration = Duration::from_secs(5);

pub struct DoenerClient {
    http: reqwest::Client,
    base_url: String,
}

impl DoenerClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .wrap_err("build doener HTTP client")?;
        Ok(Self {
            http,
            base_url: BASE_URL.to_string(),
        })
    }

    /// Test hook: inject an existing `reqwest::Client` (commonly with a short
    /// timeout) and a custom base URL pointing at a wiremock server.
    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }
}
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p core --quiet`
Expected: success. Unused-import warnings for `CityHit`, `CitiesResponse`, `GlobalStats` are fine — methods land in Tasks 5–6.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/doener/client.rs
git commit -m "$(cat <<'EOF'
feat(doener): DoenerClient skeleton

Constants + constructors only. Methods land in follow-up commits with
their wiremock tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `DoenerClient::stats`

**Files:**
- Modify: `crates/core/src/doener/client.rs`

- [ ] **Step 1: Add the failing test**

Append to `crates/core/src/doener/client.rs` (inside a new `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn test_client(server: &MockServer) -> DoenerClient {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .expect("build test client");
        DoenerClient::with_base_url(http, server.uri())
    }

    #[tokio::test]
    async fn stats_parses_canonical_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"total_locations":6092,"total_cities":2202,"min_price":5.5,"max_price":9,"avg_price":6.1,"locations_no_price":5304,"locations_no_price_pct":87.1}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let stats = client.stats().await.expect("stats ok");
        assert_eq!(stats.total_locations, 6092);
        assert_eq!(stats.total_cities, 2202);
    }

    #[tokio::test]
    async fn stats_returns_err_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = test_client(&server);
        assert!(client.stats().await.is_err());
    }

    #[tokio::test]
    async fn stats_returns_err_on_malformed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = test_client(&server);
        assert!(client.stats().await.is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo check -p core --tests --quiet`
Expected: error `no method named 'stats' found for struct 'DoenerClient'`.

- [ ] **Step 3: Implement `stats`**

Add to the `impl DoenerClient` block in the same file:

```rust
    pub async fn stats(&self) -> Result<GlobalStats> {
        let url = format!("{}/api/stats.php", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("doener stats: request failed")?
            .error_for_status()
            .wrap_err("doener stats: non-2xx")?;
        resp.json::<GlobalStats>()
            .await
            .wrap_err("doener stats: parse JSON")
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail doener::client::tests::stats`
Expected: 3 tests passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/doener/client.rs
git commit -m "$(cat <<'EOF'
feat(doener): client.stats() against /api/stats.php

Reqwest GET with project-wide UA and 5s timeout; wiremock-tested for
200, 500 and malformed JSON paths.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `DoenerClient::search_cities`

**Files:**
- Modify: `crates/core/src/doener/client.rs`

- [ ] **Step 1: Add the failing test**

Append to the existing `mod tests` block:

```rust
    #[tokio::test]
    async fn search_cities_returns_hits_in_upstream_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .and(wiremock::matchers::query_param("q", "Han"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"Han","count":3,"cities":[
                    {"city":"Hannover","zip":"30459","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"},
                    {"city":"Hanau","zip":"63456","location_count":3,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"},
                    {"city":"Handewitt","zip":"24983","location_count":1,"min_price":null,"max_price":null,"avg_price":null}
                ]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let hits = client.search_cities("Han").await.expect("cities ok");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].city, "Hannover");
        assert_eq!(hits[2].avg_price, None);
    }

    #[tokio::test]
    async fn search_cities_empty_array_is_ok_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"zzz","count":0,"cities":[]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let hits = client.search_cities("zzz").await.expect("ok");
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_cities_returns_err_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = test_client(&server);
        assert!(client.search_cities("x").await.is_err());
    }
```

- [ ] **Step 2: Implement `search_cities`**

Add to `impl DoenerClient`:

```rust
    pub async fn search_cities(&self, q: &str) -> Result<Vec<CityHit>> {
        let url = format!("{}/api/cities.php", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", q)])
            .send()
            .await
            .wrap_err("doener cities: request failed")?
            .error_for_status()
            .wrap_err("doener cities: non-2xx")?;
        let body = resp
            .json::<CitiesResponse>()
            .await
            .wrap_err("doener cities: parse JSON")?;
        Ok(body.cities)
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail doener::client`
Expected: 6 tests passed (3 from Task 5 + 3 new).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/doener/client.rs
git commit -m "$(cat <<'EOF'
feat(doener): client.search_cities() against /api/cities.php

Returns hits in upstream order. Wiremock-tested for non-empty, empty and
5xx paths.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `cooldowns.doener` config field

**Files:**
- Modify: `crates/core/src/config.rs`
- Modify: `crates/twitch-1337/config.toml.example`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `crates/core/src/config.rs` (any spot near the other `CooldownsConfig`-touching tests, or append at end of the module):

```rust
    #[test]
    fn cooldowns_doener_defaults_to_30() {
        let c: CooldownsConfig = toml::from_str("").expect("empty cooldowns parses");
        assert_eq!(c.doener, 30);
    }

    #[test]
    fn cooldowns_doener_overrides_via_toml() {
        let c: CooldownsConfig = toml::from_str("doener = 5").expect("parses");
        assert_eq!(c.doener, 5);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo check -p core --tests --quiet`
Expected: error `no field 'doener' on type 'CooldownsConfig'`.

- [ ] **Step 3: Add the field + default**

In `crates/core/src/config.rs`, near the existing default fns (~line 446):

```rust
fn default_doener_cooldown() -> u64 {
    30
}
```

Extend `CooldownsConfig` (currently lines 450–460):

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct CooldownsConfig {
    #[serde(default = "default_ai_cooldown")]
    pub ai: u64,
    #[serde(default = "default_news_cooldown")]
    pub news: u64,
    #[serde(default = "default_up_cooldown")]
    pub up: u64,
    #[serde(default = "default_feedback_cooldown")]
    pub feedback: u64,
    #[serde(default = "default_doener_cooldown")]
    pub doener: u64,
}
```

Extend the `Default` impl (currently lines 462–471):

```rust
impl Default for CooldownsConfig {
    fn default() -> Self {
        Self {
            ai: default_ai_cooldown(),
            news: default_news_cooldown(),
            up: default_up_cooldown(),
            feedback: default_feedback_cooldown(),
            doener: default_doener_cooldown(),
        }
    }
}
```

- [ ] **Step 4: Document in the example config**

In `crates/twitch-1337/config.toml.example`, find the `[cooldowns]` block and add a `doener` line:

```toml
# Optional: Command cooldowns in seconds
# [cooldowns]
# ai = 30        # !ai command (default: 30)
# news = 60      # !news command (default: 60)
# up = 30        # !up command (default: 30)
# feedback = 300 # !fb command (default: 300)
# doener = 30    # !dpi command (default: 30)
```

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail config::tests::cooldowns_doener`
Expected: 2 tests passed.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/config.rs crates/twitch-1337/config.toml.example
git commit -m "$(cat <<'EOF'
feat(config): cooldowns.doener for !dpi (default 30s)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `DoenerCommand` (chat) + `pub mod doener;` in commands

The chat command. Branching logic lives here; formatting and HTTP are already covered.

**Files:**
- Create: `crates/core/src/commands/doener.rs`
- Modify: `crates/core/src/commands/mod.rs`

- [ ] **Step 1: Add the module declaration**

In `crates/core/src/commands/mod.rs`, in the `pub mod` block (after `feedback`):

```rust
pub mod doener;
pub mod feedback;
```

(Maintain alphabetical order; `doener` precedes `feedback`.)

- [ ] **Step 2: Create the command with its branching tests**

`crates/core/src/commands/doener.rs`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::time::Duration;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::doener::format::{
    api_down_message, format_city, format_did_you_mean, format_global, format_not_found,
};
use crate::doener::{CityHit, DoenerClient};

pub struct DoenerCommand {
    client: Arc<DoenerClient>,
    cooldown: PerUserCooldown,
}

impl DoenerCommand {
    pub fn new(client: Arc<DoenerClient>, cooldown: Duration) -> Self {
        Self {
            client,
            cooldown: PerUserCooldown::new(cooldown),
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for DoenerCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!dpi"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        if let Some(remaining) = self.cooldown.check(user).await {
            send(
                &ctx,
                format!(
                    "Bitte warte noch {} Waiting",
                    format_cooldown_remaining(remaining)
                ),
            )
            .await;
            return Ok(());
        }
        self.cooldown.record(user).await;

        let query = ctx.args.join(" ");
        let query = query.trim();

        let response = if query.is_empty() {
            match self.client.stats().await {
                Ok(s) => format_global(&s),
                Err(e) => {
                    error!(error = ?e, "doener stats lookup failed");
                    api_down_message().to_string()
                }
            }
        } else {
            match self.client.search_cities(query).await {
                Ok(hits) => decide_city_response(query, hits),
                Err(e) => {
                    error!(error = ?e, query, "doener city lookup failed");
                    api_down_message().to_string()
                }
            }
        };

        send(&ctx, response).await;
        Ok(())
    }
}

fn decide_city_response(query: &str, hits: Vec<CityHit>) -> String {
    if hits.is_empty() {
        return format_not_found(query);
    }
    if hits.len() == 1 {
        return format_city(&hits[0]);
    }
    if hits[0].city.eq_ignore_ascii_case(query) {
        return format_city(&hits[0]);
    }
    format_did_you_mean(&hits)
}

async fn send<T, L>(ctx: &CommandContext<'_, T, L>, line: String)
where
    T: Transport,
    L: LoginCredentials,
{
    if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, line).await {
        error!(error = ?e, "Failed to send !dpi response");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doener::CityHit;

    fn hit(city: &str, n: u32, with_prices: bool) -> CityHit {
        CityHit {
            city: city.into(),
            location_count: n,
            min_price: with_prices.then_some(6.0),
            max_price: with_prices.then_some(6.0),
            avg_price: with_prices.then_some(6.0),
        }
    }

    #[test]
    fn city_response_empty_hits_is_not_found() {
        let out = decide_city_response("xyz", vec![]);
        assert_eq!(out, "FeelsDankMan keine Stadt für 'xyz' gefunden.");
    }

    #[test]
    fn city_response_single_hit_uses_format_city() {
        let out = decide_city_response("Hannover", vec![hit("Hannover", 51, true)]);
        assert!(out.starts_with("Hannover: 51 Buden"));
    }

    #[test]
    fn city_response_exact_match_short_circuits() {
        // "Berlin" → upstream returns Berlin + Berlingerode. Treat as single hit.
        let hits = vec![hit("Berlin", 324, true), hit("Berlingerode", 1, false)];
        let out = decide_city_response("berlin", hits);
        assert!(out.starts_with("Berlin: 324 Buden"));
    }

    #[test]
    fn city_response_multi_hit_no_exact_match_is_did_you_mean() {
        let hits = vec![
            hit("Hannover", 51, true),
            hit("Hanau", 3, true),
            hit("Handewitt", 1, false),
        ];
        let out = decide_city_response("Han", hits);
        assert_eq!(
            out,
            "Meintest du: Hannover (51), Hanau (3), Handewitt (1)?"
        );
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail commands::doener`
Expected: 4 tests passed.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/commands/doener.rs crates/core/src/commands/mod.rs
git commit -m "$(cat <<'EOF'
feat(doener): DoenerCommand (!dpi) with all branches covered

Empty arg → global stats. Arg → cities.php fuzzy; 0/1/exact-match/many
each map to a distinct chat string. Exact case-insensitive match on the
first hit short-circuits did-you-mean so '!dpi Berlin' doesn't suggest
'Berlin (324), Berlingerode (1)'.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Plumb `DoenerClient` through `Services` + `main.rs`

**Files:**
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/twitch-1337/src/main.rs`

- [ ] **Step 1: Add the field to `Services`**

In `crates/core/src/lib.rs`, in `pub struct Services` (currently ~line 73), add after `aviation`:

```rust
    pub aviation: Option<AviationClient>,
    pub doener: Arc<crate::doener::DoenerClient>,
```

- [ ] **Step 2: Destructure it in `run_bot`**

In `crates/core/src/lib.rs`, in `run_bot` (currently ~line 128, `let Services { ... }`), add `doener,` to the destructure pattern next to `aviation,`.

- [ ] **Step 3: Import the module in `main.rs`**

In `crates/twitch-1337/src/main.rs`, in the existing `use twitch_1337_core::{ ... }` block (currently ~lines 11–20), add `doener` next to `aviation`:

```rust
use twitch_1337_core::{
    AuthenticatedLoginCredentials, Services,
    ai::{command::memory_caps_from_config, memory::store::MemoryStore},
    aviation, doener, ensure_data_dir, get_data_dir, install_crypto_provider, install_tracing, llm_factory,
    load_configuration,
    ping::PingManager,
    run_bot, setup_and_verify_twitch_client,
    twitch::whisper,
    util::clock::SystemClock,
};
```

- [ ] **Step 4: Construct the client and place it in `Services`**

In `crates/twitch-1337/src/main.rs`, after the aviation block (currently ~line 73), add:

```rust
    let doener_client = Arc::new(
        doener::DoenerClient::new().wrap_err("Failed to initialize Döner-index client")?,
    );
```

Then add the field to the `Services { ... }` literal (currently ~line 115):

```rust
    let services = Services {
        clock: Arc::new(SystemClock),
        llm: llm_client,
        aviation: aviation_client,
        doener: doener_client,
        whisper: Some(whisper),
        ...
```

- [ ] **Step 5: Verify compile**

Run: `cargo check --workspace --quiet`
Expected: success. Plumbing through the rest of the chain (SpawnDeps, CommandHandlerConfig) happens in Task 10. `Services.doener` is constructed but not yet read — that is fine (no field-unused warning because `Services` is `pub`).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/lib.rs crates/twitch-1337/src/main.rs
git commit -m "$(cat <<'EOF'
feat(doener): plumb DoenerClient into Services

Constructed once in main.rs (Arc-shared). Consumers wired in
follow-up commits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Thread doener through `SpawnDeps` → `CommandHandlerConfig`

**Files:**
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/core/src/twitch/handlers/spawn.rs`
- Modify: `crates/core/src/twitch/handlers/commands.rs`

- [ ] **Step 1: Add the field to `SpawnDeps`**

In `crates/core/src/twitch/handlers/spawn.rs`, in `pub(crate) struct SpawnDeps`, add next to the shared section (after `data_dir: PathBuf,` ~line 71):

```rust
    pub data_dir: PathBuf,
    pub doener: Arc<crate::doener::DoenerClient>,
```

Destructure it in `spawn_handlers` (~line 103) by adding `doener,` to the pattern.

Add it to the `CommandHandlerConfig { ... }` literal built inside the `generic_commands` spawn (~line 228) — see Step 3 below for the corresponding `CommandHandlerConfig` field.

- [ ] **Step 2: Pass it from `run_bot` into `SpawnDeps`**

In `crates/core/src/lib.rs`, in `run_bot` after destructuring `services`, locate the `spawn_handlers(SpawnDeps { ... })` call (or wherever `SpawnDeps` is constructed) and add `doener: doener.clone(),` (or `doener,` if not used afterwards).

- [ ] **Step 3: Add the field to `CommandHandlerConfig`**

In `crates/core/src/twitch/handlers/commands.rs`, in `pub struct CommandHandlerConfig<T,L>` (currently ~line 25), add next to `data_dir`:

```rust
    pub data_dir: std::path::PathBuf,
    pub doener: Arc<crate::doener::DoenerClient>,
```

Add `doener,` to the destructure pattern in `run_generic_command_handler` (~line 63).

In the `spawn.rs` `generic_commands` block where `CommandHandlerConfig { ... }` is built, add `doener: doener.clone(),` (or `doener,` if last use).

- [ ] **Step 4: Verify compile**

Run: `cargo check --workspace --quiet`
Expected: success. `doener` may produce a `field is never read` warning until Task 11 — that is acceptable for this transient state, but prefer to combine Step 5 of Task 11 into this commit if convenient. The cleanest split: commit Task 10 with the new field marked `#[allow(dead_code)]` if needed, then drop the allow in Task 11. The simpler choice is to register the command immediately in Task 11 and treat them as a single deploy unit.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/lib.rs crates/core/src/twitch/handlers/spawn.rs crates/core/src/twitch/handlers/commands.rs
git commit -m "$(cat <<'EOF'
feat(doener): thread DoenerClient through SpawnDeps + CommandHandlerConfig

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Register `DoenerCommand` in the dispatcher

**Files:**
- Modify: `crates/core/src/twitch/handlers/commands.rs`

- [ ] **Step 1: Push the command into `cmd_list`**

In `crates/core/src/twitch/handlers/commands.rs`, in the `cmd_list` `vec![...]` (currently ~line 134), add an entry alongside the other simple commands (after `FeedbackCommand` is fine):

```rust
        Box::new(commands::feedback::FeedbackCommand::new(
            data_dir.clone(),
            Duration::from_secs(cooldowns.feedback),
        )),
        Box::new(commands::doener::DoenerCommand::new(
            doener.clone(),
            Duration::from_secs(cooldowns.doener),
        )),
    ];
```

- [ ] **Step 2: Verify compile**

Run: `cargo check --workspace --quiet`
Expected: success, no `field is never read` warnings on `doener`.

- [ ] **Step 3: Run the full test suite**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: full pass.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/twitch/handlers/commands.rs
git commit -m "$(cat <<'EOF'
feat(doener): register !dpi in the command dispatcher

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: AI tool definition + `is_web_tool` regression test

**Files:**
- Create: `crates/core/src/ai/doener_tool.rs`
- Modify: `crates/core/src/ai/mod.rs`

- [ ] **Step 1: Add the module declaration**

In `crates/core/src/ai/mod.rs`:

```rust
pub mod chat_history;
pub mod command;
pub mod content;
pub mod doener_tool;
pub mod memory;
pub mod prefill;
pub mod session;
```

- [ ] **Step 2: Create the module with the tool def only (no execute fn yet)**

`crates/core/src/ai/doener_tool.rs`:

```rust
use llm::ToolDefinition;
use serde::Deserialize;

pub const DOENER_TOOL_NAME: &str = "doener_index";

#[derive(Debug, Deserialize)]
pub(crate) struct DoenerArgs {
    #[serde(default)]
    pub city: Option<String>,
}

pub fn doener_tool() -> ToolDefinition {
    ToolDefinition {
        name: DOENER_TOOL_NAME.into(),
        description: "Look up the German Döner price index from dönerindex.com. \
            Without `city`, returns the country-wide aggregate (location count, \
            avg/min/max price). With `city` (free-form), returns the top matching \
            cities and their per-city aggregate. Use this for any question about \
            Döner prices, kebab prices, or how expensive Döner is in a German city."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "Optional city name or prefix. German spelling preferred (e.g. 'Köln', 'München')."
                }
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_def_has_expected_name() {
        let t = doener_tool();
        assert_eq!(t.name, "doener_index");
    }

    #[test]
    fn doener_index_is_not_a_web_tool() {
        // Regression guard: a future refactor must not gate this tool behind
        // [ai.web]. is_web_tool drives routing into ContentToolExecutor.
        assert!(!crate::ai::content::is_web_tool(DOENER_TOOL_NAME));
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail ai::doener_tool`
Expected: 2 tests passed.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/ai/mod.rs crates/core/src/ai/doener_tool.rs
git commit -m "$(cat <<'EOF'
feat(ai): doener_index tool definition

Standalone module so the tool is not gated by [ai.web]. Regression test
asserts is_web_tool(\"doener_index\") stays false.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: `execute_doener_index` + wiremock tests

**Files:**
- Modify: `crates/core/src/ai/doener_tool.rs`

- [ ] **Step 1: Add the failing tests**

Append to the existing `#[cfg(test)] mod tests` in `crates/core/src/ai/doener_tool.rs`:

```rust
    use std::time::Duration;

    use llm::{ToolCall, ToolResultMessage};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::doener::DoenerClient;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: DOENER_TOOL_NAME.into(),
            arguments: args,
            arguments_parse_error: None,
        }
    }

    fn test_client(server: &MockServer) -> DoenerClient {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        DoenerClient::with_base_url(http, server.uri())
    }

    fn content(msg: &ToolResultMessage) -> String {
        // ToolResultMessage.content is a public String field — see crates/llm/src/types.rs:74.
        msg.content.clone()
    }

    #[tokio::test]
    async fn no_city_returns_global_scope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"total_locations":6092,"total_cities":2202,"min_price":5.5,"max_price":9,"avg_price":6.1,"locations_no_price":5304,"locations_no_price_pct":87.1}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({}))).await;
        let body = content(&msg);
        assert!(body.contains("\"scope\":\"global\""), "got: {body}");
        assert!(body.contains("\"total_locations\":6092"), "got: {body}");
    }

    #[tokio::test]
    async fn with_city_returns_city_scope_and_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .and(wiremock::matchers::query_param("q", "Han"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"Han","count":1,"cities":[{"city":"Hannover","zip":"30459","location_count":51,"min_price":"6.00","max_price":"6.00","avg_price":"6.00"}]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(
            &client,
            &call(serde_json::json!({"city": "Han"})),
        )
        .await;
        let body = content(&msg);
        assert!(body.contains("\"scope\":\"city\""), "got: {body}");
        assert!(body.contains("Hannover"), "got: {body}");
    }

    #[tokio::test]
    async fn zero_hits_returns_empty_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cities.php"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"ok":true,"query":"zzz","count":0,"cities":[]}"#.as_slice(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(
            &client,
            &call(serde_json::json!({"city": "zzz"})),
        )
        .await;
        let body = content(&msg);
        assert!(body.contains("\"hits\":[]"), "got: {body}");
    }

    #[tokio::test]
    async fn upstream_failure_returns_error_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/stats.php"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let msg = execute_doener_index(&client, &call(serde_json::json!({}))).await;
        let body = content(&msg);
        assert!(body.contains("\"error\":\"doener_index API unavailable\""), "got: {body}");
    }

    #[tokio::test]
    async fn malformed_args_returns_invalid_arguments_json() {
        // city as integer should fail to deserialize as Option<String>.
        let server = MockServer::start().await;
        // No mock needed; the args parse error short-circuits before any HTTP.
        let client = test_client(&server);
        let msg = execute_doener_index(
            &client,
            &call(serde_json::json!({"city": 123})),
        )
        .await;
        let body = content(&msg);
        assert!(body.contains("\"error\":\"invalid_arguments\""), "got: {body}");
    }
```

- [ ] **Step 2: Implement `execute_doener_index`**

Append to `crates/core/src/ai/doener_tool.rs` (before the `#[cfg(test)]` block):

```rust
use llm::{ToolCall, ToolResultMessage};
use serde_json::json;

use crate::doener::DoenerClient;

pub async fn execute_doener_index(
    client: &DoenerClient,
    call: &ToolCall,
) -> ToolResultMessage {
    let args = match call.parse_args::<DoenerArgs>() {
        Ok(a) => a,
        Err(e) => {
            return ToolResultMessage::for_call(
                call,
                json!({
                    "error": "invalid_arguments",
                    "details": e.to_string(),
                })
                .to_string(),
            );
        }
    };

    let payload = match args.city.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => match client.stats().await {
            Ok(stats) => json!({"scope": "global", "stats": stats}),
            Err(e) => {
                tracing::warn!(error = ?e, "doener_index global lookup failed");
                json!({"error": "doener_index API unavailable"})
            }
        },
        Some(q) => match client.search_cities(q).await {
            Ok(hits) => {
                let top: Vec<_> = hits.into_iter().take(5).collect();
                json!({"scope": "city", "query": q, "hits": top})
            }
            Err(e) => {
                tracing::warn!(error = ?e, query = q, "doener_index city lookup failed");
                json!({"error": "doener_index API unavailable"})
            }
        },
    };

    ToolResultMessage::for_call(call, payload.to_string())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p core --show-progress=none --cargo-quiet --status-level=fail ai::doener_tool`
Expected: 7 tests passed.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/ai/doener_tool.rs
git commit -m "$(cat <<'EOF'
feat(ai): execute_doener_index returns JSON payload

Global vs city scopes, top-5 hits cap, transparent error surface so the
model can decide how to phrase failures.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Plumb `DoenerClient` into `AiCommandDeps` + `AiCommand`

**Files:**
- Modify: `crates/core/src/ai/command.rs`
- Modify: `crates/core/src/twitch/handlers/commands.rs`

- [ ] **Step 1: Add the field to `AiCommandDeps`**

In `crates/core/src/ai/command.rs`, in `pub struct AiCommandDeps` (currently ~line 90–110 region), add:

```rust
    pub doener: std::sync::Arc<crate::doener::DoenerClient>,
```

Add the matching field to `pub struct AiCommand` (in the same file, near `web: Option<AiWeb>` at line 91):

```rust
    doener: std::sync::Arc<crate::doener::DoenerClient>,
```

In the `AiCommand::new` builder (line 135ish, the `Self { ... }` literal), add:

```rust
            doener: deps.doener,
```

- [ ] **Step 2: Pass it at the call site**

In `crates/core/src/twitch/handlers/commands.rs`, in the `cmd_list.push(Box::new(ai::command::AiCommand::new(...)))` block (~line 230), add `doener: doener.clone(),` to the `AiCommandDeps { ... }` literal alongside the other fields.

- [ ] **Step 3: Verify compile**

Run: `cargo check --workspace --quiet`
Expected: success. `self.doener` field unused warning is acceptable here — fixed in Task 15.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/ai/command.rs crates/core/src/twitch/handlers/commands.rs
git commit -m "$(cat <<'EOF'
feat(ai): plumb DoenerClient into AiCommand

Field threaded through AiCommandDeps; used in the next commit by
V2Executor.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Route `doener_index` in `V2Executor`, push tool unconditionally

This is the wiring that makes the tool actually reachable from the model.

**Files:**
- Modify: `crates/core/src/ai/command.rs`

- [ ] **Step 1: Extend `V2Executor` with the doener field**

In `crates/core/src/ai/command.rs`, in `struct V2Executor<'a>` (currently ~line 149):

```rust
struct V2Executor<'a> {
    chat: &'a ChatTurnExecutor,
    web: Option<&'a content::ContentToolExecutor>,
    doener: &'a crate::doener::DoenerClient,
    trace: &'a TraceIds,
}
```

In the `impl ToolExecutor for V2Executor<'_>` `execute` method (currently ~line 157), route the doener call BEFORE the web branch (so it works even when `web` is `None`):

```rust
#[async_trait]
impl ToolExecutor for V2Executor<'_> {
    async fn execute(&self, call: &ToolCall) -> ToolResultMessage {
        if call.name == crate::ai::doener_tool::DOENER_TOOL_NAME {
            return crate::ai::doener_tool::execute_doener_index(self.doener, call).await;
        }
        if content::is_web_tool(&call.name) {
            match self.web {
                Some(w) => w.execute_tool_call(call, self.trace).await,
                None => ToolResultMessage::for_call(call, "unknown_tool".to_string()),
            }
        } else {
            self.chat.execute(call).await
        }
    }
}
```

- [ ] **Step 2: Update the `V2Executor` construction**

Find the `let combined_exec = V2Executor { ... }` (currently ~line 433):

```rust
        let combined_exec = V2Executor {
            chat: &exec,
            web: self.web.as_ref().map(|w| w.executor.as_ref()),
            doener: self.doener.as_ref(),
            trace: &trace,
        };
```

- [ ] **Step 3: Push `doener_tool()` into the tools list unconditionally**

Find the tools-assembly block (currently ~line 407):

```rust
        let mut tools = chat_turn_tools();
        tools.push(crate::ai::doener_tool::doener_tool());
        if self.web.is_some() {
            tools.extend(content::ai_tools());
        }
```

- [ ] **Step 4: Verify compile + run all tests**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: full pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/ai/command.rs
git commit -m "$(cat <<'EOF'
feat(ai): V2Executor routes doener_index unconditionally

Adds DOENER_TOOL_NAME as the first dispatch branch so the model can call
it even when [ai.web] is disabled. The tool def is now always present in
the chat turn's tools list.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Full pre-commit gate

The CI gate (`fmt + clippy + test`) blocks merges to `main`. Run it now so anything that slipped through nextest surfaces here, not in CI.

**Files:** none.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: silent. If files change, stage them in the same commit as Step 2 of this task.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: silent (no warnings).

If clippy fires on the new code, fix in place. Avoid `#[allow]` without a one-line reason — the repo enforces this.

- [ ] **Step 3: Full test suite**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: full pass.

- [ ] **Step 4: Cargo audit (optional but recommended locally)**

Run: `cargo audit`
Expected: 0 vulnerabilities. (No new deps added by this work, so the lockfile is unchanged — there should be nothing new to audit.)

- [ ] **Step 5: Commit any formatting changes**

```bash
git diff --name-only
# if non-empty:
git add -A
git commit -m "$(cat <<'EOF'
chore(doener): cargo fmt fallout

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Otherwise skip the commit.

---

## Task 17: Manual smoke check (optional, recommended)

Run the bot locally against the real upstream once to confirm end-to-end behavior, then revert any local config tweaks before pushing.

- [ ] **Step 1: Quick endpoint sanity**

Run: `curl -s 'https://xn--dnerindex-07a.com/api/stats.php' | jq .`
Expected: a `{"ok": true, ...}` payload (no auth).

- [ ] **Step 2: Build the bot**

Run: `cargo build --quiet`
Expected: success.

- [ ] **Step 3 (optional): Run the bot in a dev channel and try four message variants**

Manual: invoke `!dpi`, `!dpi Berlin`, `!dpi Han`, `!dpi NichtExistenteStadtXYZ` and confirm the responses match the formats from Task 3.

This step is optional because the unit + wiremock tests already cover every branch. Skip if the dev environment is not set up.

---

## Task 18: PR

**Files:** none.

- [ ] **Step 1: Push the branch**

Run:

```bash
git push -u origin worktree-feature+donerpreisindex:feature/donerpreisindex
```

(The worktree's branch is named `worktree-feature+donerpreisindex` locally, but the project convention is `feature/donerpreisindex` on origin.)

- [ ] **Step 2: Open the PR**

Run:

```bash
gh pr create --title "feat: dönerpreisindex command + AI tool (#178)" --body "$(cat <<'EOF'
## Summary
- !dpi chat command: global aggregate (no arg) and fuzzy city lookup with did-you-mean fallback.
- doener_index AI tool: exposes the same data to !ai; routed unconditionally so it works even when [ai.web] is disabled.
- Shared DoenerClient against https://xn--dnerindex-07a.com (stats.php + cities.php).

Closes #178.

## Test plan
- [ ] Wait for CI: fmt + clippy + test, cargo audit, hadolint, trivy, actionlint, zizmor, gitleaks.
- [ ] Manual: !dpi (global), !dpi Berlin (single hit short-circuit), !dpi Han (did-you-mean), !dpi xyz (not found), !ai "Wie teuer ist Döner in Hamburg?".

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Report the PR URL back to the user.**
