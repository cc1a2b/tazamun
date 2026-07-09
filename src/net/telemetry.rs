//! Per-peer connection telemetry and health grading.
//!
//! Invariant: this module is pure bookkeeping — samples go in, a grade comes
//! out; no I/O, no clocks of its own (`now` is injected everywhere), and no
//! network types beyond plain data. The daemon actor owns every instance and
//! feeds it from iroh's connection/path APIs; everything here is exhaustively
//! unit-testable with synthetic samples.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::consts::{
    EWMA_ALPHA, GRADE_GOOD_MAX_JITTER_MS, GRADE_GOOD_MAX_RTT_MS, GRADE_POOR_FLAPS_PER_MIN,
    GRADE_POOR_MIN_RTT_MS, ONLINE_WINDOW,
};

/// How a peer is currently reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConnState {
    Direct,
    Relayed,
    None,
}

impl std::fmt::Display for ConnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnState::Direct => f.write_str("Direct"),
            ConnState::Relayed => f.write_str("Relayed"),
            ConnState::None => f.write_str("None"),
        }
    }
}

/// Derived link quality, worst-to-best ordering not implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HealthGrade {
    Good,
    Fair,
    Poor,
    Offline,
}

impl std::fmt::Display for HealthGrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthGrade::Good => f.write_str("Good"),
            HealthGrade::Fair => f.write_str("Fair"),
            HealthGrade::Poor => f.write_str("Poor"),
            HealthGrade::Offline => f.write_str("Offline"),
        }
    }
}

/// One reading taken from a live connection.
#[derive(Debug, Clone)]
pub struct PathSample {
    pub conn: ConnState,
    pub rtt_ms: f64,
    pub relay_url: Option<String>,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
}

/// Rolling health state for one peer.
#[derive(Debug, Clone)]
pub struct PeerHealth {
    pub conn: ConnState,
    pub rtt_ms: f64,
    /// EWMA of |Δrtt| between consecutive samples.
    pub rtt_jitter_ms: f64,
    /// Total path changes since this connection came up.
    pub path_changes: u32,
    /// Timestamps of recent path changes (pruned to the last minute).
    recent_changes: Vec<Instant>,
    /// Last proof of life from any source (control traffic or presence).
    pub last_seen: Instant,
    pub relay_url: Option<String>,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    /// EWMA transfer rates in bytes/second.
    pub rate_tx: f64,
    pub rate_rx: f64,
    /// When the current control connection was established.
    pub connected_at: Option<Instant>,
    /// How long the connection took to reach its first Direct path.
    pub time_to_direct: Option<Duration>,
    prev_rtt: Option<f64>,
    prev_sample_at: Option<Instant>,
}

impl PeerHealth {
    /// A peer known only through presence (no control connection yet).
    pub fn seen_only(now: Instant) -> Self {
        Self {
            conn: ConnState::None,
            rtt_ms: 0.0,
            rtt_jitter_ms: 0.0,
            path_changes: 0,
            recent_changes: Vec::new(),
            last_seen: now,
            relay_url: None,
            bytes_tx: 0,
            bytes_rx: 0,
            rate_tx: 0.0,
            rate_rx: 0.0,
            connected_at: None,
            time_to_direct: None,
            prev_rtt: None,
            prev_sample_at: None,
        }
    }

    /// Marks a fresh control connection (counters restart with it).
    pub fn on_connect(&mut self, now: Instant) {
        self.connected_at = Some(now);
        self.time_to_direct = None;
        self.path_changes = 0;
        self.recent_changes.clear();
        self.bytes_tx = 0;
        self.bytes_rx = 0;
        self.rate_tx = 0.0;
        self.rate_rx = 0.0;
        self.prev_rtt = None;
        self.prev_sample_at = None;
        self.last_seen = now;
    }

    /// Ingests one telemetry sample from the live connection.
    pub fn on_sample(&mut self, s: &PathSample, now: Instant) {
        if let Some(prev) = self.prev_rtt {
            let delta = (s.rtt_ms - prev).abs();
            self.rtt_jitter_ms = EWMA_ALPHA * delta + (1.0 - EWMA_ALPHA) * self.rtt_jitter_ms;
        }
        if let Some(prev_at) = self.prev_sample_at {
            let secs = now.duration_since(prev_at).as_secs_f64();
            if secs > f64::EPSILON {
                let inst_tx = s.bytes_tx.saturating_sub(self.bytes_tx) as f64 / secs;
                let inst_rx = s.bytes_rx.saturating_sub(self.bytes_rx) as f64 / secs;
                self.rate_tx = EWMA_ALPHA * inst_tx + (1.0 - EWMA_ALPHA) * self.rate_tx;
                self.rate_rx = EWMA_ALPHA * inst_rx + (1.0 - EWMA_ALPHA) * self.rate_rx;
            }
        }
        if s.conn == ConnState::Direct
            && self.time_to_direct.is_none()
            && let Some(at) = self.connected_at
        {
            self.time_to_direct = Some(now.duration_since(at));
        }
        self.conn = s.conn;
        self.rtt_ms = s.rtt_ms;
        self.relay_url = s.relay_url.clone();
        self.bytes_tx = s.bytes_tx;
        self.bytes_rx = s.bytes_rx;
        self.prev_rtt = Some(s.rtt_ms);
        self.prev_sample_at = Some(now);
        self.last_seen = now;
    }

