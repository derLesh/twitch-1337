//! Process-wide rustls configuration.

/// Install the `ring` rustls [`CryptoProvider`] as the process-wide default.
///
/// Our dependency tree enables both `ring` (via this crate) and `aws-lc-rs`
/// (transitively, through rustls' default features on other deps), so rustls
/// 0.23 refuses to auto-pick and panics on the first TLS handshake. Pick one
/// explicitly. Must run before any TLS client is built. Idempotent.
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
