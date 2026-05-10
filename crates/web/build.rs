//! Stamp a content-fingerprint env var per embedded asset so templates can
//! cache-bust by appending `?v=<hash>` — needed because the asset router
//! serves `Cache-Control: immutable, max-age=1y`, which would otherwise pin
//! browsers to a stale CSS/JS bundle for a year after every redeploy.

use std::env;
use std::fs;
use std::path::Path;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    for name in ["app.css", "app.js", "htmx.min.js", "favicon.svg"] {
        let path = Path::new(&manifest).join("assets").join(name);
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let key = name.replace(['.', '-'], "_").to_uppercase();
        println!("cargo:rustc-env=ASSET_V_{key}={:016x}", fnv1a64(&bytes));
        println!("cargo:rerun-if-changed=assets/{name}");
    }
}
