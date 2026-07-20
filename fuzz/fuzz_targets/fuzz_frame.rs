#![no_main]
//! Fuzz the length-prefixed control-frame decoder.
//!
//! Adversary: anything on the wire. The decoder must, over *arbitrary* bytes,
//! never panic, never allocate unboundedly (it enforces `MAX_FRAME` before the
//! body read), and never loop forever. Truncated frames, oversized length
//! prefixes, zero-length, and a length/body mismatch are all just typed errors.

use libfuzzer_sys::fuzz_target;
use tazamun::proto;

fuzz_target!(|data: &[u8]| {
    // Interpret the input as a whole frame: `u32` big-endian length + body.
    // A returned `Msg` is a valid decode; any error is a graceful rejection.
    let _ = proto::decode_frame(data);
});
