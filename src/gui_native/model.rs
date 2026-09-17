//! The typed data the worker publishes to the view, and the commands the view
//! sends back.
//!
//! Split out of `gui_native.rs` so the worker and the views can be worked on
//! without editing the same file: this module is the contract between them and
//! changes only when that contract does. Pure data — no I/O, no painting.

use std::collections::BTreeMap;
use std::path::PathBuf;

// ─── typed data model (worker → UI snapshot) ─────────────────────────────────

#[derive(Clone, Default)]
pub(super) struct Overview {
    pub(super) version: String,
    pub(super) supervisor: bool,
    pub(super) sessions: Vec<SessionRow>,
}

#[derive(Clone)]
pub(super) struct SessionRow {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) running: bool,
    pub(super) paused: bool,
    pub(super) hosted_by_gui: bool,
    pub(super) readable: bool,
    pub(super) role: String,
    pub(super) strict: bool,
    pub(super) files: usize,
    pub(super) total_bytes: u64,
    pub(super) conflicts: usize,
    pub(super) peers_online: usize,
    pub(super) peers_total: usize,
    pub(super) id_short: String,
}

#[derive(Clone, Default)]
pub(super) struct Detail {
    pub(super) dir: String,
    pub(super) running: bool,
    pub(super) role: String,
    pub(super) strict: bool,
    pub(super) invite: Option<String>,
    pub(super) members: Vec<Member>,
    pub(super) files: Vec<FileRow>,
    pub(super) files_total: usize,
    pub(super) files_truncated: bool,
    pub(super) conflicts: Vec<ConflictRow>,
    pub(super) leases: Vec<LeaseRow>,
    pub(super) audit: Vec<AuditRow>,
    pub(super) config: Option<ConfigView>,
    pub(super) versions: BTreeMap<String, Vec<VersionRow>>,
    pub(super) pulls: Vec<PullRow>,
    pub(super) backlog: usize,
    pub(super) resuming: usize,
    pub(super) download_limit_bps: u64,
    pub(super) events: Vec<EventRow>,
    pub(super) error: Option<String>,
}

/// The daemon's config summary (from the DashboardState payload).
#[derive(Clone, Default)]
pub(super) struct ConfigView {
    pub(super) autolock: bool,
    pub(super) audit: bool,
    pub(super) hooks: bool,
    pub(super) notify: bool,
    pub(super) strict: bool,
    pub(super) role: String,
    pub(super) update_channel: String,
    pub(super) lease_ttl_ms: u64,
    pub(super) acquire_timeout_ms: u64,
    pub(super) wait_timeout_ms: u64,
    pub(super) dashboard_port: u16,
    pub(super) relay: Option<String>,
    pub(super) lan: bool,
    pub(super) max_down: u64,
}

#[derive(Clone)]
pub(super) struct VersionRow {
    pub(super) n: u64,
    pub(super) ts_ms: u64,
    pub(super) size: u64,
    pub(super) tag: Option<String>,
    pub(super) pinned: bool,
}

#[derive(Clone)]
pub(super) struct PullRow {
    pub(super) path: String,
    pub(super) percent: u64,
    pub(super) bytes_done: u64,
    pub(super) bytes_total: u64,
    pub(super) rate: u64,
}

#[derive(Clone)]
pub(super) struct EventRow {
    pub(super) text: String,
}

#[derive(Clone)]
pub(super) struct Member {
    pub(super) id_short: String,
    pub(super) name: Option<String>,
    pub(super) online: bool,
    pub(super) grade: String,
    pub(super) conn: String,
    pub(super) rtt_ms: Option<u64>,
    pub(super) via_lan: bool,
    pub(super) jitter_ms: f64,
    pub(super) rate_tx: u64,
    pub(super) rate_rx: u64,
    pub(super) bytes_tx: u64,
    pub(super) bytes_rx: u64,
    pub(super) relay_url: Option<String>,
    pub(super) ttd_ms: Option<u64>,
    pub(super) flaps: u64,
}

#[derive(Clone)]
pub(super) struct FileRow {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) locked_by: Option<String>,
    pub(super) mine_lock: bool,
}

#[derive(Clone)]
pub(super) struct ConflictRow {
    pub(super) name: String,
    pub(super) path: String,
    pub(super) reason: String,
    pub(super) ts_ms: u64,
    pub(super) size: u64,
}

#[derive(Clone)]
pub(super) struct LeaseRow {
    pub(super) path: String,
    pub(super) holder: String,
    pub(super) mine: bool,
    pub(super) expires_in_ms: u64,
}

