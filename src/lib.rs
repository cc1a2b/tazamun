//! تزامُن (tazamun) — strict-exclusive-checkout P2P folder sync over iroh.
//!
//! Crate invariant (the Golden Invariant): never overwrite data a peer has not
//! seen, never silently delete user bytes. Every ambiguous situation resolves
//! to "preserve both copies, warn loudly".
#![forbid(unsafe_code)]

pub mod cli;
pub mod daemon;
pub mod doctor;
pub mod guard;
pub mod ipc;
pub mod locks;
pub mod net;
pub mod proto;
pub mod session;
pub mod state;
pub mod sync;
pub mod ui;
pub mod versions;
pub mod watcher;

/// Global tuning constants. Every magic number in the crate lives here.
pub mod consts {
    use std::time::Duration;

    /// ALPN for the authenticated control-plane protocol.
    pub const CTL_ALPN: &[u8] = b"tazamun/ctl/1";
    /// Hard cap for a single control-plane frame (length prefix value).
    pub const MAX_FRAME: usize = 4 * 1024 * 1024;
    /// Hard cap for a relative path, in bytes.
    pub const MAX_PATH_LEN: usize = 4096;
    /// Hard cap on the number of chunks a single file manifest may reference.
    pub const MAX_CHUNKS_PER_FILE: usize = 1_048_576;
    /// FastCDC minimum chunk size.
    pub const CDC_MIN: u32 = 16 * 1024;
    /// FastCDC average (target) chunk size.
    pub const CDC_AVG: u32 = 64 * 1024;
    /// FastCDC maximum chunk size.
    pub const CDC_MAX: u32 = 256 * 1024;
    /// Manifests with at most this many chunks are inlined into messages;
    /// larger ones spill into a manifest blob.
    pub const INLINE_MANIFEST_MAX: usize = 256;
    /// Maximum number of chunk fetches in flight during a pull.
    pub const FETCH_CONCURRENCY: usize = 16;
    /// Production lease time-to-live.
    pub const LEASE_TTL: Duration = Duration::from_secs(90);
    /// Production lease renew interval.
    pub const LEASE_RENEW: Duration = Duration::from_secs(30);
    /// Production lock-acquire timeout.
    pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(8);
    /// Whole-handshake deadline for the control-plane proof exchange.
    pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
    /// Interval between encrypted presence beacons on the gossip topic.
    pub const PRESENCE_INTERVAL: Duration = Duration::from_secs(10);
    /// A member is considered online if seen within this window.
    pub const ONLINE_WINDOW: Duration = Duration::from_secs(30);
    /// Filesystem watcher debounce window.
    pub const DEBOUNCE: Duration = Duration::from_millis(250);
    /// Watch events for a path are suppressed for this long after tazamun
    /// itself writes to it.
    pub const MUTE_WINDOW: Duration = Duration::from_secs(2);
    /// Number of historical versions kept per path.
    pub const HISTORY_KEEP: usize = 5;
    /// Initial redial backoff after a failed dial to a known member.
    pub const REDIAL_BACKOFF_MIN: Duration = Duration::from_secs(1);
    /// Redial backoff cap.
    pub const REDIAL_BACKOFF_MAX: Duration = Duration::from_secs(60);
    /// Hard cap for one JSON line on the local IPC socket.
    pub const IPC_LINE_MAX: usize = 1024 * 1024;
    /// Interval of the blob store's scheduled garbage collection.
    pub const GC_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
    /// Interval between connection telemetry samples.
    pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(2);
    /// Interval between re-hole-punch probes for peers stuck on a relay.
    pub const REPUNCH_INTERVAL: Duration = Duration::from_secs(60);
    /// Health grading: `Good` requires a Direct path with RTT below this.
    pub const GRADE_GOOD_MAX_RTT_MS: f64 = 80.0;
    /// Health grading: `Good` requires RTT jitter (EWMA of |Δrtt|) below this.
    pub const GRADE_GOOD_MAX_JITTER_MS: f64 = 20.0;
    /// Health grading: RTT at or above this is `Poor` regardless of path.
    pub const GRADE_POOR_MIN_RTT_MS: f64 = 300.0;
    /// Health grading: strictly more path changes than this per minute is
    /// flapping, i.e. `Poor`.
    pub const GRADE_POOR_FLAPS_PER_MIN: usize = 3;
    /// Smoothing factor for jitter and transfer-rate EWMAs.
    pub const EWMA_ALPHA: f64 = 0.3;
    /// Reconnect/path events kept for the status panel.
    pub const EVENT_RING: usize = 5;
    /// Name of the metadata directory inside a session folder.
    pub const META_DIR: &str = ".tazamun";
}

/// Milliseconds since the Unix epoch, saturating at zero for pre-epoch clocks.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
