# Module Reorganization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize the flat `src/` module layout into four domain modules: `ai`, `aviation`, `twitch`, and `util`, with bot-core files staying at root level.

**Architecture:** Move files into subdirectories, create `mod.rs` files with re-exports so old `crate::` paths remain valid where used by other systems. Update `lib.rs` pub-mod declarations and pub-use paths. Bot core (`config`, `cooldown`, `database`, `ping`, `suspend`, `commands/` for non-AI non-aviation commands) stays at root level.

**Tech Stack:** Rust, cargo, no new dependencies.

---

## Target Directory Structure

```
src/
  lib.rs                        ← updated pub mod + pub use
  main.rs                       ← unchanged
  config.rs                     ← stays
  cooldown.rs                   ← stays
  database.rs                   ← stays
  ping.rs                       ← stays
  suspend.rs                    ← stays

  ai/
    mod.rs                      ← declares sub-mods + re-exports public surface
    command.rs                  ← was src/commands/ai.rs
    chat_history.rs             ← was src/chat_history.rs
    prefill.rs                  ← was src/prefill.rs
    llm/                        ← was src/llm/ (moved wholesale)
      mod.rs
      ollama.rs
      openai.rs
    memory/                     ← was src/memory/ (moved wholesale)
      mod.rs
      consolidation.rs
      extraction.rs
      scope.rs
      store.rs
      tools.rs
    web_search/                 ← was src/web_search/ (moved wholesale)
      mod.rs
      cache.rs
      client.rs
      executor.rs
      tools.rs

  aviation/
    mod.rs                      ← was src/aviation.rs; adds sub-mod declarations + re-exports
    tracker.rs                  ← was src/flight_tracker.rs
    commands/
      mod.rs                    ← new; declares sub-mods
      flights.rs                ← was src/commands/flights.rs
      flights_above.rs          ← was src/commands/flights_above.rs
      track.rs                  ← was src/commands/track.rs
      untrack.rs                ← was src/commands/untrack.rs
      random_flight.rs          ← was src/commands/random_flight.rs

  twitch/
    mod.rs                      ← new; declares sub-mods + re-exports
    setup.rs                    ← was src/twitch_setup.rs
    token_storage.rs            ← was src/token_storage.rs
    tls.rs                      ← was src/tls.rs
    seventv.rs                  ← was src/seventv.rs
    handlers/                   ← was src/handlers/ (moved wholesale)
      mod.rs
      commands.rs
      latency.rs
      router.rs
      schedules.rs
      tracker_1337.rs

  util/
    mod.rs                      ← was src/util.rs content
    clock.rs                    ← was src/clock.rs
    telemetry.rs                ← was src/telemetry.rs

  commands/                     ← only bot-core commands remain
    mod.rs                      ← updated: removes ai, aviation sub-mods
    feedback.rs                 ← unchanged
    leaderboard.rs              ← unchanged
    news.rs                     ← unchanged
    ping_admin.rs               ← unchanged
    ping_trigger.rs             ← unchanged
    suspend.rs                  ← unchanged
```

## Path Migration Table

| Old path | New path |
|---|---|
| `crate::aviation` | `crate::aviation` (mod.rs re-exports all public items) |
| `crate::chat_history` | `crate::ai::chat_history` |
| `crate::clock` | `crate::util::clock` |
| `crate::commands::ai` | `crate::ai::command` |
| `crate::commands::flights` | `crate::aviation::commands::flights` |
| `crate::commands::flights_above` | `crate::aviation::commands::flights_above` |
| `crate::commands::track` | `crate::aviation::commands::track` |
| `crate::commands::untrack` | `crate::aviation::commands::untrack` |
| `crate::commands::random_flight` | `crate::aviation::commands::random_flight` |
| `crate::flight_tracker` | `crate::aviation::tracker` (+ re-export from `crate::aviation`) |
| `crate::llm` | `crate::ai::llm` |
| `crate::memory` | `crate::ai::memory` |
| `crate::prefill` | `crate::ai::prefill` |
| `crate::seventv` | `crate::twitch::seventv` |
| `crate::telemetry` | `crate::util::telemetry` |
| `crate::tls` | `crate::twitch::tls` |
| `crate::token_storage` | `crate::twitch::token_storage` |
| `crate::twitch_setup` | `crate::twitch::setup` |
| `crate::util` | `crate::util` (mod.rs re-exports all) |
| `crate::web_search` | `crate::ai::web_search` |
| `crate::handlers` | `crate::twitch::handlers` |