#[derive(Clone)]
pub(super) struct AuditRow {
    pub(super) ts_ms: u64,
    pub(super) kind: String,
    pub(super) path: Option<String>,
    pub(super) peer: Option<String>,
    pub(super) detail: Option<String>,
}

/// A refused edit, kept in full rather than flattened into a toast.
///
/// The daemon diagnoses every refusal — which of the three lease preconditions
/// failed, what would clear it, who holds the lease, which peers it asked — and
/// the CLI prints all of it. The window used to keep the one-line message and
/// drop the rest, so the words REACHABILITY, FRESHNESS and LEASE appeared
/// nowhere in the interface and the user was told "lock refused" with no way to
/// learn why.
#[derive(Clone)]
pub(super) struct Refusal {
    pub(super) path: String,
    pub(super) precondition: String,
    pub(super) message: String,
    pub(super) hint: String,
    pub(super) held_by: Option<String>,
    pub(super) peers: Vec<String>,
}

/// The worker → UI snapshot, plus any toasts the UI has not drained yet.
#[derive(Default)]
pub(super) struct Shared {
    pub(super) overview: Option<Overview>,
    pub(super) detail: Option<Detail>,
    /// A queue, not a slot: a bulk action produces several messages between
    /// two UI frames, and a slot would keep only the last of them.
    pub(super) toasts: Vec<Toast>,
    pub(super) picked: Option<(PickTarget, String)>,
    /// Bumped once per completed refresh so the UI can sample telemetry
    /// per poll, not per frame.
    pub(super) tick: u64,
    pub(super) busy: bool,
    /// Commands accepted and not yet settled.
    pub(super) inflight: Vec<InFlight>,
    /// Per-session reachability of the poll itself, keyed by directory.
    pub(super) reach: BTreeMap<String, Reach>,
    /// When each session's snapshot was last refreshed successfully, so the
    /// view can say how old what it is showing actually is.
    pub(super) fetched_at: BTreeMap<String, f64>,
    /// A long answer waiting to be read.
    pub(super) report: Option<Report>,
    /// The most recent answer to a file query.
    pub(super) file_page: Option<FilePage>,
    /// The most recent refused edit. Persists until the user clears it or the
    /// next edit succeeds — a refusal the user must act on cannot live in a
    /// four-second toast.
    pub(super) refusal: Option<Refusal>,
}

#[derive(Clone)]
pub(super) struct Toast {
    pub(super) text: String,
    pub(super) error: bool,
}

/// Which text field a native folder-picker result lands in.
#[derive(Clone, Copy)]
pub(super) enum PickTarget {
    Init,
    Join,
}

/// A command the worker has accepted and not yet finished.
///
/// The window used to fire every action into a channel and show nothing until a
/// toast arrived, so a click on Lock looked identical to a click on nothing.
/// Each accepted command now takes a ticket here, the view draws it, and the
/// worker clears it when the command settles.
#[derive(Clone)]
pub(super) struct InFlight {
    /// Monotonic, so the view can key animations and the user can cancel one.
    pub(super) id: u64,
    /// Session this belongs to, for filtering to the open one.
    pub(super) dir: String,
    /// The verb, in the house voice ("locking", "publishing", "restoring").
    pub(super) what: &'static str,
    /// What it is acting on — a path, a peer, a session name.
    pub(super) subject: String,
    /// `(step, of)` for the guided sequences, which are several round trips and
    /// used to report only their final failure.
    pub(super) step: Option<(u8, u8)>,
    /// When it was accepted, so the view can age it into "this is taking a while".
    pub(super) started: f64,
    /// Set when the user asks to abandon it; the worker stops at its next step
    /// boundary rather than mid-write.
    pub(super) cancelling: bool,
}

/// One page of a server-side file query.
///
/// The snapshot's file list is capped by the daemon, and a client-side filter
/// over a capped list can only ever find what is already in it — so in a large
/// session every path past the cap was unreachable from the window. This is the
/// answer to a query run against the whole index.
#[derive(Clone)]
pub(super) struct FilePage {
    /// The session it belongs to, so a stale answer for another folder is
    /// discarded rather than rendered.
    pub(super) dir: String,
    /// The pattern it answers, for the same reason.
    pub(super) pattern: String,
    /// The rows of this page.
    pub(super) rows: Vec<FileRow>,
    /// How many files matched in total, which is what the view reports.
    pub(super) matched: usize,
    /// Where this page starts.
    pub(super) offset: usize,
    /// Where the next page starts, if there is one.
    pub(super) next_offset: Option<usize>,
}

