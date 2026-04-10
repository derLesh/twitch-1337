# AI Conversation Context Design

**Date:** 2026-04-10
**Issue:** #12

## Summary

Add a per-channel message history buffer to the `!ai` command so the LLM can reference recent chat context when responding. The buffer captures all chat messages in the main channel (not just `!ai` exchanges), giving the AI awareness of the ongoing conversation.

## Configuration

A new `history_length` field in the `[ai]` section of `config.toml`:

```toml
[ai]
# ... existing fields ...
history_length = 10  # number of individual messages to keep (0 = disabled, max 100)
```

- Type: `u64`, default `0` (preserves current stateless behavior)
- Capped at `100`, validated in `Configuration::validate()`
- Counts individual messages (user chat messages and bot responses), not pairs

## Data Structure & Ownership

A `VecDeque<(String, String)>` (username, message text) wrapped in `Arc<Mutex<VecDeque<...>>>` (using `tokio::sync::Mutex`), created in `main()`.

- Created in `run_generic_command_handler` when `history_length > 0`
- Passed to both:
  - `run_command_dispatcher` — to record all messages
  - `AiCommand` — to read history when building requests
- When `history_length` is `0`, `None` is passed to both — zero overhead
- `VecDeque` capacity set to `history_length`
- When the buffer is full, oldest messages are popped from the front before pushing new ones

This follows the existing pattern of shared state constructed in `main()` and passed to handlers (like `ping_manager`, `schedule_cache`). It also leaves the door open for seeding the buffer at startup in the future.

## Recording Logic

Recording happens in `run_command_dispatcher`, which already receives every PRIVMSG:

- Records **after** the admin channel gate but **before** command matching — every main-channel message is captured, whether it triggers a command or not
- The `!ai` message itself is recorded (it's part of the chat flow)
- Bot responses: `AiCommand` pushes `(bot_username, response_text)` into the buffer after a successful reply
- Error/timeout responses are **not** recorded
- Cooldown-blocked invocations: the `!ai` message is already recorded by the dispatcher, but no bot response is added
- Admin channel messages are **never** recorded

## Request Construction

When `!ai` is invoked, the history is formatted and injected into the single user message sent to the LLM. The message array remains two entries: system prompt + user message.

Format of the user message when history is present:

```
[Chat History]
user_a: hello everyone
user_b: hey!
user_a: !ai what were we talking about?

[Current Message]
user_a: what were we talking about?
```

The current user's `!ai` message appears both in the history (as part of the chat log) and as the explicit current message, so the LLM knows which message to respond to.

When history is empty (no messages recorded yet), the `[Chat History]` section is omitted entirely — the request looks identical to today's behavior.

## Thread Safety & Performance

- Uses `tokio::sync::Mutex` since the buffer is accessed across `.await` points
- Recording a message: lock, push back, possibly pop front — fast, no async work under the lock
- Reading history for `!ai`: lock, clone the contents, release — the clone happens under the lock but the buffer is small (max 100 string tuples)
- No contention concerns: recording is sequential (one dispatcher loop) and reads are infrequent (`!ai` invocations)

## Error Handling & Edge Cases

- **Empty history**: `[Chat History]` section omitted, request identical to current behavior
- **Bot restart**: History is lost (in-memory only). Future work can seed on startup
- **`history_length = 0`**: No buffer allocated, no recording, no change to request format — fully backwards compatible
- **Long messages**: Individual chat messages are not truncated in the buffer. The `history_length` cap keeps total size bounded

## Files Changed

1. **`src/main.rs`**
   - Add `history_length` field to `AiConfig` (default `0`, validated max `100`)
   - Create the `VecDeque` buffer in `run_generic_command_handler` when `history_length > 0`
   - Pass buffer to `run_command_dispatcher` and to `AiCommand::new()`
   - Record every main-channel PRIVMSG into the buffer in `run_command_dispatcher`

2. **`src/commands/ai.rs`**
   - Add `history: Option<Arc<Mutex<VecDeque<(String, String)>>>>` and `bot_username: String` fields
   - Update `new()` constructor to accept these
   - In `execute()`: read and format history into the user message
   - After successful response: push `(bot_username, response)` into the buffer

3. **`config.toml.example`**
   - Add `history_length` with a comment

4. **`CLAUDE.md`**
   - Document `history_length` in the config reference
