//! Client-side peer RTT telemetry for the native GUI's Peers view.
//!
//! The daemon reports each peer's *current* round-trip time once per poll but
//! keeps no history, and a sparkline needs a short trail. [`TelemetryStore`] is
//! that trail: a bounded per-peer ring of the last `CAP` samples, accumulated
//! across refresh ticks and pruned against the live peer set each tick. Samples
//! are normalized against each peer's *own* min/max so a quiet link and a busy
//! one both fill the same drawn height.
//!
//! The module is pure `std` (`HashMap` + `VecDeque`) — no egui, no I/O — so
//! every branch (ring cap, self-relative normalization, byte/rate formatting) is
//! exhaustively unit-testable on any host. Byte formatting mirrors the crate's
//! existing `human_bytes` convention: binary 1024 divisor, integer bytes, one
//! decimal once scaled.

use std::collections::{HashMap, VecDeque};

/// Maximum samples retained per peer. One poll per refresh tick, so this is the
/// sparkline's visible history depth.
const CAP: usize = 120;

/// Per-peer RTT rings accumulated across refresh ticks. Bounded: at most `CAP`
/// samples per peer, peers pruned against the live set each refresh.
#[derive(Default)]
pub struct TelemetryStore {
    rings: HashMap<String, VecDeque<u64>>,
}

impl TelemetryStore {
    /// Records a sample for `peer` (keyed by short id). `None` (no rtt this poll
    /// — peer offline or no path) records nothing and creates no entry.
    pub fn push(&mut self, peer: &str, rtt_ms: Option<u64>) {
        let Some(rtt) = rtt_ms else {
            return;
        };
        let ring = self.rings.entry(peer.to_string()).or_default();
        ring.push_back(rtt);
        while ring.len() > CAP {
            ring.pop_front();
        }
    }

    /// The peer's samples normalized `0..=1` against its own ring min/max (flat
    /// ring => `0.5` for every point), oldest first. Empty vec under 2 samples.
    pub fn series(&self, peer: &str) -> Vec<f32> {
        let Some(ring) = self.rings.get(peer) else {
            return Vec::new();
        };
        if ring.len() < 2 {
            return Vec::new();
        }
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        for &v in ring {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if lo == hi {
            return vec![0.5; ring.len()];
        }
        let span = (hi - lo) as f32;
        ring.iter().map(|&v| (v - lo) as f32 / span).collect()
    }

    /// The most recent sample, if any.
    pub fn last_ms(&self, peer: &str) -> Option<u64> {
        self.rings.get(peer).and_then(|ring| ring.back().copied())
    }

    /// Drops peers not present in `live` (call once per refresh tick).
    pub fn prune(&mut self, live: &[String]) {
        self.rings.retain(|key, _| live.contains(key));
    }
}

/// Scales `n` bytes onto binary units (1024 divisor): integer bytes below 1 KiB,
/// one decimal once scaled. The shared core of [`fmt_rate`] and [`fmt_total`].
fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < U.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{f:.1} {}", U[i])
    }
}

/// "1.2 MB/s"-style rate; `0` renders the em-dash placeholder "—".
pub fn fmt_rate(bps: u64) -> String {
    if bps == 0 {
        return "—".to_string();
    }
    format!("{}/s", human_bytes(bps))
}

/// Human total bytes ("48.3 MB"); `0` renders "0 B".
pub fn fmt_total(bytes: u64) -> String {
    human_bytes(bytes)
}

