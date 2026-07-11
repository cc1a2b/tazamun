#![no_main]
//! Fuzz the `tzm1…` invite-ticket parser.
//!
//! Adversary: a stranger pasting a hostile ticket. Bad prefix, bad base32,
//! truncated postcard, version mismatch, and absurd bootstrap vectors must all
//! be typed errors — never a panic.

use libfuzzer_sys::fuzz_target;
use tazamun::session::Ticket;

fuzz_target!(|data: &[u8]| {
    // The raw input as UTF-8 (lossy so every byte string reaches the parser).
    let s = String::from_utf8_lossy(data);
    let _ = Ticket::decode(&s);

    // Also drive the post-prefix base32 → postcard path directly by prepending
    // the real ticket prefix, so the decoder's inner stages get hostile bytes
    // even when the raw input did not start with `tzm1`.
    let mut with_prefix = String::from("tzm1");
    with_prefix.push_str(&s);
    let _ = Ticket::decode(&with_prefix);
});