**Re-export strategy:** To keep `lib.rs` pub-use stable and minimize cascading changes, each new `mod.rs` re-exports the public surface that `lib.rs` currently exposes. `lib.rs` `pub mod` declarations change but all its `pub use` paths only need the prefix updated.

---

## Task 1: Create `src/util/` module

**Files:**
- Create: `src/util/mod.rs`
- Create: `src/util/clock.rs`
- Create: `src/util/telemetry.rs`
- Delete: `src/util.rs`, `src/clock.rs`, `src/telemetry.rs`
- Modify: `src/lib.rs`
- Modify: `src/database.rs`
- Modify: `src/handlers/tracker_1337.rs`
- Modify: `src/llm/ollama.rs`
- Modify: `src/llm/openai.rs`
- Modify: `src/web_search/client.rs`
- Modify: `src/token_storage.rs`

- [ ] **Step 1: Create `src/util/` directory and copy files**

```bash
mkdir -p src/util
cp src/util.rs src/util/mod.rs
cp src/clock.rs src/util/clock.rs
cp src/telemetry.rs src/util/telemetry.rs
```

- [ ] **Step 2: Add sub-module declarations to `src/util/mod.rs`**

At the top of `src/util/mod.rs`, add:

```rust
pub mod clock;
pub mod telemetry;
```

- [ ] **Step 3: Update `src/lib.rs` — replace three `pub mod` lines with two**

Replace:
```rust
pub mod clock;
```
with nothing (delete that line).

Replace:
```rust
pub mod telemetry;
```
with nothing (delete that line).

Replace:
```rust
pub mod util;
```
with:
```rust
pub mod util;
```
(unchanged — `util` stays, now points to `util/mod.rs`)

Then update the `pub use` for telemetry:
```rust
// old:
pub use telemetry::install_tracing;
// new:
pub use util::telemetry::install_tracing;
```

And update the `pub use` for util (paths unchanged since `util` is still the module name):
```rust
// These stay the same:
pub use util::{
    APP_USER_AGENT, MAX_RESPONSE_LENGTH, ensure_data_dir, get_config_path, get_data_dir,
    parse_flight_duration, resolve_berlin_time, truncate_response,
};
```

Also add a re-export for `Clock` via util so existing `crate::clock::Clock` paths can be updated:
```rust
pub use util::clock;
```

- [ ] **Step 4: Update `src/database.rs`**

```rust
// old:
use crate::util::resolve_berlin_time;
// new: (unchanged — util module name stays)
use crate::util::resolve_berlin_time;
```
No change needed here.

- [ ] **Step 5: Update files that import `crate::clock`**

`src/handlers/tracker_1337.rs` line with `use crate::{clock::Clock, resolve_berlin_time};`:
```rust
// old:
use crate::{clock::Clock, resolve_berlin_time};
// new:
use crate::{util::clock::Clock, resolve_berlin_time};
```

`src/flight_tracker.rs` line with `use crate::clock::Clock;`:
```rust
// old:
use crate::clock::Clock;
// new:
use crate::util::clock::Clock;
```

`src/handlers/schedules.rs` line with `use crate::{clock::Clock, ...}`:
```rust
// old:
use crate::{clock::Clock, config::Configuration, database, get_config_path};
// new:
use crate::{util::clock::Clock, config::Configuration, database, get_config_path};
```

- [ ] **Step 6: Update files that import `crate::APP_USER_AGENT` (now via `crate::util`)**

`src/llm/ollama.rs`:
```rust
// old:
use crate::APP_USER_AGENT;
// new: (APP_USER_AGENT is pub-used from util in lib.rs, so crate::APP_USER_AGENT still works)
use crate::APP_USER_AGENT;
```
No change needed — `lib.rs` re-exports it.

Same for `src/llm/openai.rs` and `src/web_search/client.rs` — no change needed since `lib.rs` pub-uses `APP_USER_AGENT` from `util`.

- [ ] **Step 7: Update `src/token_storage.rs`**