/// A body of text the worker produced that is too long, and too worth reading,
/// to be a toast — a diff, a doctor report, a dashboard address.
#[derive(Clone)]
pub(super) struct Report {
    pub(super) title: String,
    pub(super) body: String,
    /// A URL the view should offer to open, if the report names one.
    pub(super) link: Option<String>,
    /// True when the report is a failure rather than an answer.
    pub(super) failed: bool,
}

/// How a session's own poll is faring. One unreachable daemon used to stall the
/// single worker loop for the full 30-second IPC timeout, freezing every other
/// session and every user command behind it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Reach {
    /// Answering normally.
    #[default]
    Live,
    /// A poll is outstanding and has been for a while.
    Slow,
    /// The last poll timed out. The data on screen is the last good read.
    Stalled,
}

// ─── commands (UI → worker) ──────────────────────────────────────────────────

pub(super) enum Cmd {
    Refresh,
    Select(Option<PathBuf>),
    Lock {
        dir: PathBuf,
        path: String,
    },
    Unlock {
        dir: PathBuf,
        path: String,
    },
    ConfigSet {
        dir: PathBuf,
        key: String,
        value: String,
    },
    /// keep-mine: the guided lock → apply → unlock → discard sequence (the
    /// daemon's ConflictApply needs a self-held lease, so a bare apply won't do).
    ResolveMine {
        dir: PathBuf,
        id: String,
        target: String,
    },
    /// keep-both: the same sequence into a fresh path, with no discard. The
    /// only resolution that deletes nothing, and therefore the only one
    /// offered for a preserved copy whose original path is unknown.
    ResolveBoth {
        dir: PathBuf,
        id: String,
        target: String,
    },
    /// Restore version `n`: guided lock → restore → unlock (the daemon's
    /// Restore needs a self-held lease; the replaced content is pushed to
    /// history first, so nothing is lost).
    Restore {
        dir: PathBuf,
        path: String,
        n: usize,
    },
    Tag {
        dir: PathBuf,
        path: String,
        n: usize,
        name: Option<String>,
    },
    Pin {
        dir: PathBuf,
        path: String,
        n: usize,
        pinned: bool,
    },
    ConflictDiscard {
        dir: PathBuf,
        id: String,
    },
    PeerName {
        dir: PathBuf,
        id: String,
        name: Option<String>,
    },
    Start(PathBuf),
    Stop(PathBuf),
    Pause(PathBuf),
    Resume(PathBuf),
    Init(PathBuf),
    Join(PathBuf, String),
    /// Open the OS folder picker; the chosen path lands in `Shared.picked`.
    PickFolder(PickTarget),
    Quit,

    // ─── P36: the operations the CLI had and the window did not ─────────────
    /// Join the daemon's waitlist for a held path instead of re-clicking Lock.
    /// The one remedy the daemon suggests for a LEASE refusal.
    LockWait {
        dir: PathBuf,
        path: String,
    },
    /// Rename a synced path under a lease: lock → rename → publish. A bare
    /// rename in the file manager has its delete half reverted by design, so
    /// this is the only way to rename from the GUI.
    Move {
        dir: PathBuf,
        from: String,
        to: String,
    },
    /// What changed between version `n` and the current bytes.
    Diff {
        dir: PathBuf,
        path: String,
        n: usize,
    },
    /// The daemon's connectivity self-check.
    Doctor {
        dir: PathBuf,
    },
    /// Start the web dashboard if needed and report its address.
    Dashboard {
        dir: PathBuf,
    },
    /// Drop unreferenced blobs.
    Gc {
        dir: PathBuf,
    },
    /// Delete preserved copies older than `older_than_ms`. Destructive, so the
    /// view confirms with the exact count and byte total first.
    PruneConflicts {
        dir: PathBuf,
        older_than_ms: u64,
        /// Names resolved by the view from the list it is showing, so the user
        /// confirms against exactly what will be deleted.
        names: Vec<String>,
    },
    /// Mint an invite scoped to a role and an expiry, rather than the
    /// never-expiring editor ticket the window used to be hardcoded to.
    Invite {
        dir: PathBuf,
        role: Option<String>,
        ttl_ms: Option<u64>,
    },
    /// Rotate the session secret. The only honest revocation.
    Rekey {
        dir: PathBuf,
    },
    /// Install or remove the OS supervisor so sessions survive a reboot.
    Supervisor {
        install: bool,
    },
    /// Abandon an in-flight command at its next safe boundary.
    Cancel(u64),
    /// Ask the daemon to match a pattern against the whole index, rather than
    /// filtering the capped list the snapshot carries.
    SearchFiles {
        dir: PathBuf,
        pattern: String,
        by_size: bool,
        desc: bool,
        offset: usize,
    },
}