    /// Records a path change event (open/close/selected-path switch).
    pub fn on_path_change(&mut self, now: Instant) {
        self.path_changes = self.path_changes.saturating_add(1);
        self.recent_changes.push(now);
        self.prune_changes(now);
    }

    /// The control connection dropped.
    pub fn on_disconnect(&mut self, now: Instant) {
        self.conn = ConnState::None;
        self.relay_url = None;
        self.connected_at = None;
        self.rate_tx = 0.0;
        self.rate_rx = 0.0;
        self.prev_rtt = None;
        self.prev_sample_at = None;
        self.last_seen = now;
    }

    /// A presence beacon arrived (no connection implied).
    pub fn on_presence(&mut self, now: Instant) {
        self.last_seen = now;
    }

    fn prune_changes(&mut self, now: Instant) {
        self.recent_changes
            .retain(|t| now.duration_since(*t) <= Duration::from_secs(60));
    }

    /// Path changes within the trailing minute.
    pub fn flaps_last_minute(&self, now: Instant) -> usize {
        self.recent_changes
            .iter()
            .filter(|t| now.duration_since(**t) <= Duration::from_secs(60))
            .count()
    }

    /// Pure grading over the stored fields.
    ///
    /// - `Offline`: no connection and nothing heard within [`ONLINE_WINDOW`].
    /// - `Poor`: reachable but degraded — no live connection (presence gap),
    ///   flapping paths, or RTT ≥ the poor threshold.
    /// - `Good`: Direct with RTT and jitter under the good thresholds.
    /// - `Fair`: everything in between (e.g. stable Relayed, or Direct with
    ///   elevated RTT/jitter).
    pub fn grade(&self, now: Instant) -> HealthGrade {
        if self.conn == ConnState::None {
            return if now.duration_since(self.last_seen) > ONLINE_WINDOW {
                HealthGrade::Offline
            } else {
                HealthGrade::Poor
            };
        }
        if self.flaps_last_minute(now) > GRADE_POOR_FLAPS_PER_MIN
            || self.rtt_ms >= GRADE_POOR_MIN_RTT_MS
        {
            return HealthGrade::Poor;
        }
        if self.conn == ConnState::Direct
            && self.rtt_ms < GRADE_GOOD_MAX_RTT_MS
            && self.rtt_jitter_ms < GRADE_GOOD_MAX_JITTER_MS
        {
            return HealthGrade::Good;
        }
        HealthGrade::Fair
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(conn: ConnState, rtt_ms: f64) -> PathSample {
        PathSample {
            conn,
            rtt_ms,
            relay_url: match conn {
                ConnState::Relayed => Some("https://relay.example.com./".into()),
                _ => None,
            },
            bytes_tx: 0,
            bytes_rx: 0,
        }
    }

    fn connected(conn: ConnState, rtt_ms: f64, now: Instant) -> PeerHealth {
        let mut h = PeerHealth::seen_only(now);
        h.on_connect(now);
        h.on_sample(&sample(conn, rtt_ms), now);
        h
    }

    #[test]
    fn grade_matrix_all_four() {
        let now = Instant::now();
        // Good: Direct, fast, stable.
        assert_eq!(
            connected(ConnState::Direct, 12.0, now).grade(now),
            HealthGrade::Good
        );
        // Fair: stable relay.
        assert_eq!(
            connected(ConnState::Relayed, 60.0, now).grade(now),
            HealthGrade::Fair
        );
        // Fair: direct but slow (between good and poor).
        assert_eq!(
            connected(ConnState::Direct, 150.0, now).grade(now),
            HealthGrade::Fair
        );
        // Poor: very high rtt, even direct.
        assert_eq!(
            connected(ConnState::Direct, 450.0, now).grade(now),
            HealthGrade::Poor
        );
        // Poor: presence gap — recently seen but no connection.
        let gone = {
            let mut h = connected(ConnState::Direct, 10.0, now);
            h.on_disconnect(now);
            h
        };
        assert_eq!(gone.grade(now), HealthGrade::Poor);
        // Offline: no connection and silence past the online window.
        assert_eq!(
            gone.grade(now + ONLINE_WINDOW + Duration::from_secs(1)),
            HealthGrade::Offline
        );
    }

    #[test]
    fn grade_thresholds_are_exact_boundaries() {
        let now = Instant::now();
        // RTT strictly below 80 with low jitter → Good; exactly 80 → Fair.
        assert_eq!(
            connected(ConnState::Direct, GRADE_GOOD_MAX_RTT_MS - 0.1, now).grade(now),
            HealthGrade::Good
        );
        assert_eq!(
            connected(ConnState::Direct, GRADE_GOOD_MAX_RTT_MS, now).grade(now),
            HealthGrade::Fair
        );
        // Exactly at the poor threshold → Poor; just under → Fair.
        assert_eq!(
            connected(ConnState::Direct, GRADE_POOR_MIN_RTT_MS, now).grade(now),
            HealthGrade::Poor
        );
        assert_eq!(
            connected(ConnState::Direct, GRADE_POOR_MIN_RTT_MS - 0.1, now).grade(now),
            HealthGrade::Fair
        );
        // Jitter boundary: high jitter demotes an otherwise Good link.
        let mut h = connected(ConnState::Direct, 10.0, now);
        for i in 0..50 {
            let rtt = if i % 2 == 0 { 10.0 } else { 150.0 };
            h.on_sample(
                &sample(ConnState::Direct, rtt),
                now + Duration::from_secs(i),
            );
        }
        assert!(h.rtt_jitter_ms >= GRADE_GOOD_MAX_JITTER_MS);
        let final_now = now + Duration::from_secs(50);
        assert_ne!(h.grade(final_now), HealthGrade::Good);
    }

    #[test]
    fn flapping_paths_grade_poor_and_decay() {
        let now = Instant::now();
        let mut h = connected(ConnState::Direct, 10.0, now);
        for i in 0..4 {
            h.on_path_change(now + Duration::from_secs(i));
        }
        let t = now + Duration::from_secs(10);
        assert_eq!(h.flaps_last_minute(t), 4);
        assert_eq!(h.grade(t), HealthGrade::Poor);
        // A minute later the flaps age out and the grade recovers.
        let later = now + Duration::from_secs(90);
        h.on_sample(&sample(ConnState::Direct, 10.0), later);
        assert_eq!(h.grade(later), HealthGrade::Good);
        assert_eq!(h.path_changes, 4, "cumulative count never decays");
    }

    #[test]
    fn ewma_jitter_and_rates() {
        let now = Instant::now();
        let mut h = PeerHealth::seen_only(now);
        h.on_connect(now);
        // First sample sets baselines; no jitter yet.
        h.on_sample(&sample(ConnState::Direct, 100.0), now);
        assert_eq!(h.rtt_jitter_ms, 0.0);
        // Second sample: |Δ| = 50 → jitter = α·50 = 15.
        h.on_sample(
            &sample(ConnState::Direct, 150.0),
            now + Duration::from_secs(2),
        );
        assert!((h.rtt_jitter_ms - EWMA_ALPHA * 50.0).abs() < 1e-9);
        // Third: |Δ| = 50 again → 0.3·50 + 0.7·15 = 25.5.
        h.on_sample(
            &sample(ConnState::Direct, 100.0),
            now + Duration::from_secs(4),
        );
        assert!((h.rtt_jitter_ms - (0.3 * 50.0 + 0.7 * 15.0)).abs() < 1e-9);

        // Rates: 2 MB over 2s → 1 MB/s instant → EWMA α·1e6.
        let mut r = PeerHealth::seen_only(now);
        r.on_connect(now);
        r.on_sample(&sample(ConnState::Direct, 10.0), now);
        let mut s2 = sample(ConnState::Direct, 10.0);
        s2.bytes_rx = 2_000_000;
        r.on_sample(&s2, now + Duration::from_secs(2));
        assert!((r.rate_rx - EWMA_ALPHA * 1_000_000.0).abs() < 1.0);
        assert_eq!(r.bytes_rx, 2_000_000);
    }

    #[test]
    fn time_to_direct_records_first_upgrade_only() {
        let now = Instant::now();
        let mut h = PeerHealth::seen_only(now);
        h.on_connect(now);
        h.on_sample(
            &sample(ConnState::Relayed, 50.0),
            now + Duration::from_secs(1),
        );
        assert!(h.time_to_direct.is_none());
        h.on_sample(
            &sample(ConnState::Direct, 20.0),
            now + Duration::from_secs(3),
        );
        assert_eq!(h.time_to_direct, Some(Duration::from_secs(3)));
        // Later flips do not overwrite the first measurement.
        h.on_sample(
            &sample(ConnState::Relayed, 50.0),
            now + Duration::from_secs(5),
        );
        h.on_sample(
            &sample(ConnState::Direct, 20.0),
            now + Duration::from_secs(9),
        );
        assert_eq!(h.time_to_direct, Some(Duration::from_secs(3)));
    }
}