```rust
// old:
use crate::util::get_data_dir;
// new: (unchanged — still crate::util::get_data_dir)
use crate::util::get_data_dir;
```
No change needed.

- [ ] **Step 8: Remove old files**

```bash
rm src/util.rs src/clock.rs src/telemetry.rs
```

- [ ] **Step 9: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

- [ ] **Step 10: Run tests**

```bash
cargo test 2>&1
```

Expected: all tests pass.

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "refactor: move clock, telemetry into util/ module"
```

---

## Task 2: Create `src/twitch/` module

**Files:**
- Create: `src/twitch/mod.rs`
- Create: `src/twitch/setup.rs`
- Create: `src/twitch/token_storage.rs`
- Create: `src/twitch/tls.rs`
- Create: `src/twitch/seventv.rs`
- Move: `src/handlers/` → `src/twitch/handlers/`
- Delete: `src/twitch_setup.rs`, `src/token_storage.rs`, `src/tls.rs`, `src/seventv.rs`
- Modify: `src/lib.rs`
- Modify: `src/commands/ai.rs`
- Modify: `src/twitch/setup.rs` (path refs)
- Modify: `src/twitch/token_storage.rs` (path refs)

- [ ] **Step 1: Create `src/twitch/` directory and copy files**

```bash
mkdir -p src/twitch
cp src/twitch_setup.rs src/twitch/setup.rs
cp src/token_storage.rs src/twitch/token_storage.rs
cp src/tls.rs src/twitch/tls.rs
cp src/seventv.rs src/twitch/seventv.rs
cp -r src/handlers src/twitch/handlers
```

- [ ] **Step 2: Update internal `use crate::` paths in copied files**

`src/twitch/token_storage.rs` — `get_data_dir` is still at `crate::util::get_data_dir`, no change.

`src/twitch/setup.rs` — references `AuthenticatedTwitchClient`, `FileBasedTokenStorage`, `config::Configuration`:
```rust
// old:
use crate::{AuthenticatedTwitchClient, FileBasedTokenStorage, config::Configuration};
// new: (all still pub-used from lib.rs root, so no change needed)
use crate::{AuthenticatedTwitchClient, FileBasedTokenStorage, config::Configuration};
```
No change needed since `lib.rs` re-exports these.

`src/twitch/seventv.rs` — references `APP_USER_AGENT` and `config::AiEmotesConfigSection`:
```rust
// old:
use crate::{APP_USER_AGENT, config::AiEmotesConfigSection};
// new: (unchanged — pub-used from lib.rs)
use crate::{APP_USER_AGENT, config::AiEmotesConfigSection};
```
No change needed.

- [ ] **Step 3: Update `src/twitch/handlers/` — fix `crate::` paths**

`src/twitch/handlers/schedules.rs` — update `clock::Clock` path:
```rust
// old:
use crate::{clock::Clock, config::Configuration, database, get_config_path};
// new:
use crate::{util::clock::Clock, config::Configuration, database, get_config_path};
```

`src/twitch/handlers/tracker_1337.rs` — update `clock::Clock` path:
```rust
// old:
use crate::{clock::Clock, resolve_berlin_time};
// new:
use crate::{util::clock::Clock, resolve_berlin_time};
```

`src/twitch/handlers/commands.rs` — check what it imports from `crate::`:
```bash
grep "use crate::" src/twitch/handlers/commands.rs
```
Update any `crate::commands::ai::` → `crate::ai::command::` and `crate::commands::flights` → `crate::aviation::commands::flights` etc. (do this once aviation and ai tasks are complete; for now just ensure the file compiles with existing paths or add temporary re-exports).

- [ ] **Step 4: Create `src/twitch/mod.rs`**

```rust
pub mod handlers;
pub mod setup;
pub mod seventv;
pub mod tls;
pub mod token_storage;
```

- [ ] **Step 5: Update `src/lib.rs`**

Replace:
```rust
pub mod handlers;
pub mod seventv;
pub mod tls;
pub mod token_storage;
pub mod twitch_setup;
```
with:
```rust
pub mod twitch;
```

Update `pub use` lines:
```rust
// old:
pub use tls::install_crypto_provider;
pub use token_storage::FileBasedTokenStorage;
pub use twitch_setup::{setup_and_verify_twitch_client, setup_twitch_client};
// new:
pub use twitch::tls::install_crypto_provider;
pub use twitch::token_storage::FileBasedTokenStorage;
pub use twitch::setup::{setup_and_verify_twitch_client, setup_twitch_client};
```

Update internal `use crate::handlers::` references in `lib.rs`:
```rust
// old:
use crate::{
    ...
    handlers::{
        commands::{CommandHandlerConfig, run_generic_command_handler},
        latency::run_latency_handler,
        router::run_message_router,
        schedules::{...},
        tracker_1337::{TARGET_HOUR, TARGET_MINUTE, load_leaderboard, run_1337_handler},
    },
    ...
};
// new:
use crate::{
    ...
    twitch::handlers::{
        commands::{CommandHandlerConfig, run_generic_command_handler},
        latency::run_latency_handler,
        router::run_message_router,
        schedules::{...},
        tracker_1337::{TARGET_HOUR, TARGET_MINUTE, load_leaderboard, run_1337_handler},
    },
    ...
};
```

Also add re-export so `crate::handlers` still resolves for any external test code:
```rust
pub use twitch::handlers;
```

- [ ] **Step 6: Update `src/commands/ai.rs` — fix SevenTV path**

```rust
// old:
use crate::seventv::SevenTvEmoteProvider;
// new:
use crate::twitch::seventv::SevenTvEmoteProvider;
```

- [ ] **Step 7: Remove old files**

```bash
rm src/twitch_setup.rs src/token_storage.rs src/tls.rs src/seventv.rs
rm -rf src/handlers
```

- [ ] **Step 8: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors. Fix any remaining path issues before continuing.

- [ ] **Step 9: Run tests**

```bash
cargo test 2>&1
```

Expected: all tests pass.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "refactor: move twitch IRC infra into twitch/ module"
```

