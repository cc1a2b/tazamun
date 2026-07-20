#![no_main]
//! Fuzz the P11 sync-scope engine: arbitrary `.tazamunignore` content,
//! arbitrary selective-sync subtree strings, and arbitrary paths.
//!
//! Adversary: a hostile session member editing the shared ignore file (it
//! syncs, so any peer can write it). Building the matcher from any byte soup
//! and taking verdicts on any sanitized path must never panic; malformed glob
//! lines are skipped exactly as git skips them. The verdict for the ignore
//! file itself must always be `Sync`, no matter what the rules say — the
//! shared contract cannot be used to hide itself.

use libfuzzer_sys::fuzz_target;
use tazamun::sync::ignore::{IGNORE_FILE, IgnoreSet};
use tazamun::sync::index::sanitize_rel_path;

fuzz_target!(|data: &[u8]| {
    // Split the input into rules / only / skip / a candidate path.
    let text = String::from_utf8_lossy(data);
    let mut parts = text.splitn(4, '\u{1f}');
    let rules = parts.next().unwrap_or("");
    let only = parts.next().unwrap_or("");
    let skip = parts.next().unwrap_or("");
    let candidate = parts.next().unwrap_or("");

    for junk in [false, true] {
        let set = IgnoreSet::build(rules, junk, only, skip, data.len() as u64);
        // Any sanitizable path gets a verdict without panicking, with and
        // without a size.
        if let Ok(rel) = sanitize_rel_path(candidate) {
            let _ = set.verdict(&rel, None);
            let _ = set.verdict(&rel, Some(u64::MAX));
        }
        // The shared contract is always carried.
        let ignore_rel = sanitize_rel_path(IGNORE_FILE).expect("constant path is valid");
        assert!(set.verdict(&ignore_rel, Some(u64::MAX)).is_sync());
    }
});
