//! تزامُن (tazamun) — strict-exclusive-checkout P2P folder sync over iroh.
//!
//! Crate invariant (the Golden Invariant): never overwrite data a peer has not
//! seen, never silently delete user bytes. Every ambiguous situation resolves
//! to "preserve both copies, warn loudly".
#![forbid(unsafe_code)]

pub mod cli;
pub mod daemon;
pub mod dashboard;
pub mod doctor;
pub mod guard;
pub mod ipc;
pub mod locks;
pub mod net;
pub mod proto;
pub mod service;
pub mod session;
pub mod state;
pub mod sync;
pub mod ui;
pub mod versions;
pub mod watcher;
pub mod win_fs;

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

    // ── DoS / resource bounds (P6 security pass) ────────────────────────────
    // Every value here caps a resource an attacker on the wire could otherwise
    // grow without limit. Rationale for each lives in DECISIONS.md and
    // docs/THREAT_MODEL.md; the enforcement sites are named there.
    /// Max control connections allowed to be mid-handshake at once. A peer that
    /// knows the gossip topic but not the session secret can open QUIC
    /// connections yet never complete the proof; without a cap each attempt
    /// ties up a task and a stream for up to [`HANDSHAKE_DEADLINE`]. Beyond
    /// this the accept side closes immediately (fail-closed).
    pub const MAX_INFLIGHT_HANDSHAKES: usize = 64;
    /// Max simultaneously authenticated control peers. A session is a small
    /// trusted group; this bounds peer-table, per-peer task and channel growth
    /// even if an insider spins up many endpoint identities.
    pub const MAX_PEERS: usize = 128;
    /// Max file pulls executing at once. A malicious `Index` advertising
    /// thousands of paths would otherwise spawn one dial+fetch task each;
    /// excess paths wait in a backlog and start as running pulls complete.
    pub const MAX_CONCURRENT_PULLS: usize = 32;
    /// Max paths waiting behind the running-pull cap. Beyond this an advertised
    /// record is dropped (it stays in the peer index, so FRESHNESS still gates
    /// edits and the peer re-advertises it later); bounds backlog memory under
    /// a hostile `Index` flood.
    pub const MAX_PULL_BACKLOG: usize = 8192;
    /// Max distinct paths the lock-waitlist interest map tracks. A new path is
    /// ignored past this cap so a peer cannot grow the map without limit (the
    /// per-path waiter set is separately bounded by [`MAX_PEERS`]).
    pub const MAX_WAITLIST_ENTRIES: usize = 4096;
    /// Max leases the pure lock table tracks across all paths. Bounds memory
    /// against an `Index` advertising a flood of hostile `LeaseInfo` entries or
    /// a `LockReq` storm.
    pub const MAX_TRACKED_LEASES: usize = 4096;
    /// Max encoded size of a manifest blob the transfer layer will load into
    /// memory. [`MAX_CHUNKS_PER_FILE`] chunk refs at the largest postcard
    /// encoding (32-byte hash + 5-byte varint length ≈ 37 B) with headroom, so
    /// a hostile peer cannot force an unbounded `get_bytes` on a manifest blob.
    pub const MAX_MANIFEST_BYTES: u64 = MAX_CHUNKS_PER_FILE as u64 * 48;
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
    /// Default lease time-to-live (overridable per session via config).
    pub const LEASE_TTL: Duration = Duration::from_secs(90);
    /// Default lease renew interval. The effective renew is always `ttl / 3`
    /// (derived, never configured directly); this is the default's third.
    pub const LEASE_RENEW: Duration = Duration::from_secs(30);
    /// Default lock-acquire timeout (overridable per session via config).
    pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(8);
    /// Lower bound for a configured / wire-supplied lease TTL. A TTL below this
    /// (including a hostile `ttl_ms = 0`) is clamped up to it so a lease is
    /// never effectively instantaneous.
    pub const MIN_LEASE_TTL: Duration = Duration::from_secs(10);
    /// Upper bound for a configured / wire-supplied lease TTL, so a malicious or
    /// misconfigured peer cannot park an effectively-infinite lease.
    pub const MAX_LEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
    /// Lower bound for the configured lock-acquire timeout.
    pub const MIN_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
    /// Upper bound for the configured lock-acquire timeout.
    pub const MAX_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(60);
    /// Autolock: how long a lease acquired by auto-lock-on-first-write is held
    /// after the last write before it auto-releases. Each write resets it.
    pub const AUTOLOCK_IDLE_RELEASE: Duration = Duration::from_secs(60);
    /// Default lifetime of a lock-waitlist entry before it gives up and reports
    /// a timeout (overridable per session via `wait_timeout`).
    pub const WAIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
    /// Control-plane protocol minor version. The `CTL_ALPN` major stays `/1`;
    /// this documents append-only wire additions. Bumped to 2 in P4 for the
    /// `LockInterest` / `LockFreed` waitlist messages (appended after `Bye`, so
    /// every prior variant keeps its postcard discriminant). Bumped to 3 in P6
    /// for the `DenyReason::Unavailable` variant (appended after `TieLost`, same
    /// append-only rule). Within the v0.1 dev line all nodes share one build, so
    /// an older node never receives a newer variant; the const is the
    /// forward-compat marker.
    pub const PROTOCOL_MINOR: u16 = 3;
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

    // ── Web dashboard (P7) ──────────────────────────────────────────────────
    /// Default loopback port the daemon serves the web dashboard on
    /// (overridable via the `dashboard_port` session config key).
    pub const DASHBOARD_PORT: u16 = 8787;
    /// Length in bytes of the random dashboard session token (hex-encoded to
    /// twice this many characters). Guards every state-changing dashboard call.
    pub const DASHBOARD_TOKEN_BYTES: usize = 32;
    /// Hard cap on one inbound dashboard HTTP request (headers + body). The
    /// dashboard is a single-user localhost surface; this bounds a hostile or
    /// buggy client.
    pub const DASHBOARD_MAX_REQUEST: usize = 1024 * 1024;
}

/// Milliseconds since the Unix epoch, saturating at zero for pre-epoch clocks.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
