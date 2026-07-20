#![no_main]
//! Fuzz the chunk-manifest parser and its sanity checks.
//!
//! Adversary: a malicious authenticated peer serving a hostile manifest blob.
//! Decoding must never panic or over-allocate (the chunk-count cap is enforced
//! post-decode; postcard/serde bound their own pre-allocation), and the size
//! fold must be overflow-safe — no wrap in release, no panic in debug — for any
//! chunk list, including a hostile petabyte `expected` size.

use libfuzzer_sys::fuzz_target;
use tazamun::sync::manifest;

fuzz_target!(|data: &[u8]| {
    // Blob manifest: postcard `Vec<ChunkRef>` + count cap.
    if let Ok(refs) = manifest::decode_blob(data) {
        // The checked fold must return a value or a typed overflow error —
        // never panic. Then exercise the equality check against both the true
        // total and a hostile size claim.
        let total = manifest::folded_size(&refs).unwrap_or(0);
        let _ = manifest::check(&refs, total);
        let _ = manifest::check(&refs, u64::MAX);
        let _ = manifest::check(&refs, 0);
    }
    // Also feed the raw bytes straight at the count guard + fold via a second
    // decode attempt with no cap short-circuit, so the fold sees whatever the
    // decoder accepts.
    if let Ok(refs) = postcard::from_bytes::<Vec<tazamun::proto::ChunkRef>>(data) {
        let _ = manifest::folded_size(&refs);
    }
});