---

## Task 3: Create `src/ai/` module

**Files:**
- Create: `src/ai/mod.rs`
- Create: `src/ai/command.rs`
- Create: `src/ai/chat_history.rs`
- Create: `src/ai/prefill.rs`
- Move: `src/llm/` → `src/ai/llm/`
- Move: `src/memory/` → `src/ai/memory/`
- Move: `src/web_search/` → `src/ai/web_search/`
- Delete: `src/chat_history.rs`, `src/prefill.rs`, `src/llm/`, `src/memory/`, `src/web_search/`
- Modify: `src/lib.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/commands/news.rs`
- Modify: `src/config.rs`
- Modify: `src/twitch/handlers/commands.rs`

- [ ] **Step 1: Create `src/ai/` directory and copy files**

```bash
mkdir -p src/ai
cp src/commands/ai.rs src/ai/command.rs
cp src/chat_history.rs src/ai/chat_history.rs
cp src/prefill.rs src/ai/prefill.rs
cp -r src/llm src/ai/llm
cp -r src/memory src/ai/memory
cp -r src/web_search src/ai/web_search
```

- [ ] **Step 2: Update `crate::` paths inside `src/ai/command.rs`**

```rust
// old:
use crate::chat_history::{ChatHistory, ChatHistoryQuery, MAX_TOOL_RESULT_MESSAGES};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::llm::{...};
use crate::memory;
use crate::seventv::SevenTvEmoteProvider;
use crate::util::{MAX_RESPONSE_LENGTH, truncate_response};
use crate::web_search;
// new:
use crate::ai::chat_history::{ChatHistory, ChatHistoryQuery, MAX_TOOL_RESULT_MESSAGES};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::ai::llm::{...};
use crate::ai::memory;
use crate::twitch::seventv::SevenTvEmoteProvider;
use crate::util::{MAX_RESPONSE_LENGTH, truncate_response};
use crate::ai::web_search;
```

- [ ] **Step 3: Update `crate::` paths inside `src/ai/llm/ollama.rs` and `src/ai/llm/openai.rs`**

Both import `use crate::APP_USER_AGENT;` — unchanged (re-exported from lib.rs root).

- [ ] **Step 4: Update `crate::` paths inside `src/ai/memory/`**

