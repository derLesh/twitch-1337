//! Hex-encoded double-submit CSRF token helpers.
//!
//! Verification uses constant-time comparison so an attacker cannot probe
//! per-byte differences via timing.

pub fn encode(value: &[u8; 32]) -> String {
    hex::encode(value)
}

pub fn decode(encoded: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(encoded).ok()?;
    bytes.try_into().ok()
}

pub fn verify(encoded: &str, expected: &[u8; 32]) -> bool {
    decode(encoded)
        .map(|got| constant_time_eq(&got, expected))
        .unwrap_or(false)
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
