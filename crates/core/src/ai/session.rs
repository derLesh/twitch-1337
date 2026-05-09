//! Session-id generation. The id is forwarded as the OpenAI `session_id`
//! field on outbound requests; OpenRouter's Langfuse broadcast maps it to
//! the trace Session ID, grouping every round of one agent turn under one
//! row in the Langfuse UI.

use rand::RngExt as _;

/// 16 hex chars from a 64-bit random value: collision-resistant within any
/// realistic Langfuse retention window, short enough to be readable.
pub fn new_session_id() -> String {
    let n: u64 = rand::rng().random();
    format!("{n:016x}")
}

#[cfg(test)]
mod tests {
    use super::new_session_id;

    #[test]
    fn ids_are_16_hex_chars_and_distinct() {
        let a = new_session_id();
        let b = new_session_id();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