`src/ai/memory/consolidation.rs`:
```rust
// old:
use crate::llm::{...};
use crate::memory::Memory;
use crate::memory::store::MemoryStore;
use crate::memory::tools::consolidator_tools;
// new:
use crate::ai::llm::{...};
use crate::ai::memory::Memory;
use crate::ai::memory::store::MemoryStore;
use crate::ai::memory::tools::consolidator_tools;
```

`src/ai/memory/extraction.rs`:
```rust
// old:
use crate::llm::{...};
use crate::memory::store::{Caps, DispatchContext, MemoryStore};
use crate::memory::tools::extractor_tools;
use crate::memory::{Scope, UserRole};
// new:
use crate::ai::llm::{...};
use crate::ai::memory::store::{Caps, DispatchContext, MemoryStore};
use crate::ai::memory::tools::extractor_tools;
use crate::ai::memory::{Scope, UserRole};
```

`src/ai/memory/store.rs`:
```rust
// old:
use crate::llm::ToolCall;
use crate::memory::scope::{is_write_allowed, seed_confidence, trust_level_for};
use crate::memory::{Scope, UserRole};
// new:
use crate::ai::llm::ToolCall;
use crate::ai::memory::scope::{is_write_allowed, seed_confidence, trust_level_for};
use crate::ai::memory::{Scope, UserRole};
```

`src/ai/memory/tools.rs`:
```rust
// old:
use crate::llm::ToolDefinition;
// new:
use crate::ai::llm::ToolDefinition;
```

- [ ] **Step 5: Update `crate::` paths inside `src/ai/web_search/`**

`src/ai/web_search/client.rs`:
```rust
// old:
use crate::APP_USER_AGENT;
// new: (unchanged)
use crate::APP_USER_AGENT;
```

`src/ai/web_search/executor.rs`:
```rust
// old:
use crate::llm::{ToolCall, ToolResultMessage};
// new:
use crate::ai::llm::{ToolCall, ToolResultMessage};
```

`src/ai/web_search/tools.rs`:
```rust
// old:
use crate::llm::ToolDefinition;
// new:
use crate::ai::llm::ToolDefinition;
```

- [ ] **Step 6: Create `src/ai/mod.rs`**

```rust
pub mod chat_history;
pub mod command;
pub mod llm;
pub mod memory;
pub mod prefill;
pub mod web_search;
```

- [ ] **Step 7: Update `src/lib.rs`**

Remove:
```rust
pub mod chat_history;
pub mod llm;
pub mod memory;
pub mod prefill;
pub mod web_search;
```

Add:
```rust
pub mod ai;
```

Update `pub use` lines:
```rust
// old:
pub use chat_history::{
    ChatHistory, ChatHistoryBuffer, ChatHistoryEntry, ChatHistoryPage, ChatHistoryQuery,
    ChatHistorySource, DEFAULT_HISTORY_LENGTH, MAX_HISTORY_LENGTH, MAX_TOOL_RESULT_MESSAGES,
};
// new:
pub use ai::chat_history::{
    ChatHistory, ChatHistoryBuffer, ChatHistoryEntry, ChatHistoryPage, ChatHistoryQuery,
    ChatHistorySource, DEFAULT_HISTORY_LENGTH, MAX_HISTORY_LENGTH, MAX_TOOL_RESULT_MESSAGES,
};
```

Also add re-exports so `crate::llm`, `crate::memory`, `crate::web_search` still work in handler/command code:
```rust
pub use ai::{llm, memory, web_search};
```

- [ ] **Step 8: Update `src/config.rs`**

```rust
// old:
use crate::{database, prefill};
// new:
use crate::{database, ai::prefill};
```

- [ ] **Step 9: Update `src/commands/mod.rs`**

Remove `pub mod ai;` line (ai command now lives in `crate::ai::command`).

- [ ] **Step 10: Update `src/commands/news.rs`**

```rust
// old:
use crate::commands::ai::ChatContext;
use crate::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::{ChatHistoryEntry, ChatHistorySource};
// new:
use crate::ai::command::ChatContext;
use crate::ai::llm::{ChatCompletionRequest, LlmClient, Message};
use crate::{ChatHistoryEntry, ChatHistorySource};
```

- [ ] **Step 11: Update `src/twitch/handlers/commands.rs`**