/// Signal arcs to light for a health grade string: "Good" => 3, "Fair" => 2,
/// "Poor" => 1, anything else (incl. "Offline") => 0. Case-sensitive exact.
pub fn grade_lit(grade: &str) -> u8 {
    match grade {
        "Good" => 3,
        "Fair" => 2,
        "Poor" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn ring_capped_at_cap() {
        let mut store = TelemetryStore::default();
        let pushed = CAP as u64 + 50;
        for v in 0..pushed {
            store.push("p", Some(v));
        }
        // Only the last CAP survive; the oldest are popped off the front.
        assert_eq!(store.series("p").len(), CAP);
        assert_eq!(store.last_ms("p"), Some(pushed - 1));
    }

    #[test]
    fn push_none_records_nothing() {
        let mut store = TelemetryStore::default();
        store.push("p", None);
        assert_eq!(store.last_ms("p"), None);
        assert!(store.series("p").is_empty());

        // A None between real samples is skipped, not counted.
        store.push("p", Some(10));
        store.push("p", None);
        store.push("p", Some(20));
        assert_eq!(store.last_ms("p"), Some(20));
        assert_eq!(store.series("p").len(), 2);
    }

    #[test]
    fn series_under_two_samples_is_empty() {
        let mut store = TelemetryStore::default();
        assert!(store.series("p").is_empty());
        store.push("p", Some(5));
        assert!(store.series("p").is_empty());
        store.push("p", Some(6));
        assert_eq!(store.series("p").len(), 2);
    }

    #[test]
    fn flat_series_is_all_half() {
        let mut store = TelemetryStore::default();
        for _ in 0..5 {
            store.push("p", Some(42));
        }
        let s = store.series("p");
        assert_eq!(s.len(), 5);
        assert!(s.iter().all(|&x| approx(x, 0.5)));
    }

    #[test]
    fn rising_series_spans_zero_to_one() {
        let mut store = TelemetryStore::default();
        for v in [10u64, 20, 30, 40, 50] {
            store.push("p", Some(v));
        }
        let s = store.series("p");
        assert_eq!(s.len(), 5);
        assert!(approx(s[0], 0.0));
        assert!(approx(s[4], 1.0));
        for w in s.windows(2) {
            assert!(w[1] > w[0]);
        }
    }

    #[test]
    fn series_normalizes_against_own_ring() {
        let mut store = TelemetryStore::default();
        for v in [0u64, 5, 10] {
            store.push("a", Some(v));
        }
        for v in [100u64, 500, 900] {
            store.push("b", Some(v));
        }
        let a = store.series("a");
        let b = store.series("b");
        // Each peer is scaled against its own min/max, independently.
        assert!(approx(a[0], 0.0) && approx(a[1], 0.5) && approx(a[2], 1.0));
        assert!(approx(b[0], 0.0) && approx(b[1], 0.5) && approx(b[2], 1.0));
    }

    #[test]
    fn last_ms_tracks_latest_and_missing() {
        let mut store = TelemetryStore::default();
        assert_eq!(store.last_ms("nope"), None);
        store.push("p", Some(7));
        store.push("p", Some(9));
        assert_eq!(store.last_ms("p"), Some(9));
    }

    #[test]
    fn prune_drops_dead_keeps_live() {
        let mut store = TelemetryStore::default();
        store.push("a", Some(1));
        store.push("b", Some(2));
        store.push("c", Some(3));

        store.prune(&["a".to_string(), "c".to_string()]);
        assert_eq!(store.last_ms("a"), Some(1));
        assert_eq!(store.last_ms("c"), Some(3));
        assert_eq!(store.last_ms("b"), None);

        // An empty live set clears everything.
        store.prune(&[]);
        assert_eq!(store.last_ms("a"), None);
        assert_eq!(store.last_ms("c"), None);
    }

    #[test]
    fn fmt_rate_zero_is_em_dash() {
        assert_eq!(fmt_rate(0), "—");
    }

    #[test]
    fn fmt_rate_scales_and_appends_per_second() {
        assert!(fmt_rate(1_300_000).starts_with("1.2 MB"));
        assert_eq!(fmt_rate(1_300_000), "1.2 MB/s");
        assert_eq!(fmt_rate(512), "512 B/s");
        assert_eq!(fmt_rate(2048), "2.0 KB/s");
    }

    #[test]
    fn fmt_total_zero_and_scaled() {
        assert_eq!(fmt_total(0), "0 B");
        assert_eq!(fmt_total(512), "512 B");
        assert_eq!(fmt_total(1536), "1.5 KB");
        assert_eq!(fmt_total(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(fmt_total(2 * 1024 * 1024 * 1024), "2.0 GB");
        assert_eq!(fmt_total(5 * 1024u64.pow(4)), "5.0 TB");
    }

    #[test]
    fn grade_lit_mapping() {
        assert_eq!(grade_lit("Good"), 3);
        assert_eq!(grade_lit("Fair"), 2);
        assert_eq!(grade_lit("Poor"), 1);
        assert_eq!(grade_lit("Offline"), 0);
        assert_eq!(grade_lit(""), 0);
        assert_eq!(grade_lit("good"), 0);
        assert_eq!(grade_lit("GOOD"), 0);
        assert_eq!(grade_lit("unknown"), 0);
    }
}
