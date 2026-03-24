# Random Flight Command (`!fl`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `!fl <AIRCRAFT> <DURATION>` chat command that generates a random flight plan and replies with a compact one-liner.

**Architecture:** Add `random-flight` as a path dependency. Parse the command in the existing `handle_generic_commands()` dispatch, call the crate's `generate_flight_plan()` in a blocking task, format the result, and reply.

**Tech Stack:** Rust, tokio, random-flight crate, twitch-irc

**Spec:** `docs/superpowers/specs/2026-03-24-random-flight-command-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `Cargo.toml` | Modify | Add `random-flight` path dependency |
| `src/main.rs` | Modify | Add `!fl` dispatch + `flight_command()` fn + `parse_flight_duration()` helper |

---

### Task 1: Add `random-flight` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, add under `[dependencies]`:

```toml
random-flight = { path = "../random-flight" }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles successfully (warnings OK)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat: add random-flight crate dependency"
```

---

### Task 2: Add `parse_flight_duration()` helper

**Files:**
- Modify: `src/main.rs`

This is a small helper that parses compact duration strings like `1h`, `30m`, `2h30m` into `std::time::Duration`. Placed as a standalone function near the other helper functions.

- [ ] **Step 1: Write the `parse_flight_duration` function**

Add this function in `src/main.rs` (near the other utility functions, before or after `one_of`):

```rust
/// Parse a compact duration string like "1h", "30m", "2h30m" into a Duration.
fn parse_flight_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }

    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else if ch == 'h' {
            let hours: u64 = current_num.parse().ok()?;
            total_secs += hours * 3600;
            current_num.clear();
        } else if ch == 'm' {
            let minutes: u64 = current_num.parse().ok()?;
            total_secs += minutes * 60;
            current_num.clear();
        } else {
            return None;
        }
    }

    // Reject if there are leftover digits (no unit suffix) or zero duration
    if !current_num.is_empty() || total_secs == 0 {
        return None;
    }

    Some(std::time::Duration::from_secs(total_secs))
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles (function may show dead_code warning — that's fine, used in next task)

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: add parse_flight_duration helper"
```

---

### Task 3: Add `flight_command()` handler and wire up dispatch

**Files:**
- Modify: `src/main.rs:1721-1751` (handle_generic_commands)
- Modify: `src/main.rs` (new function)

- [ ] **Step 1: Add the `flight_command` function**

Add this function in `src/main.rs` near the other command handlers (after `list_pings_command` or `ai_command`):

```rust
#[instrument(skip(privmsg, client))]
async fn flight_command(
    privmsg: &PrivmsgMessage,
    client: &Arc<AuthenticatedTwitchClient>,
    aircraft_code: Option<&str>,
    duration_str: Option<&str>,
) -> Result<()> {
    // Validate arguments
    let (Some(aircraft_code), Some(duration_str)) = (aircraft_code, duration_str) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM")
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    };

    // Look up aircraft
    let Some(aircraft) = random_flight::aircraft_by_icao_type(aircraft_code) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Das Flugzeug kenn ich nich FDM")
            .await
        {
            error!(error = ?e, "Failed to send error message");
        }
        return Ok(());
    };

    // Parse duration
    let Some(duration) = parse_flight_duration(duration_str) else {
        if let Err(e) = client
            .say_in_reply_to(privmsg, "Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM")
            .await
        {
            error!(error = ?e, "Failed to send usage message");
        }
        return Ok(());
    };

    // Generate flight plan in blocking task (can take many retries)
    let result = tokio::task::spawn_blocking(move || {
        random_flight::generate_flight_plan(aircraft, duration, None)
    })
    .await
    .wrap_err("Flight plan generation task panicked")?;

    let fp = match result {
        Ok(fp) => fp,
        Err(e) => {
            warn!(error = ?e, "Flight plan generation failed");
            if let Err(e) = client
                .say_in_reply_to(
                    privmsg,
                    "Hab keine Route gefunden, versuch mal ne andere Zeit FDM",
                )
                .await
            {
                error!(error = ?e, "Failed to send error message");
            }
            return Ok(());
        }
    };

    // Format block time as compact string (e.g. "1h12m", "45m")
    let total_mins = fp.block_time.as_secs() / 60;
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    let time_str = if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    };

    let response = format!(
        "{} → {} | {:.0} nm | {} | FL{} | {}",
        fp.departure.icao,
        fp.arrival.icao,
        fp.distance_nm,
        time_str,
        fp.cruise_altitude_ft / 100,
        fp.simbrief_url(),
    );

    client.say_in_reply_to(privmsg, response).await?;

    Ok(())
}
```

- [ ] **Step 2: Wire up the dispatch in `handle_generic_commands`**

In `src/main.rs`, inside `handle_generic_commands()` (around line 1748, after the `!ai` branch), add:

```rust
    } else if first_word == "!fl" {
        flight_command(privmsg, client, words.next(), words.next()).await?;
    }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles successfully

- [ ] **Step 4: Verify clippy passes**

Run: `cargo clippy`
Expected: no errors (warnings about existing code are OK)

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: add !fl random flight command"
```

---

### Task 4: Manual smoke test

- [ ] **Step 1: Run the bot locally**

Run: `RUST_LOG=debug cargo run`

If you don't have a `config.toml`, skip this task — the command can be verified by code review and compilation.

- [ ] **Step 2: Test in chat**

Send `!fl A20N 1h` in the configured Twitch channel. Verify the bot replies with a message like:
```
EDDF → EGLL | 280 nm | 1h12m | FL360 | https://dispatch.simbrief.com/...
```

- [ ] **Step 3: Test error cases**

- `!fl` (no args) → "Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM"
- `!fl ZZZZ 1h` (bad aircraft) → "Das Flugzeug kenn ich nich FDM"
- `!fl A20N abc` (bad duration) → usage message
