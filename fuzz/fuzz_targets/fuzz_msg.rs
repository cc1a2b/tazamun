#![no_main]
//! Fuzz the full `Msg` control-plane deserializer.
//!
//! Adversary: a malicious authenticated peer (it completed the handshake, so it
//! can send any bytes on the control stream). Every variant, hostile field
//! values, path fields feeding `sanitize_rel_path`, and hostile version-vector
//! maps must decode without panicking, and every carried path must survive the
//! sanitizer call the daemon makes at the wire boundary.

use libfuzzer_sys::fuzz_target;
use tazamun::proto::Msg;
use tazamun::sync::index::sanitize_rel_path;

fuzz_target!(|data: &[u8]| {
    let Ok(msg) = postcard::from_bytes::<Msg>(data) else {
        return;
    };
    // Every path a decoded message carries goes through the sanitizer, exactly
    // as `daemon::on_ctl` does — it must reject or accept, never panic.
    for path in msg.wire_paths() {
        let _ = sanitize_rel_path(path);
    }
    // A decoded message must re-encode and round-trip (postcard is the wire
    // format; a decode/encode asymmetry would be a wire-integrity bug).
    if let Ok(bytes) = postcard::to_stdvec(&msg) {
        let round = postcard::from_bytes::<Msg>(&bytes);
        assert!(round.is_ok(), "decoded Msg failed to round-trip");
    }
});