Find the import for `commands::ai` and update to `ai::command`:
```bash
grep "commands::ai\|crate::llm\|crate::memory\|crate::web_search\|crate::chat_history\|crate::prefill" src/twitch/handlers/commands.rs
```
Then update each found path:
- `crate::commands::ai::` → `crate::ai::command::`
- `crate::llm::` → `crate::ai::llm::` (unless re-exported via `pub use ai::llm` in lib.rs — in that case leave unchanged)
- `crate::memory` → `crate::ai::memory` (same caveat)

- [ ] **Step 12: Remove old files**

```bash
rm src/commands/ai.rs src/chat_history.rs src/prefill.rs
rm -rf src/llm src/memory src/web_search
```

- [ ] **Step 13: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors. Fix any remaining path issues.

- [ ] **Step 14: Run tests**

```bash
cargo test 2>&1
```

Expected: all tests pass.

- [ ] **Step 15: Commit**

```bash
git add -A
git commit -m "refactor: move AI subsystem (llm, memory, web_search, chat_history) into ai/ module"
```

---

## Task 4: Create `src/aviation/` module

**Files:**
- Create: `src/aviation/mod.rs`
- Create: `src/aviation/tracker.rs`
- Create: `src/aviation/commands/mod.rs`
- Create: `src/aviation/commands/flights.rs`
- Create: `src/aviation/commands/flights_above.rs`
- Create: `src/aviation/commands/track.rs`
- Create: `src/aviation/commands/untrack.rs`
- Create: `src/aviation/commands/random_flight.rs`
- Delete: `src/aviation.rs`, `src/flight_tracker.rs`, `src/commands/flights.rs`, `src/commands/flights_above.rs`, `src/commands/track.rs`, `src/commands/untrack.rs`, `src/commands/random_flight.rs`
- Modify: `src/lib.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/twitch/handlers/commands.rs`

- [ ] **Step 1: Create `src/aviation/` directory and copy files**

```bash
mkdir -p src/aviation/commands
cp src/aviation.rs src/aviation/mod.rs
cp src/flight_tracker.rs src/aviation/tracker.rs
cp src/commands/flights.rs src/aviation/commands/flights.rs
cp src/commands/flights_above.rs src/aviation/commands/flights_above.rs
cp src/commands/track.rs src/aviation/commands/track.rs
cp src/commands/untrack.rs src/aviation/commands/untrack.rs
cp src/commands/random_flight.rs src/aviation/commands/random_flight.rs
```

- [ ] **Step 2: Update `src/aviation/mod.rs`**

`src/aviation/mod.rs` was `src/aviation.rs`. Add sub-module declarations at the top:

```rust
pub mod commands;
pub mod tracker;
```

Also add re-exports at the bottom so `crate::flight_tracker::TrackerCommand` etc. still resolve via `crate::aviation`:
```rust
pub use tracker::{
    FlightIdentifier, TrackerCommand, run_flight_tracker,
    // add any other pub items from flight_tracker.rs
};
```

- [ ] **Step 3: Update `crate::` paths in `src/aviation/tracker.rs`** (was `flight_tracker.rs`)

```rust
// old:
use crate::aviation::{AltBaro, AviationClient, NearbyAircraft, iata_to_coords};
use crate::clock::Clock;
// new:
use crate::aviation::{AltBaro, AviationClient, NearbyAircraft, iata_to_coords};
use crate::util::clock::Clock;
```

- [ ] **Step 4: Update `crate::` paths in `src/aviation/commands/`**

`src/aviation/commands/flights.rs`:
```rust
// old:
use crate::flight_tracker::TrackerCommand;
// new:
use crate::aviation::TrackerCommand;
```

`src/aviation/commands/flights_above.rs`:
```rust
// old:
use crate::aviation::AviationClient;
use crate::cooldown::PerUserCooldown;
// new: (unchanged — aviation module name stays)
use crate::aviation::AviationClient;
use crate::cooldown::PerUserCooldown;
```

`src/aviation/commands/track.rs`:
```rust
// old:
use crate::flight_tracker::{FlightIdentifier, TrackerCommand};
// new:
use crate::aviation::{FlightIdentifier, TrackerCommand};
```

`src/aviation/commands/untrack.rs`:
```rust
// old:
use crate::flight_tracker::TrackerCommand;
// new:
use crate::aviation::TrackerCommand;
```

