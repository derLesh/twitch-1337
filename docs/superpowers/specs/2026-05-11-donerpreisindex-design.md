# Dönerpreisindex command + AI tool

Issue: [#178](https://github.com/Chronophylos/twitch-1337/issues/178)

## Goal

Expose the public dönerpreisindex dataset (https://xn--dnerindex-07a.com) to chat
as `!dpi [city]` and to the AI as a `doener_index(city?)` tool. Single
implementation, two surfaces.

## Upstream API

Two endpoints used. Both return `application/json`, no auth, public site.

### `GET /api/stats.php`

Global aggregate. Example response:

```json
{
  "ok": true,
  "total_locations": 6092,
  "total_cities": 2202,
  "min_price": 5.5,
  "max_price": 9,
  "avg_price": 6.1,
  "locations_no_price": 5304,
  "locations_no_price_pct": 87.1
}
```

### `GET /api/cities.php?q=<prefix>`

Fuzzy/prefix city search. Returns matched cities sorted (empirically by
`location_count` descending). Example for `q=Han`:

```json
{
  "ok": true,
  "query": "Han",
  "count": 5,
  "cities": [
    {"city": "Hannover",  "zip": "30459", "location_count": 51, "min_price": "6.00", "max_price": "6.00", "avg_price": "6.00"},
    {"city": "Hanau",     "zip": "63456", "location_count": 3,  "min_price": "6.00", "max_price": "6.00", "avg_price": "6.00"},
    {"city": "Handewitt", "zip": "24983", "location_count": 1,  "min_price": null,   "max_price": null,   "avg_price": null}
  ]
}
```

Prices are quoted strings; `null` when no location in the city has a price.
`zip` is one representative postcode (not a list).

### Not used

- `/api/city.php?q=<exact>` — would return the full location array (324 entries
  for Berlin). Same aggregate fields are already in `cities.php`. No reason to
  call it.
- `/api/locations.php` — full dump. Too large.

## Module layout

```
crates/core/src/doener/
  mod.rs        # re-exports
  client.rs     # DoenerClient
  types.rs      # GlobalStats, CityHit, CitiesResponse
  format.rs     # chat formatters
```

`DoenerClient` is constructed once in `Services` (`crates/core/src/lib.rs` /
wherever services are wired) and shared as `Arc<DoenerClient>` between the chat
command and the AI tool executor.

### `client.rs`

```rust
pub struct DoenerClient {
    http: reqwest::Client,
    base_url: String, // injected for tests; defaults to BASE_URL const
}

impl DoenerClient {
    pub fn new() -> Result<Self>;                       // 5s timeout, UA "twitch-1337/<pkg_version>"
    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self; // tests
    pub async fn stats(&self) -> Result<GlobalStats>;
    pub async fn search_cities(&self, q: &str) -> Result<Vec<CityHit>>;
}
```

Constants in `client.rs`:

```rust
const BASE_URL: &str = "https://xn--dnerindex-07a.com";
const TIMEOUT: Duration = Duration::from_secs(5);
```

Errors bubble up as `eyre::Result`. Caller logs with `?error` and falls back to
a user-visible "API down" message.

### `types.rs`

`Deserialize`-only structs. Subset of upstream fields — drop anything the bot
does not display.

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct GlobalStats {
    pub total_locations: u32,
    pub total_cities: u32,
    pub min_price: f64,
    pub max_price: f64,
    pub avg_price: f64,
    pub locations_no_price_pct: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CityHit {
    pub city: String,
    pub location_count: u32,
    #[serde(deserialize_with = "deser_optional_f64_str")]
    pub min_price: Option<f64>,
    #[serde(deserialize_with = "deser_optional_f64_str")]
    pub max_price: Option<f64>,
    #[serde(deserialize_with = "deser_optional_f64_str")]
    pub avg_price: Option<f64>,
}
```

Prices are strings upstream (`"6.00"`) or `null`. Custom deserializer parses
both forms; unparseable strings become `None`.

`CitiesResponse` is a thin envelope with `cities: Vec<CityHit>`. The client
returns `Vec<CityHit>` to callers — envelope stays internal.

### `format.rs`

Pure functions, return `String`. No I/O. Easy unit tests against fixtures.

```rust
pub fn format_global(s: &GlobalStats) -> String;
pub fn format_city(c: &CityHit) -> String;             // for single best match
pub fn format_did_you_mean(hits: &[CityHit]) -> String; // 2..N matches, top 3
pub fn format_not_found(query: &str) -> String;
```

Output (German, matches `news.rs` tone, no Markdown):

| Case | Output |
|---|---|
| global | `Döner-Index DE: 6092 Buden in 2202 Städten, ⌀ 6.10€ (5.50–9.00€). 87% ohne Preis.` |
| city with price | `Hannover: 51 Buden, ⌀ 6.00€ (6.00–6.00€).` |
| city no price | `Handewitt: 1 Bude, noch keine Preise.` |
| 2–3 hits | `Meintest du: Hannover (51), Hanau (3), Handewitt (1)?` |
| 0 hits | `FeelsDankMan keine Stadt für 'xyz' gefunden.` |
| API down | `FeelsDankMan döner-index API down` |

Numbers: avg/min/max formatted as `{:.2}€`. `location_count == 1` → `"Bude"`,
else `"Buden"`.

## Chat command

`crates/core/src/commands/doener.rs` — implements the `Command` trait used by
the existing dispatcher in `crates/core/src/commands/mod.rs`. Registered next
to the other commands in the command-handler wiring.

Trigger: `!dpi`.

Behavior:

1. Strip trigger, trim arg.
2. Empty arg → `client.stats()` → `format_global`.
3. Non-empty arg → `client.search_cities(arg)`.
   - 0 hits → `format_not_found`.
   - exactly 1 hit → `format_city`.
   - 2+ hits, but the first hit's city name equals the query case-insensitively
     → treat as 1 hit (`format_city` on the first). Avoids `!dpi Berlin`
     returning "Meintest du: Berlin (324), Berlingerode (1)?".
   - otherwise → `format_did_you_mean` (top 3).

Cooldown: per-user, default 30s, via existing `PerUserCooldown` like `!up`.
Add `pub doener: u64` to `CooldownsConfig` in `crates/core/src/config.rs`
with `default_doener_cooldown() -> 30` and update the `Default` impl and the
TOML example.

`config.toml.example` `[cooldowns]` block gains:

```toml
# doener = 30   # !dpi command (default: 30)
```

No `[doener]` config block. Base URL and timeout are consts.

## AI tool

The existing `ai_tools()` in `crates/core/src/ai/content/tools.rs` is only
pushed when `[ai.web]` is enabled (see `crates/core/src/ai/command.rs` near
line 408: `if self.web.is_some() { tools.extend(content::ai_tools()); }`).
The doener tool must work even when web is disabled, so it does not live
inside `ContentToolExecutor`. Instead it gets its own thin module.

New module `crates/core/src/ai/doener_tool.rs`:

```rust
use std::sync::Arc;
use llm::{ToolCall, ToolDefinition, ToolResultMessage};
use serde::Deserialize;
use serde_json::json;

use crate::doener::DoenerClient;

pub const DOENER_TOOL_NAME: &str = "doener_index";

#[derive(Debug, Deserialize)]
struct DoenerArgs {
    #[serde(default)]
    city: Option<String>,
}

pub fn doener_tool() -> ToolDefinition {
    ToolDefinition {
        name: DOENER_TOOL_NAME.into(),
        description: "Look up the German Döner price index from dönerindex.com. \
            Without `city`, returns the country-wide aggregate (location count, \
            avg/min/max price). With `city` (free-form), returns the top matching \
            cities and their per-city aggregate. Use this for any question about \
            Döner prices, kebab prices, or how expensive Döner is in a German city.".into(),
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

pub async fn execute_doener_index(
    client: &DoenerClient,
    call: &ToolCall,
) -> ToolResultMessage {
    let args = match call.parse_args::<DoenerArgs>() {
        Ok(a) => a,
        Err(e) => {
            return ToolResultMessage::for_call(
                call,
                json!({"error": "invalid_arguments", "details": e.to_string()}).to_string(),
            );
        }
    };

    let payload = match args.city.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => match client.stats().await {
            Ok(stats) => json!({"scope": "global", "stats": stats}),
            Err(_) => json!({"error": "doener_index API unavailable"}),
        },
        Some(q) => match client.search_cities(q).await {
            Ok(hits) => {
                let top: Vec<_> = hits.into_iter().take(5).collect();
                json!({"scope": "city", "query": q, "hits": top})
            }
            Err(_) => json!({"error": "doener_index API unavailable"}),
        },
    };
    ToolResultMessage::for_call(call, payload.to_string())
}
```

`GlobalStats` and `CityHit` derive `Serialize` (in addition to `Deserialize`)
so the `serde_json::json!` macro can embed them. They contain no secrets, so
the project's "no Serialize on config" rule does not apply.

`AiCommand` is wired to receive the client and dispatch the tool:

- `AiCommandDeps` grows `pub doener: Arc<DoenerClient>`.
- `AiCommand` stores it in a `doener: Arc<DoenerClient>` field.
- `V2Executor` grows `doener: &'a DoenerClient` and routes
  `DOENER_TOOL_NAME` calls to `execute_doener_index`, regardless of whether
  `web` is `Some`.
- `command.rs` push site (line 407–410) becomes:

  ```rust
  let mut tools = chat_turn_tools();
  tools.push(crate::ai::doener_tool::doener_tool());
  if self.web.is_some() {
      tools.extend(content::ai_tools());
  }
  ```

- `is_web_tool` is **not** modified. `doener_index` is dispatched by an
  explicit name check inside `V2Executor::execute` placed before the
  `is_web_tool` branch.

The tool ships any time `[ai]` is configured, regardless of `[ai.web]`.

## Wiring

`Services` (`crates/core/src/lib.rs`) grows:

```rust
pub doener: Arc<DoenerClient>,
```

Constructed once at startup in `crates/twitch-1337/src/main.rs` with
`Arc::new(DoenerClient::new()?)`. `DoenerClient::new()` only fails on an
invalid reqwest builder (OS-level TLS issue), which is fatal anyway, so the
`?` propagation aborts startup the same way as other infrastructure-level
errors.

`run_bot` destructures the new field. `CommandHandlerConfig` and
`AiCommandDeps` both grow `pub doener: Arc<DoenerClient>`. The
`run_generic_command_handler` body clones it once for the chat command and
once for `AiCommandDeps`.

Consumers:
- `!dpi` command (`crates/core/src/commands/doener.rs`)
- `V2Executor` inside `AiCommand` (`crates/core/src/ai/command.rs`) routes
  `doener_index` to `crate::ai::doener_tool::execute_doener_index`.

## Error handling

Single best-effort HTTP call per invocation. No retries, no fallback host.

- reqwest error / timeout / non-2xx → `Err(eyre!(...))`, logged with `?error`
  and `endpoint = "stats"|"cities"`, query (if present).
- `ok: false` in response body (defensive — never observed) → treated as
  failure.
- JSON parse failure → failure.

Chat: `"FeelsDankMan döner-index API down"`. AI tool: `{"error": "..."}` — the
model decides how to surface this; existing executor pattern.

## Tests

### Unit

- `format.rs` against hand-written `GlobalStats` / `CityHit` fixtures, all
  branches (global, single, plural-hits, no-hits, missing prices, singular vs
  plural "Bude/Buden").
- `types.rs` deserializer: stringy `"6.00"`, `null`, and a malformed string
  all yield expected `Option<f64>`.

### Client (wiremock)

`crates/core/src/doener/client.rs#[cfg(test)] mod tests`:

- `stats` 200 → struct populated correctly.
- `stats` 500 → `Err`.
- `cities` 200 with three hits → struct populated, order preserved.
- `cities` 200 with empty `cities` → `Ok(vec![])`.
- timeout (delayed response > 5s) → `Err`. Use a shorter test timeout via
  `with_base_url` + a low-timeout test-only `reqwest::Client` to keep the
  test fast.

### Doener tool

`crates/core/src/ai/doener_tool.rs#[cfg(test)] mod tests`:

- `doener_tool()` returns a definition named `"doener_index"`.
- `execute_doener_index` with empty/missing `city` → JSON contains
  `"scope":"global"` and the parsed stats.
- `execute_doener_index` with a matching city → JSON contains
  `"scope":"city"` and a non-empty `hits` array.
- `execute_doener_index` with a query that returns 0 hits → `"hits":[]`.
- Args parse error path (model passes `{"city": 123}`) → returned JSON
  contains `"error":"invalid_arguments"`.

Use wiremock with a base URL injected into the test `DoenerClient`.

### Command

`crates/core/src/commands/doener.rs` — unit-test the branches via the
`format::*` helpers (already tested) plus a wiremock-backed test using
`TestBotBuilder` from `crates/core/tests/common/` for the global branch.
Single happy-path integration test is enough — per-branch coverage lives in
the format and client tests.

### `ai_tools()` test

Existing `ai_tools_surface_contains_search_and_read` test asserts exact tool
order from `ai_tools()`. Do not modify it — the doener tool is not added by
`ai_tools()`. Add a separate test in `crates/core/src/ai/doener_tool.rs`
asserting that `is_web_tool(DOENER_TOOL_NAME)` is `false`, so a future
refactor cannot silently regress by gating the doener tool behind `[ai.web]`.

## Out of scope

- Submitting prices (`/api/submit.php` exists, would need auth/captcha).
- Map / location-level detail (would need `city.php` + truncation logic).
- Caching / TTL. Upstream is cheap and small; revisit if rate-limited.
- Localization. Output is German; bot is German.
- A `[doener]` config block. Not needed today.

## Files touched

New:
- `crates/core/src/doener/mod.rs`
- `crates/core/src/doener/client.rs`
- `crates/core/src/doener/types.rs`
- `crates/core/src/doener/format.rs`
- `crates/core/src/commands/doener.rs`
- `crates/core/src/ai/doener_tool.rs`
- `docs/superpowers/specs/2026-05-11-donerpreisindex-design.md` (this file)

Modified:
- `crates/core/src/lib.rs` — `pub mod doener;`, `Services.doener`, destructure
  the new field, thread it through `SpawnDeps`/`spawn_handlers` to the
  command handler.
- `crates/core/src/ai/mod.rs` — `pub mod doener_tool;`.
- `crates/core/src/commands/mod.rs` — `pub mod doener;`.
- `crates/core/src/twitch/handlers/commands.rs` — `CommandHandlerConfig.doener`,
  destructure it, push `DoenerCommand` into `cmd_list`, pass into
  `AiCommandDeps`.
- `crates/core/src/twitch/handlers/spawn.rs` — `SpawnDeps.doener`, plumb to
  `CommandHandlerConfig`.
- `crates/core/src/ai/command.rs` — `AiCommandDeps.doener`,
  `AiCommand.doener`, push `doener_tool()` unconditionally, route the call
  in `V2Executor::execute`.
- `crates/core/src/config.rs` — `CooldownsConfig.doener` with
  `default_doener_cooldown() -> 30`.
- `crates/twitch-1337/src/main.rs` — construct `DoenerClient`, place in
  `Services`.
- `crates/twitch-1337/config.toml.example` — `# doener = 30` line.

Cargo: no new dependencies (reqwest, serde, eyre, wiremock, async-trait
already in tree).

Cargo: no new dependencies (reqwest, serde, eyre, wiremock, async-trait
already in tree).