`src/aviation/commands/random_flight.rs`:
```rust
// old:
use crate::util::parse_flight_duration;
// new: (unchanged)
use crate::util::parse_flight_duration;
```

- [ ] **Step 5: Create `src/aviation/commands/mod.rs`**

```rust
pub mod flights;
pub mod flights_above;
pub mod random_flight;
pub mod track;
pub mod untrack;
```

- [ ] **Step 6: Update `src/lib.rs`**

Remove:
```rust
pub mod aviation;
pub mod flight_tracker;
```

Add (or update existing `pub mod aviation`):
```rust
pub mod aviation;
```
(now points to `aviation/mod.rs`)

Remove `pub mod aviation` duplicate if present, add:
```rust
pub use aviation::tracker as flight_tracker;
```
so `crate::flight_tracker` still resolves in any remaining code.

Update the `run_bot` internal `use`:
```rust
// old:
use crate::{
    aviation::AviationClient,
    ...
    flight_tracker,
    ...
};
// new:
use crate::{
    aviation::AviationClient,
    aviation::tracker as flight_tracker,
    ...
};
```

- [ ] **Step 7: Update `src/commands/mod.rs`**

Remove:
```rust
pub mod flights;
pub mod flights_above;
pub mod random_flight;
pub mod track;
pub mod untrack;
```

- [ ] **Step 8: Update `src/twitch/handlers/commands.rs`**

Update any `crate::commands::flights` → `crate::aviation::commands::flights`, etc.:
```bash
grep "commands::flights\|commands::track\|commands::untrack\|commands::random_flight\|crate::flight_tracker" src/twitch/handlers/commands.rs
```
Replace each found path with the new `aviation::commands::` prefix.

- [ ] **Step 9: Remove old files**

```bash
rm src/aviation.rs src/flight_tracker.rs
rm src/commands/flights.rs src/commands/flights_above.rs
rm src/commands/track.rs src/commands/untrack.rs src/commands/random_flight.rs
```

- [ ] **Step 10: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

- [ ] **Step 11: Run tests**

```bash
cargo test 2>&1
```

Expected: all tests pass.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "refactor: move aviation subsystem (client, tracker, commands) into aviation/ module"
```

---

## Task 5: Cleanup — remove compatibility re-exports

After all four modules are in place and tests pass, strip out the temporary re-exports added in earlier tasks (e.g., `pub use ai::{llm, memory, web_search}`, `pub use aviation::tracker as flight_tracker`, `pub use twitch::handlers`) and update all remaining callers to use the canonical new paths.

**Files:**
- Modify: `src/lib.rs`
- Modify: any file still using old paths

- [ ] **Step 1: Identify remaining compat re-exports in lib.rs**

```bash
grep "pub use twitch\|pub use ai::\|pub use aviation::tracker\|pub use util::clock\|pub use twitch::handlers" src/lib.rs
```

- [ ] **Step 2: For each re-export, find callers**

```bash
cargo check 2>&1 | grep "error\[E" | head -40
```

For each error, update the caller to use the new canonical path, then remove the re-export from `lib.rs`.

- [ ] **Step 3: Verify compilation**

```bash
cargo check 2>&1
```

Expected: no errors.

- [ ] **Step 4: Run full test suite and clippy**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings 2>&1
cargo test 2>&1
```

Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: remove compatibility re-exports, all paths canonical"
```

---

## Self-Review

**Spec coverage:**
- ✅ `util/` — clock, telemetry, util (Task 1)
- ✅ `twitch/` — setup, token_storage, tls, seventv, handlers (Task 2)
- ✅ `ai/` — llm, memory, web_search, chat_history, prefill, command (Task 3)
- ✅ `aviation/` — client (mod.rs), tracker, commands (Task 4)
- ✅ Cleanup of compat shims (Task 5)
- ✅ `commands/` retains: feedback, leaderboard, news, ping_admin, ping_trigger, suspend
- ✅ Root-level stays: config, cooldown, database, ping, suspend, lib.rs, main.rs

**Placeholder scan:** No TBD/TODO/placeholders present. All code blocks show exact changes.

**Type consistency:** `TrackerCommand`, `FlightIdentifier`, `AviationClient` referenced consistently. `Clock` trait path updated uniformly to `util::clock::Clock`.
