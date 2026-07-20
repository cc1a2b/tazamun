//! The daemon: a single state-owning actor task.
//!
//! Invariant: every mutation of [`AppState`], the [`LockTable`] and the member
//! table happens inside this one task via message passing — no shared-state
//! locking anywhere. Heavy I/O (chunking, fetching, assembly, hashing) runs in
//! spawned tasks that report completion events back into the loop.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_blobs::BlobsProtocol;
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};
use iroh_gossip::proto::TopicId;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, instrument, warn};

use crate::consts::{META_DIR, MUTE_WINDOW, REDIAL_BACKOFF_MAX, REDIAL_BACKOFF_MIN};
use crate::guard;
use crate::ipc::{self, IpcRequest, IpcResponse};
use crate::locks::{Decision, LockTable, LockTimings};
use crate::net::control::{PeerEvent, PeerHandle, handshake_acceptor, handshake_initiator};
use crate::net::endpoint::{NetConfig, build_endpoint, path_info};
use crate::net::membership::{self, MemberCmd, MemberEvent};
use crate::net::telemetry::{HealthGrade, PeerHealth};
use crate::proto::{DenyReason, FileRecord, LeaseInfo, Msg};
use crate::session::{AddrWire, SessionKeys, SessionSecret, Ticket};
use crate::state::{AppState, RelPath, VersionEntry};
use crate::sync::index::{diff, sanitize_rel_path};
use crate::sync::transfer::{Published, Staged, Transfer, TransferError};
use crate::sync::vclock::{self, Causality};
use crate::ui::progress::{Meter, Ui};
use crate::versions;
use crate::watcher::WatchEvent;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    #[error(transparent)]
    Ipc(#[from] ipc::IpcError),
    #[error(transparent)]
    Net(#[from] crate::net::endpoint::NetError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error(transparent)]
    Watch(#[from] crate::watcher::WatchError),
    #[error("gossip: {0}")]
    Gossip(String),
}

/// Configuration for one daemon instance.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub dir: PathBuf,
    pub net: NetConfig,
    pub timings: LockTimings,
    /// Terminal progress presentation; `Ui::disabled()` for headless runs.
    pub ui: Ui,
}

/// What triggered a publish, deciding what happens after it commits.
pub(crate) enum PublishCause {
    /// A watched edit under a self-held lease.
    Edit,
    /// Genesis import at first start: no lease required, nothing broadcast
    /// beyond the normal FileMeta (there are no peers yet anyway).
    Import,
    /// Final flush before releasing a lease; the reply completes the unlock.
    Unlock(oneshot::Sender<IpcResponse>),
}

/// Result of inspecting a path's on-disk bytes against its index record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inspection {
    /// Disk agrees with the index (or both say "absent").
    Clean,
    /// Disk bytes differ from the indexed manifest.
    Differs,
    /// Indexed (non-tombstone) file is missing from disk.
    MissingIndexed,
    /// A file exists on disk with no live index record.
    Unindexed,
}

/// Why an inspection was requested.
pub(crate) enum InspectCause {
    Watch,
    Unlock(oneshot::Sender<IpcResponse>),
}

/// A pull job for one path.
struct PullJob {
    from: EndpointId,
    record: FileRecord,
    attempts: u32,
    queued: Option<(EndpointId, FileRecord)>,
}

/// Apply work postponed because a watch-side operation held the path.
struct DeferredApply {
    from: EndpointId,
    record: FileRecord,
    staged: Option<Staged>,
}

pub(crate) enum Event {
    Authed {
        conn: Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        initiated_by_me: bool,
    },
    Peer(PeerEvent),
    DialFailed(EndpointId),
    RetryDial(EndpointId),
    Member(MemberEvent),
    Watch(WatchEvent),
    Inspected {
        rel: RelPath,
        outcome: Result<Inspection, TransferError>,
        cause: InspectCause,
    },
    PublishDone {
        rel: RelPath,
        result: Result<Published, TransferError>,
        cause: PublishCause,
    },
    PullStaged {
        rel: RelPath,
        from: EndpointId,
        record: FileRecord,
        result: Result<Staged, TransferError>,
    },
    PullRetry {
        rel: RelPath,
    },
    ViolationStaged {
        rel: RelPath,
        result: Result<Staged, TransferError>,
        quarantined: Option<PathBuf>,
        /// True when offending bytes existed to preserve: the restore may only
        /// proceed if `quarantined` is `Some` (Golden Invariant — never destroy
        /// bytes that were not preserved first).
        preserve_required: bool,
    },
    /// Autolock: the un-leased bytes have been preserved (async), so the
    /// standard acquire may now begin.
    AutolockReady {
        rel: RelPath,
        quarantined: Option<PathBuf>,
        new_file: bool,
    },
    RestoreStaged {
        rel: RelPath,
        entry: VersionEntry,
        result: Result<Staged, TransferError>,
        reply: oneshot::Sender<IpcResponse>,
    },
    /// P18: the quarantined bytes were staged into a temp file (async); the
    /// actor atomically renames it into the leased working path.
    ConflictApplied {
        rel: RelPath,
        result: Result<(tempfile::TempPath, u64), String>,
        reply: oneshot::Sender<IpcResponse>,
    },
    GcDone {
        result: Result<usize, TransferError>,
        reply: Option<oneshot::Sender<IpcResponse>>,
    },
    Ipc(IpcRequest, oneshot::Sender<IpcResponse>),
    AcquireTimeout {
        rel: RelPath,
        lamport: u64,
    },
    Sweep,
    Renew,
    GcTick,
    TelemetryTick,
    RepunchTick,
    PathChanged {
        id: EndpointId,
        conn_id: usize,
    },
    Shutdown(oneshot::Sender<()>),
}

/// Accept handler for the control ALPN: handshake, then hand the peer to the
/// actor.
#[derive(Debug, Clone)]
struct CtlAccept {
    keys: std::sync::Arc<SessionKeys>,
    me: EndpointId,
    events: mpsc::Sender<Event>,
    /// DoS bound: permits for concurrent pre-auth handshakes
    /// ([`crate::consts::MAX_INFLIGHT_HANDSHAKES`]).
    handshakes: std::sync::Arc<tokio::sync::Semaphore>,
}

impl ProtocolHandler for CtlAccept {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // DoS bound: a peer that knows the gossip topic but not the session
        // secret can open QUIC connections it can never authenticate. Cap the
        // number mid-handshake at once; beyond it, close immediately rather
        // than tie up a task and a stream for the full handshake deadline. The
        // permit is held (RAII) for the duration of the handshake.
        let Ok(_permit) = self.handshakes.clone().try_acquire_owned() else {
            debug!("at MAX_INFLIGHT_HANDSHAKES; refusing inbound control connection");
            conn.close(iroh::endpoint::VarInt::from_u32(2), b"busy");
            return Ok(());
        };
        match handshake_acceptor(&conn, &self.keys, self.me).await {
            Ok((send, recv)) => {
                let _ = self
                    .events
                    .send(Event::Authed {
                        conn,
                        send,
                        recv,
                        initiated_by_me: false,
                    })
                    .await;
            }
            Err(_) => {
                // handshake_* already closed the connection and logged one
                // generic warning; nothing else leaves this handler.
            }
        }
        Ok(())
    }
}

struct MemberInfo {
    addr: EndpointAddr,
    last_seen: Instant,
}

/// Handle to a running daemon (used by the CLI's `start` and by tests).
pub struct DaemonHandle {
    id: EndpointId,
    endpoint: Endpoint,
    events: mpsc::Sender<Event>,
    actor: tokio::task::JoinHandle<()>,
    /// P21: flips true when the actor has exited (any cause).
    stopped: tokio::sync::watch::Receiver<bool>,
    aux: Vec<tokio::task::JoinHandle<()>>,
}

impl DaemonHandle {
    pub fn id(&self) -> EndpointId {
        self.id
    }

    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Resolves once the daemon actor has stopped — e.g. after an IPC
    /// `Shutdown` (the GUI's Stop button). Race-free: a watch remembers a
    /// flip that happened before the call.
    pub async fn wait_shutdown(&self) {
        let mut rx = self.stopped.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return; // sender dropped ⇒ actor gone
            }
        }
    }

    /// Issues a request exactly like the IPC socket would.
    pub async fn request(&self, req: IpcRequest) -> IpcResponse {
        let (tx, rx) = oneshot::channel();
        if self.events.send(Event::Ipc(req, tx)).await.is_err() {
            return IpcResponse::err("shutting_down", "daemon is shutting down");
        }
        rx.await
            .unwrap_or_else(|_| IpcResponse::err("internal", "daemon dropped the request"))
    }

    /// Graceful shutdown: releases leases, says goodbye, persists state.
    pub async fn shutdown(self) {
        let (tx, rx) = oneshot::channel();
        if self.events.send(Event::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
        let _ = self.actor.await;
        for h in self.aux {
            h.abort();
        }
    }

    /// Simulates a crash: no lease release, no goodbye, no state flush.
    pub async fn kill(self) {
        self.actor.abort();
        for h in self.aux {
            h.abort();
        }
        self.endpoint.close().await;
    }
}

/// Builds and spawns a daemon for an initialized session folder.
pub async fn spawn(cfg: DaemonConfig) -> Result<DaemonHandle, DaemonError> {
    let dir = cfg.dir.clone();
    let state = AppState::load(&dir)?;
    guard::enforce_all(&dir, &state, state.config.enforce_readonly()).map_err(|e| {
        DaemonError::State(crate::state::StateError::Io(std::io::Error::other(
            e.to_string(),
        )))
    })?;

    // Bind IPC before the network so "already running" fails fast.
    let ipc_listener = ipc::bind(&dir).await?;

    let secret = iroh::SecretKey::from_bytes(&state.secret_key_bytes()?);
    let session_secret = SessionSecret(state.session_secret_bytes()?);
    let keys = SessionKeys::derive(&session_secret);
    let topic = TopicId::from_bytes(keys.topic);

    let endpoint = build_endpoint(secret, &cfg.net).await?;
    let me = endpoint.id();
    let transfer = Transfer::open(dir.clone(), crate::consts::GC_INTERVAL).await?;

    let (events_tx, events_rx) = mpsc::channel::<Event>(4096);

    let gossip = Gossip::builder().spawn(endpoint.clone());
    let ctl_handshakes = std::sync::Arc::new(tokio::sync::Semaphore::new(
        crate::consts::MAX_INFLIGHT_HANDSHAKES,
    ));
    let router = Router::builder(endpoint.clone())
        .accept(
            crate::consts::CTL_ALPN,
            CtlAccept {
                keys: std::sync::Arc::new(keys.clone()),
                me,
                events: events_tx.clone(),
                handshakes: ctl_handshakes,
            },
        )
        .accept(iroh_blobs::ALPN, BlobsProtocol::new(transfer.store(), None))
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();

    // Membership task: presence beacons + gossip events.
    let (member_cmds_tx, member_cmds_rx) = mpsc::channel::<MemberCmd>(64);
    let bootstrap: Vec<EndpointId> = state
        .known_members
        .values()
        .filter_map(|a| a.endpoint_id())
        .filter(|id| *id != me)
        .collect();
    let (member_events_tx, mut member_events_rx) = mpsc::channel::<MemberEvent>(256);
    let membership_task = tokio::spawn(membership::run(
        gossip.clone(),
        endpoint.clone(),
        keys.clone(),
        topic,
        bootstrap,
        member_events_tx,
        member_cmds_rx,
    ));
    let member_fwd = {
        let events = events_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = member_events_rx.recv().await {
                if events.send(Event::Member(ev)).await.is_err() {
                    break;
                }
            }
        })
    };

    // Watcher: raw watch events into the loop.
    let (watch_tx, mut watch_rx) = mpsc::channel::<WatchEvent>(1024);
    let watcher = crate::watcher::spawn(dir.clone(), watch_tx)?;
    let watch_fwd = {
        let events = events_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = watch_rx.recv().await {
                if events.send(Event::Watch(ev)).await.is_err() {
                    break;
                }
            }
        })
    };

    // IPC server (local socket) and the loopback web dashboard both feed the
    // same actor message channel, so the dashboard is a thin adapter with no
    // second control path of its own.
    let (ipc_tx, mut ipc_rx) = mpsc::channel::<(IpcRequest, oneshot::Sender<IpcResponse>)>(64);
    let ipc_task = tokio::spawn(ipc_listener.serve(ipc_tx.clone()));
    let ipc_fwd = {
        let events = events_tx.clone();
        tokio::spawn(async move {
            while let Some((req, reply)) = ipc_rx.recv().await {
                if events.send(Event::Ipc(req, reply)).await.is_err() {
                    break;
                }
            }
        })
    };
    // Web dashboard: a fresh per-start session token + the persisted loopback
    // port. Started ON DEMAND by `tazamun dashboard` (a `DashboardStart` IPC),
    // never at daemon startup — so nothing binds the loopback port until asked
    // and an unused dashboard can never clash or warn. `bound` reads 0 until the
    // server actually binds; `serve` then publishes the real port.
    let dashboard_port = state.config.dashboard_port;
    let dashboard_token = {
        let bytes: [u8; crate::consts::DASHBOARD_TOKEN_BYTES] = rand::random();
        data_encoding::HEXLOWER.encode(&bytes)
    };
    let dashboard_bound = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0));

    // Timers.
    let timer_task = {
        let events = events_tx.clone();
        let renew_every = cfg.timings.renew;
        tokio::spawn(async move {
            let mut sweep = tokio::time::interval(Duration::from_millis(250));
            let mut renew = tokio::time::interval(renew_every);
            let mut gc = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            let mut telemetry = tokio::time::interval(crate::consts::TELEMETRY_INTERVAL);
            let mut repunch = tokio::time::interval(crate::consts::REPUNCH_INTERVAL);
            gc.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            repunch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first immediate ticks are harmless no-ops.
            loop {
                let ev = tokio::select! {
                    _ = sweep.tick() => Event::Sweep,
                    _ = renew.tick() => Event::Renew,
                    _ = gc.tick() => Event::GcTick,
                    _ = telemetry.tick() => Event::TelemetryTick,
                    _ = repunch.tick() => Event::RepunchTick,
                };
                if events.send(ev).await.is_err() {
                    break;
                }
            }
        })
    };

    let relay_policy = match &cfg.net.relay {
        crate::net::endpoint::RelayChoice::Default => "default (n0 public relays)".to_string(),
        crate::net::endpoint::RelayChoice::Custom(url) => format!("custom: {url}"),
        crate::net::endpoint::RelayChoice::Disabled => "disabled (--no-relay)".to_string(),
    };
    let me_str = me.to_string();
    let ignore = load_ignore_set(&dir, &state.config);
    let limiter = std::sync::Arc::new(crate::ratelimit::RateLimiter::new(state.config.max_down));
    // P19: open the audit log if enabled (best-effort — a failure to open just
    // disables the trail, never the daemon).
    let audit = if state.config.audit {
        crate::audit::AuditLog::open(&dir).ok()
    } else {
        None
    };
    let actor = Actor {
        dir,
        state,
        me,
        me_str: me_str.clone(),
        endpoint: endpoint.clone(),
        router,
        transfer,
        locks: LockTable::new(me_str, cfg.timings),
        timings: cfg.timings,
        peers: BTreeMap::new(),
        peer_index: BTreeMap::new(),
        peer_roles: BTreeMap::new(),
        index_received: BTreeSet::new(),
        index_staging: BTreeMap::new(),
        members: BTreeMap::new(),
        dialing: BTreeSet::new(),
        redial_scheduled: BTreeSet::new(),
        backoff: BTreeMap::new(),
        pending_pulls: BTreeMap::new(),
        pull_backlog: std::collections::VecDeque::new(),
        pull_meters: BTreeMap::new(),
        limiter,
        audit,
        peer_health: BTreeMap::new(),
        fast_redial_used: BTreeSet::new(),
        events_ring: std::collections::VecDeque::new(),
        event_seq: 0,
        deferred: BTreeMap::new(),
        pending_acquires: BTreeMap::new(),
        pending_unlocks: BTreeMap::new(),
        my_waits: BTreeMap::new(),
        interest: BTreeMap::new(),
        autolock_idle: BTreeMap::new(),
        autolock_pending: BTreeMap::new(),
        warned_nonportable: BTreeSet::new(),
        ignore,
        held_local: BTreeMap::new(),
        muted: HashMap::new(),
        busy: BTreeSet::new(),
        recheck: BTreeSet::new(),
        gc_running: false,
        gc_dirty: false,
        shutdown_requested: false,
        ui: cfg.ui.clone(),
        relay_policy,
        airgap: cfg.net.airgap,
        lan_enabled: cfg.net.lan,
        keys,
        events_tx: events_tx.clone(),
        member_cmds: member_cmds_tx,
        dashboard_ipc: ipc_tx.clone(),
        dashboard_port,
        dashboard_started: false,
        dashboard_token,
        dashboard_bound,
        _watcher: watcher,
    };

    // P21: a watch flips true when the actor exits for ANY reason (Ctrl-C
    // shutdown, IPC Shutdown, crash) so `start` and the GUI can await it.
    let (stopped_tx, stopped_rx) = tokio::sync::watch::channel(false);
    let actor_task = tokio::spawn(async move {
        actor.run(events_rx).await;
        let _ = stopped_tx.send(true);
    });

    Ok(DaemonHandle {
        id: me,
        endpoint,
        events: events_tx,
        actor: actor_task,
        stopped: stopped_rx,
        aux: vec![
            membership_task,
            member_fwd,
            watch_fwd,
            ipc_task,
            ipc_fwd,
            timer_task,
        ],
    })
}

struct Actor {
    dir: PathBuf,
    state: AppState,
    me: EndpointId,
    me_str: String,
    endpoint: Endpoint,
    router: Router,
    transfer: Transfer,
    locks: LockTable,
    timings: LockTimings,
    peers: BTreeMap<EndpointId, PeerHandle>,
    peer_index: BTreeMap<EndpointId, BTreeMap<RelPath, FileRecord>>,
    /// P17: each authenticated peer's role code, from its verified `Identity`
    /// grant. Absent = not yet advertised / legacy peer. Enforced on `LockReq`
    /// only when this session is role-enforcing (`AppState::enforcing_roles`).
    peer_roles: BTreeMap<EndpointId, u8>,
    index_received: BTreeSet<EndpointId>,
    /// P20: partially-received sharded indexes, keyed by peer. Files stage here
    /// as `IndexPart`s arrive and are promoted to `peer_index` only when the
    /// final part lands, so freshness/voter logic never sees a partial index.
    /// The `u32` is the next expected `seq` (contiguity check).
    index_staging: BTreeMap<EndpointId, (u32, BTreeMap<RelPath, FileRecord>)>,
    members: BTreeMap<EndpointId, MemberInfo>,
    dialing: BTreeSet<EndpointId>,
    redial_scheduled: BTreeSet<EndpointId>,
    backoff: BTreeMap<EndpointId, Duration>,
    pending_pulls: BTreeMap<RelPath, PullJob>,
    /// DoS bound: paths waiting to become active pulls once a slot frees, so at
    /// most [`crate::consts::MAX_CONCURRENT_PULLS`] pulls run at once. Bounded
    /// by [`crate::consts::MAX_PULL_BACKLOG`].
    pull_backlog: std::collections::VecDeque<(RelPath, EndpointId, FileRecord)>,
    pull_meters: BTreeMap<RelPath, std::sync::Arc<Meter>>,
    /// P15: shared download rate limiter (`max-down`) applied to every chunk
    /// fetch across all pulls; unlimited by default, reconfigurable live.
    limiter: std::sync::Arc<crate::ratelimit::RateLimiter>,
    /// P19 append-only audit log (`None` when `config.audit` is off or the file
    /// could not be opened). Written from the actor at lifecycle sites.
    audit: Option<crate::audit::AuditLog>,
    peer_health: BTreeMap<EndpointId, PeerHealth>,
    fast_redial_used: BTreeSet<EndpointId>,
    events_ring: std::collections::VecDeque<(u64, String)>,
    event_seq: u64,
    deferred: BTreeMap<RelPath, DeferredApply>,
    pending_acquires: BTreeMap<RelPath, (u64, oneshot::Sender<IpcResponse>)>,
    pending_unlocks: BTreeMap<RelPath, ()>,
    /// Paths this node is waiting for: `path → (holder id, give-up deadline)`.
    /// Populated by `LockWait`; surfaced in `status`/`locks` and expired by the
    /// sweep.
    my_waits: BTreeMap<RelPath, (String, Instant)>,
    /// Peers that told us (via `LockInterest`) they want a path we hold/know —
    /// shown as waiters in `status`/`locks`.
    interest: BTreeMap<RelPath, BTreeSet<String>>,
    /// Leases acquired by autolock-on-first-write: `path → idle-release
    /// deadline`. Each write resets it; the sweep releases when it passes.
    autolock_idle: BTreeMap<RelPath, Instant>,
    /// Autolock acquires in flight: `path → (preserved-bytes quarantine, was it
    /// a brand-new file)`. Resolved by the grant/deny/timeout handlers.
    autolock_pending: BTreeMap<RelPath, (Option<PathBuf>, bool)>,
    /// Paths already warned about as Windows-non-portable this run (Unix
    /// warn-only mode; keeps the log to one line per path).
    warned_nonportable: BTreeSet<RelPath>,
    /// P11 sync-scope policy: junk preset + `.tazamunignore` + selective sync
    /// + size ceiling. Rebuilt whenever the ignore file settles on disk.
    ignore: crate::sync::ignore::IgnoreSet,
    /// Local files held out of the sync scope (left on disk untouched, never
    /// published, never quarantined) with the reason — surfaced in `status`.
    /// Not persisted: recomputed from events and the startup scan each run.
    held_local: BTreeMap<RelPath, String>,
    muted: HashMap<RelPath, Instant>,
    busy: BTreeSet<RelPath>,
    recheck: BTreeSet<RelPath>,
    gc_running: bool,
    gc_dirty: bool,
    /// P21: set by the IPC `Shutdown` verb; honored between events in the run
    /// loop so the requester's ok reply is sent before teardown begins.
    shutdown_requested: bool,
    ui: Ui,
    relay_policy: String,
    airgap: bool,
    lan_enabled: bool,
    keys: SessionKeys,
    events_tx: mpsc::Sender<Event>,
    member_cmds: mpsc::Sender<MemberCmd>,
    /// Web dashboard: the per-start session token (returned to the CLI over IPC
    /// so the browser can present it) and the actual bound loopback port (0
    /// until started). The server is spawned on demand from `DashboardStart`
    /// using `dashboard_ipc` (its adapter into this actor) and `dashboard_port`;
    /// `dashboard_started` makes the start idempotent.
    dashboard_token: String,
    dashboard_bound: std::sync::Arc<std::sync::atomic::AtomicU16>,
    dashboard_ipc: mpsc::Sender<(IpcRequest, oneshot::Sender<IpcResponse>)>,
    dashboard_port: u16,
    dashboard_started: bool,
    _watcher: crate::watcher::Watcher,
}

impl Actor {
    async fn run(mut self, mut events: mpsc::Receiver<Event>) {
        self.dial_known_members();
        self.startup_scan();
        info!(me = %self.me.fmt_short(), dir = %self.dir.display(), "daemon running");
        while let Some(ev) = events.recv().await {
            match ev {
                Event::Authed {
                    conn,
                    send,
                    recv,
                    initiated_by_me,
                } => self.on_authed(conn, send, recv, initiated_by_me),
                Event::Peer(PeerEvent::Msg { id, msg }) => self.on_ctl(id, msg).await,
                Event::Peer(PeerEvent::Gone { id, conn_id }) => self.on_peer_gone(id, conn_id),
                Event::DialFailed(id) => {
                    self.dialing.remove(&id);
                    self.schedule_redial(id);
                }
                Event::RetryDial(id) => {
                    self.redial_scheduled.remove(&id);
                    self.dial(id);
                }
                Event::Member(ev) => self.on_member(ev),
                Event::Watch(ev) => self.on_watch(ev),
                Event::Inspected {
                    rel,
                    outcome,
                    cause,
                } => self.on_inspected(rel, outcome, cause),
                Event::PublishDone { rel, result, cause } => {
                    self.on_publish_done(rel, result, cause)
                }
                Event::PullStaged {
                    rel,
                    from,
                    record,
                    result,
                } => self.on_pull_staged(rel, from, record, result),
                Event::PullRetry { rel } => {
                    if let Some(job) = self.pending_pulls.get(&rel) {
                        let (from, record) = (job.from, job.record.clone());
                        self.start_pull(rel, from, record);
                    }
                }
                Event::ViolationStaged {
                    rel,
                    result,
                    quarantined,
                    preserve_required,
                } => self.on_violation_staged(rel, result, quarantined, preserve_required),
                Event::AutolockReady {
                    rel,
                    quarantined,
                    new_file,
                } => self.begin_autolock_acquire(rel, quarantined, new_file),
                Event::RestoreStaged {
                    rel,
                    entry,
                    result,
                    reply,
                } => self.on_restore_staged(rel, entry, result, reply),
                Event::ConflictApplied { rel, result, reply } => {
                    self.on_conflict_applied(rel, result, reply)
                }
                Event::GcDone { result, reply } => {
                    self.gc_running = false;
                    if let Err(e) = &result {
                        warn!("gc protection refresh failed: {e}");
                    }
                    if std::mem::take(&mut self.gc_dirty) {
                        self.start_gc(None);
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(match result {
                            Ok(protected) => IpcResponse::ok(serde_json::json!({
                                "protected_blobs": protected,
                                "note": "unreferenced blobs are swept by the store's scheduled gc",
                            })),
                            Err(e) => IpcResponse::err("gc_failed", e.to_string()),
                        });
                    }
                }
                Event::Ipc(req, reply) => self.on_ipc(req, reply),
                Event::AcquireTimeout { rel, lamport } => self.on_acquire_timeout(rel, lamport),
                Event::Sweep => self.on_sweep(),
                Event::Renew => self.on_renew_tick(),
                Event::GcTick => self.start_gc(None),
                Event::TelemetryTick => self.on_telemetry_tick(),
                Event::RepunchTick => self.on_repunch_tick(),
                Event::PathChanged { id, conn_id } => self.on_path_changed(id, conn_id),
                Event::Shutdown(reply) => {
                    self.graceful_shutdown().await;
                    let _ = reply.send(());
                    return;
                }
            }
            // P21: an IPC `Shutdown` request sets this flag from inside its own
            // event; honoring it here (between events) keeps the reply ordering
            // sane — the requester got its ok before the teardown begins.
            if self.shutdown_requested {
                self.graceful_shutdown().await;
                return;
            }
        }
    }

    // ---------------- peers & membership ----------------

    fn on_authed(
        &mut self,
        conn: Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        initiated_by_me: bool,
    ) {
        let id = conn.remote_id();
        if id == self.me {
            conn.close(iroh::endpoint::VarInt::from_u32(0), b"self");
            return;
        }
        // DoS bound: a session is a small trusted group. Refuse a *new* peer id
        // past MAX_PEERS (a reconnecting/duplicate known peer still gets in via
        // the duplicate-handling path below).
        if !self.peers.contains_key(&id) && self.peers.len() >= crate::consts::MAX_PEERS {
            warn!(peer = %id.fmt_short(), "at MAX_PEERS; refusing new control connection");
            conn.close(iroh::endpoint::VarInt::from_u32(2), b"busy");
            return;
        }
        let new_initiator = if initiated_by_me { self.me } else { id };
        if let Some(existing) = self.peers.get(&id) {
            let existing_initiator = existing.initiator_id(self.me);
            // Duplicate connections: keep the one initiated by the lower id.
            if new_initiator.as_bytes() < existing_initiator.as_bytes() {
                debug!(peer = %id.fmt_short(), "replacing duplicate control connection");
                if let Some(old) = self.peers.remove(&id) {
                    old.close();
                }
            } else {
                debug!(peer = %id.fmt_short(), "dropping duplicate control connection");
                conn.close(iroh::endpoint::VarInt::from_u32(0), b"duplicate");
                return;
            }
        }
        let handle =
            PeerHandle::spawn(conn, send, recv, initiated_by_me, self.peer_events_sender());
        info!(peer = %id.fmt_short(), "peer authenticated");
        self.observe("peer-connected", None, Some(&id.to_string()), None);
        self.dialing.remove(&id);
        self.backoff.remove(&id);
        self.fast_redial_used.remove(&id);
        self.members
            .entry(id)
            .or_insert_with(|| MemberInfo {
                addr: EndpointAddr::new(id),
                last_seen: Instant::now(),
            })
            .last_seen = Instant::now();
        // Telemetry: fresh counters + first sample, and a path-event watcher
        // whose stream ends with the connection (no explicit teardown).
        let now = Instant::now();
        let sample = crate::net::endpoint::sample_connection(&handle.conn);
        let health = self
            .peer_health
            .entry(id)
            .or_insert_with(|| PeerHealth::seen_only(now));
        health.on_connect(now);
        health.on_sample(&sample, now);
        self.push_event(format!(
            "peer {} connected ({}, rtt {:.0}ms)",
            id.fmt_short(),
            sample.conn,
            sample.rtt_ms
        ));
        {
            let conn = handle.conn.clone();
            let conn_id = handle.conn_id();
            let events = self.events_tx.clone();
            tokio::spawn(async move {
                use n0_future::StreamExt;
                let mut path_events = conn.path_events();
                while let Some(ev) = path_events.next().await {
                    use iroh::endpoint::PathEvent;
                    let relevant = matches!(
                        ev,
                        PathEvent::Opened { .. }
                            | PathEvent::Closed { .. }
                            | PathEvent::Selected { .. }
                    );
                    if relevant
                        && events
                            .send(Event::PathChanged { id, conn_id })
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
            });
        }
        // P20: the index ships as one Msg::Index (small folders) or ordered
        // Msg::IndexParts (large), all before the Identity — same relative order.
        for part in self.build_index_parts() {
            handle.send(part);
        }
        // P17: advertise our signed role grant so the peer records and enforces
        // our role. Absent on a legacy (v1) session — enforcement stays off.
        if let Some(grant) = &self.state.my_grant {
            handle.send(Msg::Identity {
                grant: grant.clone(),
            });
        }
        self.peers.insert(id, handle);
        let _ = self.member_cmds.try_send(MemberCmd::JoinPeers(vec![id]));
    }

    fn peer_events_sender(&self) -> mpsc::Sender<PeerEvent> {
        let events = self.events_tx.clone();
        let (tx, mut rx) = mpsc::channel::<PeerEvent>(1024);
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if events.send(Event::Peer(ev)).await.is_err() {
                    break;
                }
            }
        });
        tx
    }

    // (cap_detail is a free fn below — pure and unit-tested.)

    /// P19 single observability point: record one lifecycle event to the audit
    /// log, fire the matching user hook, and (for noteworthy kinds) a desktop
    /// notification — each gated by its config toggle and each entirely
    /// off-path (audit is a small append like `persist`; hooks/notifications
    /// run off the actor). Call this at every lifecycle site with a stable
    /// `kind` slug; the routing to hooks/notify lives in those modules.
    fn observe(
        &mut self,
        kind: &str,
        path: Option<&str>,
        peer: Option<&str>,
        detail: Option<String>,
    ) {
        // Bound `detail` so no single audit line, hook stdin payload, or
        // notification body can grow without limit (a departing peer holding
        // thousands of leases would otherwise produce a huge string).
        let detail = detail.map(cap_detail);
        if let Some(a) = &mut self.audit {
            a.emit(kind, path, peer, detail.clone());
        }
        // Only build the (allocating) hook payload when a hook is actually
        // installed — the `exists` stat is cheap, an unused JSON alloc is not.
        if self.state.config.hooks
            && let Some(hook) = crate::hooks::hook_for(kind)
            && crate::hooks::exists(&self.dir, hook)
        {
            crate::hooks::fire(
                &self.dir,
                hook,
                serde_json::json!({
                    "event": hook,
                    "kind": kind,
                    "ts_ms": crate::now_ms(),
                    "folder": self.dir.display().to_string(),
                    "path": path,
                    "peer": peer,
                    "detail": detail,
                }),
            );
        }
        if self.state.config.notify {
            let body = match (path, &detail) {
                (Some(p), Some(d)) => format!("{p}: {d}"),
                (Some(p), None) => p.to_string(),
                (None, Some(d)) => d.clone(),
                (None, None) => kind.to_string(),
            };
            crate::notify::maybe_notify(kind, body);
        }
    }

    /// Pushes a human-readable reconnect/path event into the status ring.
    fn push_event(&mut self, text: String) {
        self.event_seq += 1;
        self.events_ring.push_back((self.event_seq, text));
        while self.events_ring.len() > crate::consts::EVENT_RING {
            self.events_ring.pop_front();
        }
    }

    /// Samples every live connection and grades it; logs Direct↔Relayed
    /// transitions as ring events.
    fn on_telemetry_tick(&mut self) {
        let now = Instant::now();
        let samples: Vec<(EndpointId, crate::net::telemetry::PathSample)> = self
            .peers
            .iter()
            .map(|(id, h)| (*id, crate::net::endpoint::sample_connection(&h.conn)))
            .collect();
        for (id, sample) in samples {
            let health = self
                .peer_health
                .entry(id)
                .or_insert_with(|| PeerHealth::seen_only(now));
            let before = health.conn;
            health.on_sample(&sample, now);
            use crate::net::telemetry::ConnState;
            match (before, sample.conn) {
                (ConnState::Relayed, ConnState::Direct) => {
                    info!(peer = %id.fmt_short(), rtt_ms = sample.rtt_ms, "link upgraded Relayed→Direct");
                    self.push_event(format!(
                        "peer {} upgraded Relayed→Direct (rtt {:.0}ms)",
                        id.fmt_short(),
                        sample.rtt_ms
                    ));
                }
                (ConnState::Direct, ConnState::Relayed) => {
                    warn!(peer = %id.fmt_short(), "link downgraded Direct→Relayed");
                    self.push_event(format!("peer {} downgraded Direct→Relayed", id.fmt_short()));
                }
                _ => {}
            }
        }
    }

    /// For peers stuck on a relay, nudge iroh to re-attempt a direct path by
    /// re-adding the known address; upgrades are observed by the next sample.
    fn on_repunch_tick(&mut self) {
        use crate::net::telemetry::ConnState;
        let relayed: Vec<EndpointId> = self
            .peers
            .keys()
            .copied()
            .filter(|id| {
                self.peer_health
                    .get(id)
                    .is_some_and(|h| h.conn == ConnState::Relayed)
            })
            .collect();
        for id in relayed {
            if let Some(addr) = self.known_member_addr(&id) {
                let endpoint = self.endpoint.clone();
                debug!(peer = %id.fmt_short(), "re-hole-punch probe (currently relayed)");
                tokio::spawn(async move {
                    for t in addr.addrs {
                        if let iroh::TransportAddr::Ip(sock) = t {
                            endpoint.add_external_addr(sock).await;
                        }
                    }
                });
            }
        }
    }

    /// A path opened/closed/switched on a live connection.
    fn on_path_changed(&mut self, id: EndpointId, conn_id: usize) {
        if self.peers.get(&id).map(|h| h.conn_id()) != Some(conn_id) {
            return; // stale event from a replaced connection
        }
        let now = Instant::now();
        if let Some(h) = self.peer_health.get_mut(&id) {
            h.on_path_change(now);
        }
        // Re-sample immediately so the new selected path is reflected without
        // waiting for the next tick.
        if let Some(handle) = self.peers.get(&id) {
            let sample = crate::net::endpoint::sample_connection(&handle.conn);
            if let Some(h) = self.peer_health.get_mut(&id) {
                h.on_sample(&sample, now);
            }
        }
    }

    fn on_peer_gone(&mut self, id: EndpointId, conn_id: usize) {
        let current = self.peers.get(&id).map(|h| h.conn_id());
        if current != Some(conn_id) {
            return; // stale event from a replaced connection
        }
        if let Some(h) = self.peers.remove(&id) {
            h.close();
        }
        self.peer_index.remove(&id);
        self.peer_roles.remove(&id);
        self.index_received.remove(&id);
        self.index_staging.remove(&id); // P20: drop any partial sharded index
        info!(peer = %id.fmt_short(), "peer disconnected");
        let now = Instant::now();
        // Did the departed peer hold any lease? That is the notify-worthy case
        // (a checkout is now stuck until its TTL expires).
        let held: Vec<String> = self
            .locks
            .held_leases(now)
            .into_iter()
            .filter(|(_, holder, ..)| *holder == id.to_string())
            .map(|(path, ..)| path.as_str().to_string())
            .collect();
        let detail = if held.is_empty() {
            None
        } else {
            Some(format!("held {} lease(s): {}", held.len(), held.join(", ")))
        };
        self.observe("peer-offline", None, Some(&id.to_string()), detail.clone());
        if !held.is_empty() && self.state.config.notify {
            crate::notify::maybe_notify(
                "peer-offline-held",
                format!(
                    "peer {} went offline holding {} lease(s)",
                    id.fmt_short(),
                    held.len()
                ),
            );
        }
        if let Some(h) = self.peer_health.get_mut(&id) {
            h.on_disconnect(now);
        }
        self.push_event(format!("peer {} disconnected", id.fmt_short()));
        let aborted = self.locks.on_peer_down(&id.to_string());
        for rel in aborted {
            self.broadcast(Msg::LockRelease { path: rel.clone() });
            if let Some((_, reply)) = self.pending_acquires.remove(&rel) {
                let diag = self.peer_diag(&id, now, "voter", Some(false));
                let _ = reply.send(self.lock_error(
                    "voter_lost",
                    format!(
                        "peer {} disconnected while voting on the lease",
                        id.fmt_short()
                    ),
                    "REACHABILITY",
                    vec![diag],
                    "the peer whose grant was required went offline — retry once it reconnects",
                ));
            }
        }
        if self.members.contains_key(&id) || self.known_member_addr(&id).is_some() {
            // Fast path for transient blips: one immediate redial before the
            // exponential backoff curve takes over.
            if self.fast_redial_used.insert(id) {
                debug!(peer = %id.fmt_short(), "immediate redial after path loss");
                self.dial(id);
            } else {
                self.schedule_redial(id);
            }
        }
    }

    fn on_member(&mut self, ev: MemberEvent) {
        match ev {
            MemberEvent::Seen { id, addr, .. } => {
                if id == self.me {
                    return;
                }
                let wire = AddrWire::from_endpoint_addr(&addr);
                let changed = self.state.known_members.get(&id.to_string()) != Some(&wire);
                if changed {
                    self.state.known_members.insert(id.to_string(), wire);
                    self.persist();
                }
                let now = Instant::now();
                self.members.insert(
                    id,
                    MemberInfo {
                        addr,
                        last_seen: now,
                    },
                );
                // Control connection is authoritative for liveness: a presence
                // beacon only refreshes last_seen for the health snapshot.
                self.peer_health
                    .entry(id)
                    .or_insert_with(|| PeerHealth::seen_only(now))
                    .on_presence(now);
                if !self.peers.contains_key(&id)
                    && !self.dialing.contains(&id)
                    && !self.redial_scheduled.contains(&id)
                {
                    self.dial(id);
                }
            }
            MemberEvent::NeighborUp(id) => {
                if id != self.me && !self.peers.contains_key(&id) && !self.dialing.contains(&id) {
                    self.dial(id);
                }
            }
            MemberEvent::NeighborDown(_) => {}
        }
    }

    fn known_member_addr(&self, id: &EndpointId) -> Option<EndpointAddr> {
        if let Some(info) = self.members.get(id) {
            return Some(info.addr.clone());
        }
        self.state
            .known_members
            .get(&id.to_string())
            .and_then(|w| w.to_endpoint_addr())
    }

    fn dial_known_members(&mut self) {
        let ids: Vec<EndpointId> = self
            .state
            .known_members
            .values()
            .filter_map(|w| w.endpoint_id())
            .filter(|id| *id != self.me)
            .collect();
        for id in ids {
            self.dial(id);
        }
    }

    fn dial(&mut self, id: EndpointId) {
        if id == self.me || self.peers.contains_key(&id) || self.dialing.contains(&id) {
            return;
        }
        let Some(addr) = self.known_member_addr(&id) else {
            return;
        };
        self.dialing.insert(id);
        let endpoint = self.endpoint.clone();
        let keys = self.keys.clone();
        let me = self.me;
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let result = async {
                let conn = endpoint
                    .connect(addr, crate::consts::CTL_ALPN)
                    .await
                    .map_err(|e| e.to_string())?;
                let (send, recv) = handshake_initiator(&conn, &keys, me)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((conn, send, recv))
            }
            .await;
            let ev = match result {
                Ok((conn, send, recv)) => Event::Authed {
                    conn,
                    send,
                    recv,
                    initiated_by_me: true,
                },
                Err(e) => {
                    debug!(peer = %id.fmt_short(), "dial failed: {e}");
                    Event::DialFailed(id)
                }
            };
            let _ = events.send(ev).await;
        });
    }

    fn schedule_redial(&mut self, id: EndpointId) {
        if self.peers.contains_key(&id) || self.redial_scheduled.contains(&id) {
            return;
        }
        let next = self
            .backoff
            .get(&id)
            .map(|d| (*d * 2).min(REDIAL_BACKOFF_MAX))
            .unwrap_or(REDIAL_BACKOFF_MIN);
        self.backoff.insert(id, next);
        let jitter = Duration::from_millis(u64::from(rand::random::<u16>()) % 250);
        let delay = next + jitter;
        self.redial_scheduled.insert(id);
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = events.send(Event::RetryDial(id)).await;
        });
    }

    fn broadcast(&self, msg: Msg) {
        for handle in self.peers.values() {
            if !handle.send(msg.clone()) {
                warn!(peer = %handle.id.fmt_short(), "control queue full, dropping message");
            }
        }
    }

    /// Commits a peer's *complete* index (from a single `Index` or the final
    /// `IndexPart`): install the map, mark the peer synced, observe its leases,
    /// and reconcile. Marking `index_received` here — never mid-stream — is what
    /// keeps the freshness gate closed until the whole index has landed.
    fn commit_peer_index(
        &mut self,
        from: EndpointId,
        map: BTreeMap<RelPath, FileRecord>,
        leases: Vec<LeaseInfo>,
    ) {
        self.peer_index.insert(from, map);
        self.index_received.insert(from);
        let now = Instant::now();
        for lease in leases {
            match sanitize_rel_path(lease.path.as_str()) {
                Ok(clean) => self.locks.observe_lease(
                    &clean,
                    &lease.holder,
                    lease.lamport,
                    lease.expires_in_ms,
                    now,
                ),
                Err(e) => {
                    warn!(peer = %from.fmt_short(), "dropping lease with hostile path: {e}");
                }
            }
        }
        self.reconcile(from);
    }

    /// The connect-time index, split into frames each provably under
    /// `MAX_FRAME` (P20). A folder small enough to fit one frame yields exactly
    /// one `Msg::Index` (the unchanged pre-P20 wire); a large one yields ordered
    /// `Msg::IndexPart`s the peer reassembles.
    fn build_index_parts(&mut self) -> Vec<Msg> {
        let now = Instant::now();
        let leases: Vec<LeaseInfo> = self
            .locks
            .held_leases(now)
            .into_iter()
            .map(|(path, holder, lamport, left, _age)| LeaseInfo {
                path,
                holder,
                lamport,
                expires_in_ms: left.as_millis() as u64,
            })
            .collect();
        crate::proto::split_index_parts(self.state.lamport, &self.state.files, leases)
    }

    // ---------------- control messages ----------------

    #[instrument(skip(self, msg), fields(peer = %from.fmt_short(), kind = msg.kind()))]
    async fn on_ctl(&mut self, from: EndpointId, msg: Msg) {
        let from_str = from.to_string();
        if !self.peers.contains_key(&from) {
            return;
        }
        match msg {
            Msg::Hello { .. } | Msg::HelloAck { .. } | Msg::Proof { .. } => {
                // Pre-auth messages after authentication are a protocol
                // violation; drop the connection.
                warn!("handshake message on authenticated connection");
                if let Some(h) = self.peers.remove(&from) {
                    let conn_id = h.conn_id();
                    h.close();
                    self.on_peer_gone(from, conn_id);
                }
            }
            Msg::Index {
                lamport,
                files,
                leases,
            } => {
                self.state.lamport = self.state.lamport.max(lamport);
                // A single-frame index supersedes any partial sharded stream.
                self.index_staging.remove(&from);
                let mut map = BTreeMap::new();
                for (path, record) in files {
                    if !record_acceptable(&record) {
                        warn!(peer = %from.fmt_short(), path = %path, "dropping record with an oversized version vector");
                        continue;
                    }
                    match sanitize_rel_path(path.as_str()) {
                        Ok(clean) => {
                            map.insert(clean, record);
                        }
                        Err(e) => {
                            warn!(peer = %from.fmt_short(), path = %path, "dropping record with hostile path: {e}");
                        }
                    }
                }
                self.commit_peer_index(from, map, leases);
            }
            Msg::IndexPart {
                seq,
                last,
                lamport,
                files,
                leases,
            } => {
                self.state.lamport = self.state.lamport.max(lamport);
                let stage = self
                    .index_staging
                    .entry(from)
                    .or_insert((0, BTreeMap::new()));
                // seq must be contiguous 0,1,2,…; a gap/dup/reorder is hostile.
                // Bound total parts and staged entries (DoS): a peer cannot
                // stream forever or blow memory.
                if seq != stage.0
                    || seq >= crate::consts::MAX_INDEX_PARTS
                    || stage.1.len().saturating_add(files.len()) > crate::consts::MAX_INDEX_ENTRIES
                {
                    warn!(peer = %from.fmt_short(), seq, "bad/oversized index part; dropping peer");
                    self.index_staging.remove(&from);
                    if let Some(h) = self.peers.remove(&from) {
                        let conn_id = h.conn_id();
                        h.close();
                        self.on_peer_gone(from, conn_id);
                    }
                    return;
                }
                for (path, record) in files {
                    if !record_acceptable(&record) {
                        warn!(peer = %from.fmt_short(), path = %path, "dropping record with an oversized version vector");
                        continue;
                    }
                    match sanitize_rel_path(path.as_str()) {
                        Ok(clean) => {
                            stage.1.insert(clean, record);
                        }
                        Err(e) => {
                            warn!(peer = %from.fmt_short(), path = %path, "dropping record with hostile path: {e}");
                        }
                    }
                }
                stage.0 += 1;
                if !last {
                    return; // wait for more parts — no reconcile until complete
                }
                // Final part: promote the complete staged map and reconcile.
                let map = self
                    .index_staging
                    .remove(&from)
                    .map(|(_, m)| m)
                    .unwrap_or_default();
                self.commit_peer_index(from, map, leases);
            }
            Msg::FileMeta {
                path,
                record,
                lamport,
            } => {
                self.state.lamport = self.state.lamport.max(lamport);
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    warn!(peer = %from.fmt_short(), path = %path, "dropping file meta with hostile path");
                    return;
                };
                if !record_acceptable(&record) {
                    warn!(peer = %from.fmt_short(), path = %rel, "dropping file meta with an oversized version vector");
                    return;
                }
                self.peer_index
                    .entry(from)
                    .or_default()
                    .insert(rel.clone(), record.clone());
                self.reconcile_one(from, &rel, &record);
            }
            Msg::LockReq {
                path,
                lamport,
                ttl_ms,
            } => {
                self.state.lamport = self.state.lamport.max(lamport);
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    warn!(peer = %from.fmt_short(), "dropping lock request with hostile path");
                    return;
                };
                // P17: refuse a lease to a peer whose authenticated role may not
                // edit — enforced here, on the grantor, so it holds even if the
                // requester's own binary was modified to skip its local check.
                if !self.peer_may_lock(&from) {
                    warn!(peer = %from.fmt_short(), path = %rel, "denying lock: peer role may not edit");
                    if let Some(h) = self.peers.get(&from) {
                        h.send(Msg::LockDeny {
                            path: rel,
                            reason: DenyReason::RoleForbidden,
                        });
                    }
                    return;
                }
                let decision =
                    self.locks
                        .on_remote_request(&rel, &from_str, lamport, ttl_ms, Instant::now());
                let reply = match decision {
                    Decision::Grant => Msg::LockGrant { path: rel },
                    Decision::GrantAndAbortMine => {
                        let pending = self.pending_acquires.remove(&rel);
                        if let Some((q, nf)) = self.autolock_pending.remove(&rel) {
                            // Our autolock lost the tie: revert locally and keep
                            // our bytes in quarantine (Golden Invariant).
                            self.autolock_fail(&rel, q, nf, "LEASE".to_string());
                        } else if let Some((_, waiter)) = pending {
                            let _ = waiter.send(IpcResponse::err(
                                "tie_lost",
                                "a concurrent request won the tie-break",
                            ));
                        }
                        // Free partial grants we may have collected elsewhere.
                        self.broadcast(Msg::LockRelease { path: rel.clone() });
                        Msg::LockGrant { path: rel }
                    }
                    Decision::Deny(reason) => Msg::LockDeny { path: rel, reason },
                };
                if let Some(h) = self.peers.get(&from) {
                    h.send(reply);
                }
            }
            Msg::LockGrant { path } => {
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                if self
                    .locks
                    .on_grant(&rel, &from_str, Instant::now())
                    .is_some()
                {
                    let abs = rel.to_fs_path(&self.dir);
                    if let Err(e) = guard::set_writable(&abs) {
                        warn!(path = %rel, "could not make leased file writable: {e}");
                    }
                    // We now hold it: drop any waitlist bookkeeping for the path.
                    self.my_waits.remove(&rel);
                    self.interest.remove(&rel);
                    let was_autolock = self.autolock_succeed(&rel);
                    if let Some((_, waiter)) = self.pending_acquires.remove(&rel) {
                        let _ = waiter.send(IpcResponse::ok(serde_json::json!({
                            "locked": rel.as_str(),
                            "ttl_ms": self.timings.ttl.as_millis() as u64,
                        })));
                    }
                    let kind = if was_autolock { "autolock" } else { "lock" };
                    if !was_autolock {
                        info!(path = %rel, "lease acquired");
                    }
                    self.observe(kind, Some(rel.as_str()), None, None);
                }
            }
            Msg::LockDeny { path, reason } => {
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                if self.locks.on_deny(&rel) {
                    self.broadcast(Msg::LockRelease { path: rel.clone() });
                    if let Some((q, nf)) = self.autolock_pending.remove(&rel) {
                        // Autolock lost the lease (held elsewhere / tie): revert.
                        self.pending_acquires.remove(&rel);
                        self.autolock_fail(&rel, q, nf, "LEASE".to_string());
                        return;
                    }
                    if let Some((_, waiter)) = self.pending_acquires.remove(&rel) {
                        let now = Instant::now();
                        let voter = self.peer_diag(&from, now, "voter", Some(true));
                        let response = match reason {
                            DenyReason::Held { by } => self.lock_error(
                                "lease_held",
                                format!("lease is held by {by}"),
                                "LEASE",
                                vec![voter],
                                "another peer already holds this lease; wait for it to unlock",
                            ),
                            DenyReason::TieLost => self.lock_error(
                                "tie_lost",
                                "a concurrent request won the tie-break",
                                "LEASE",
                                vec![voter],
                                "another node requested the same path first; retry in a moment",
                            ),
                            DenyReason::Unavailable => self.lock_error(
                                "unavailable",
                                "the peer is at its tracked-lease capacity",
                                "LEASE",
                                vec![voter],
                                "the responding peer is tracking too many leases; retry later",
                            ),
                            DenyReason::RoleForbidden => self.lock_error(
                                "role_forbidden",
                                "a peer refused the lease: this node's role may not edit",
                                "LEASE",
                                vec![voter],
                                "this node joined as viewer/archive; it can sync and read but \
                                 not lock. Re-join with an editor invite to gain edit rights.",
                            ),
                        };
                        let _ = waiter.send(response);
                    }
                }
            }
            Msg::LockRelease { path } => {
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                self.locks.on_release(&rel, &from_str);
            }
            Msg::LockRenew {
                path,
                lamport,
                ttl_ms,
            } => {
                self.state.lamport = self.state.lamport.max(lamport);
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                self.locks.on_renew(&rel, &from_str, ttl_ms, Instant::now());
            }
            Msg::Bye => {
                if let Some(conn_id) = self.peers.get(&from).map(|h| h.conn_id()) {
                    self.on_peer_gone(from, conn_id);
                }
            }
            Msg::LockInterest { path } => {
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                // DoS bound: cap the number of distinct waitlisted paths so a
                // peer cannot grow the interest map without limit.
                if !self.interest.contains_key(&rel)
                    && self.interest.len() >= crate::consts::MAX_WAITLIST_ENTRIES
                {
                    debug!(peer = %from.fmt_short(), "waitlist at capacity; ignoring LockInterest");
                    return;
                }
                self.interest
                    .entry(rel.clone())
                    .or_default()
                    .insert(from_str.clone());
                debug!(path = %rel, peer = %from.fmt_short(), "peer waitlisted this path");
            }
            Msg::LockFreed { path } => {
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                self.interest.remove(&rel);
                // The waiting CLI re-attempts the acquire on its next poll; note
                // the free so `status` reflects it and logs show the handoff.
                if self.my_waits.contains_key(&rel) {
                    self.push_event(format!("{rel} freed — re-attempting lock"));
                }
            }
            Msg::Identity { grant } => self.on_peer_identity(from, grant),
        }
    }

    /// Records a peer's role from its signed `Identity` grant (P17). The grant is
    /// verified against this session's admin public key and rejected if expired;
    /// an invalid or expired grant leaves the peer with no recorded editor role,
    /// so an enforcing grantor denies its locks. No-op on a legacy session.
    fn on_peer_identity(&mut self, from: EndpointId, grant: crate::session::SignedGrant) {
        let Some(admin_pub) = self.state.admin_public_bytes() else {
            return; // legacy session: no enforcement, ignore grants
        };
        if !grant.verify(&admin_pub) {
            warn!(peer = %from.fmt_short(), "rejecting Identity: grant signature invalid");
            self.peer_roles.remove(&from);
            return;
        }
        if grant.grant.is_expired(crate::now_ms()) {
            warn!(peer = %from.fmt_short(), "rejecting Identity: grant expired");
            self.peer_roles.remove(&from);
            return;
        }
        let role = grant.grant.role;
        debug!(peer = %from.fmt_short(), role = %crate::session::role_name(role), "peer role recorded");
        self.peer_roles.insert(from, role);
    }

    /// P17 mesh enforcement: whether `from`'s authenticated role may take a
    /// lease. On a legacy session every peer may (returns true). On a v2 session
    /// a peer may only if it advertised a verified editor grant — a viewer, an
    /// archive, or a peer that never sent a valid `Identity` is refused, so a
    /// modified binary cannot obtain a lease an honest grantor will not give.
    fn peer_may_lock(&self, from: &EndpointId) -> bool {
        if !self.state.enforcing_roles() {
            return true;
        }
        self.peer_roles
            .get(from)
            .copied()
            .is_some_and(crate::session::role_can_edit)
    }

    fn reconcile(&mut self, from: EndpointId) {
        let Some(remote) = self.peer_index.get(&from) else {
            return;
        };
        let remote_vec: Vec<(RelPath, FileRecord)> =
            remote.iter().map(|(p, r)| (p.clone(), r.clone())).collect();
        let d = diff(&self.state.files, &remote_vec);
        let records: BTreeMap<RelPath, FileRecord> = remote_vec.into_iter().collect();
        // Retire any unapplied markers the origin has since deleted (a tombstone
        // for an unapplied path is dropped by `diff`, so handle it directly).
        if !self.state.unapplied.is_empty() {
            let deleted: Vec<RelPath> = self
                .state
                .unapplied
                .keys()
                .filter(|rel| records.get(*rel).is_some_and(|r| r.deleted))
                .cloned()
                .collect();
            for rel in deleted {
                if let Some(rec) = records.get(&rel) {
                    self.maybe_clear_unapplied(&rel, rec);
                }
            }
        }
        for rel in d.pull {
            if let Some(rec) = records.get(&rel) {
                self.maybe_pull(&rel, from, rec.clone());
            }
        }
        for rel in d.conflicts {
            if let Some(rec) = records.get(&rel) {
                self.on_concurrent(&rel, from, rec.clone());
            }
        }
    }

    /// Clears a non-portable `unapplied` marker when the origin deletes the
    /// path. Such a path is absent from `state.files`, so the normal
    /// tombstone-vs-index reconciliation skips it — this is the only place the
    /// marker is retired on deletion.
    fn maybe_clear_unapplied(&mut self, rel: &RelPath, record: &FileRecord) {
        if record.deleted && self.state.unapplied.remove(rel).is_some() {
            self.persist();
            self.push_event(format!(
                "unapplied non-portable path removed upstream: {rel}"
            ));
            info!(path = %rel, "unapplied path tombstoned upstream; marker cleared");
        }
    }

    fn reconcile_one(&mut self, from: EndpointId, rel: &RelPath, record: &FileRecord) {
        self.maybe_clear_unapplied(rel, record);
        match self.state.files.get(rel) {
            None => {
                if !record.deleted {
                    self.maybe_pull(rel, from, record.clone());
                }
            }
            Some(mine) => match vclock::compare(&mine.vv, &record.vv) {
                Causality::Before => self.maybe_pull(rel, from, record.clone()),
                Causality::Concurrent => self.on_concurrent(rel, from, record.clone()),
                Causality::Equal | Causality::After => {}
            },
        }
    }

    /// Concurrent version vectors are impossible under strict locking, so
    /// they signal external tampering. Deterministic recovery: the higher
    /// endpoint id re-asserts its copy with a dominating clock; the lower one
    /// quarantines its bytes and pulls the winner. Nothing is merged, nothing
    /// is silently lost.
    fn on_concurrent(&mut self, rel: &RelPath, from: EndpointId, record: FileRecord) {
        warn!(path = %rel, peer = %from.fmt_short(), "concurrent version vectors — external tampering suspected");
        let from_str = from.to_string();
        if self.me_str > from_str {
            if let Some(mine) = self.state.files.get(rel).cloned() {
                let mut vv = vclock::merge(&mine.vv, &record.vv);
                vclock::inc(&mut vv, &self.me_str);
                let mut rec = mine;
                rec.vv = vv;
                rec.updated_at_ms = crate::now_ms();
                self.state.files.insert(rel.clone(), rec.clone());
                self.state.lamport += 1;
                self.persist();
                self.broadcast(Msg::FileMeta {
                    path: rel.clone(),
                    record: rec,
                    lamport: self.state.lamport,
                });
            }
        } else {
            let abs = rel.to_fs_path(&self.dir);
            if abs.is_file() {
                match guard::quarantine(&self.dir, rel, "concurrent-versions") {
                    Ok(q) => {
                        warn!(path = %rel, "local copy quarantined at {}", q.display());
                        self.observe(
                            "quarantine",
                            Some(rel.as_str()),
                            None,
                            Some("concurrent-versions".into()),
                        );
                    }
                    Err(e) => warn!(path = %rel, "quarantine failed: {e}"),
                }
            }
            self.maybe_pull(rel, from, record);
        }
    }

    // ---------------- pulls ----------------

    /// The portability verdict for a remote path on this node: the pure
    /// character/device-name rules plus the stateful NTFS case-fold check —
    /// a path that differs from an already-indexed live path only by case
    /// would silently overwrite it on a case-insensitive filesystem.
    fn portability_reason(&self, rel: &RelPath) -> Option<String> {
        if let Some(reason) = crate::sync::index::portability_violation(rel.as_str()) {
            return Some(reason);
        }
        let folded = rel.as_str().to_lowercase();
        self.state
            .files
            .iter()
            .find(|(p, r)| !r.deleted && *p != rel && p.as_str().to_lowercase() == folded)
            .map(|(p, _)| {
                format!("case-fold collision with already-indexed {p:?} (NTFS is case-insensitive)")
            })
    }

    fn maybe_pull(&mut self, rel: &RelPath, from: EndpointId, record: FileRecord) {
        if self.locks.is_held_by_me(rel) {
            return;
        }
        if record.deleted {
            // A tombstone settles an unapplied path too: the record is gone
            // upstream, so drop the marker.
            if self.state.unapplied.remove(rel).is_some() {
                self.persist();
                info!(path = %rel, "unapplied non-portable path deleted upstream; marker cleared");
            }
            self.apply_remote(rel.clone(), from, record, None);
            return;
        }
        // P11 sync-scope gate: a NEW remote path outside this node's scope is
        // acknowledged but never materialized — same held-not-dropped shape as
        // the portability gate below. Paths already carried keep pulling
        // updates (holding those would wedge FRESHNESS for every editor).
        if !self.state.files.get(rel).is_some_and(|r| !r.deleted) {
            let verdict = self.ignore.verdict(rel, Some(record.size));
            if !verdict.is_sync() {
                let reason = verdict.reason();
                let changed = self
                    .state
                    .unapplied
                    .get(rel)
                    .is_none_or(|e| e.record.vv != record.vv);
                if changed {
                    warn!(
                        path = %rel,
                        reason = %reason,
                        "HELD: remote record is outside this node's sync scope; record stored, file NOT materialized"
                    );
                    self.push_event(format!("held ({reason}): {rel}"));
                    self.state
                        .unapplied
                        .insert(rel.clone(), crate::state::UnappliedEntry { record, reason });
                    self.persist();
                }
                return;
            }
        }
        // Portability gate: a path that cannot exist on this filesystem is
        // acknowledged but never materialized (Windows), or applied with a
        // loud warning (Unix). The stored record keeps the sync loop settled —
        // no re-pull churn — and never wedges anything else.
        if let Some(reason) = self.portability_reason(rel) {
            if cfg!(windows) {
                let changed = self
                    .state
                    .unapplied
                    .get(rel)
                    .is_none_or(|e| e.record.vv != record.vv);
                if changed {
                    warn!(
                        path = %rel,
                        reason = %reason,
                        "UNAPPLIED: remote file has a non-portable path; record stored, file NOT materialized"
                    );
                    self.push_event(format!("unapplied (non-portable path): {rel}"));
                    self.state
                        .unapplied
                        .insert(rel.clone(), crate::state::UnappliedEntry { record, reason });
                    self.persist();
                }
                return;
            }
            if self.warned_nonportable.insert(rel.clone()) {
                warn!(
                    path = %rel,
                    reason = %reason,
                    "path is not portable to Windows nodes (applied here; Windows members will hold it unapplied)"
                );
            }
        }
        if let Some(job) = self.pending_pulls.get_mut(rel) {
            if job.record.manifest == record.manifest && job.record.vv == record.vv {
                return; // duplicate
            }
            job.queued = Some((from, record));
            return;
        }
        // DoS bound: cap concurrently running pulls. Excess new paths wait in a
        // bounded backlog and start as running pulls complete (finish_pull
        // drains it); a dropped-at-backlog-cap path stays in the peer index, so
        // FRESHNESS still gates edits and the peer re-advertises it.
        if self.pending_pulls.len() >= crate::consts::MAX_CONCURRENT_PULLS {
            self.enqueue_pull_backlog(rel.clone(), from, record);
            return;
        }
        self.pending_pulls.insert(
            rel.clone(),
            PullJob {
                from,
                record: record.clone(),
                attempts: 0,
                queued: None,
            },
        );
        self.start_pull(rel.clone(), from, record);
    }

    /// Adds (or refreshes) a path in the bounded pull backlog. Dedups by path
    /// so a churning advertiser cannot enqueue the same path repeatedly; drops
    /// the record if the backlog is at [`crate::consts::MAX_PULL_BACKLOG`].
    fn enqueue_pull_backlog(&mut self, rel: RelPath, from: EndpointId, record: FileRecord) {
        if let Some(slot) = self.pull_backlog.iter_mut().find(|(p, _, _)| *p == rel) {
            *slot = (rel, from, record);
        } else if self.pull_backlog.len() < crate::consts::MAX_PULL_BACKLOG {
            self.pull_backlog.push_back((rel, from, record));
        } else {
            debug!(path = %rel, "pull backlog full; dropping advertised record (stays gated by the peer index)");
        }
    }

    /// P15 priority lanes: the highest-priority backlogged pull to admit next —
    /// a path a local lock is blocked on comes first (the user is waiting on
    /// it), then the smallest file, so a folder feels alive while a huge asset
    /// streams in the background.
    fn pick_backlog_index(&self) -> Option<usize> {
        self.pull_backlog
            .iter()
            .enumerate()
            .min_by_key(|(_, (rel, _, rec))| {
                let waiting =
                    self.my_waits.contains_key(rel) || self.pending_acquires.contains_key(rel);
                (!waiting, rec.size)
            })
            .map(|(i, _)| i)
    }

    /// Starts backlogged pulls while there is capacity (a completed pull just
    /// freed a slot), highest-priority first. Skips paths that became pending
    /// or self-held meanwhile.
    fn drain_pull_backlog(&mut self) {
        while self.pending_pulls.len() < crate::consts::MAX_CONCURRENT_PULLS {
            let Some(idx) = self.pick_backlog_index() else {
                break;
            };
            let Some((rel, from, record)) = self.pull_backlog.remove(idx) else {
                break;
            };
            if self.pending_pulls.contains_key(&rel) || self.locks.is_held_by_me(&rel) {
                continue;
            }
            self.pending_pulls.insert(
                rel.clone(),
                PullJob {
                    from,
                    record: record.clone(),
                    attempts: 0,
                    queued: None,
                },
            );
            self.start_pull(rel, from, record);
        }
    }

    /// Every connected peer advertising this exact version can serve its
    /// chunks (same manifest ⇒ same chunks). `primary` first, then the rest —
    /// pull_stage swarms across up to `SWARM_PEERS` of them.
    fn swarm_peers(
        &self,
        rel: &RelPath,
        record: &FileRecord,
        primary: EndpointId,
    ) -> Vec<EndpointAddr> {
        let addr_of = |id: EndpointId| {
            self.known_member_addr(&id)
                .unwrap_or_else(|| EndpointAddr::new(id))
        };
        let mut out = vec![addr_of(primary)];
        for (id, files) in &self.peer_index {
            if *id == primary || !self.peers.contains_key(id) {
                continue;
            }
            if files
                .get(rel)
                .is_some_and(|r| !r.deleted && r.manifest == record.manifest)
            {
                out.push(addr_of(*id));
            }
        }
        out
    }

    fn start_pull(&mut self, rel: RelPath, from: EndpointId, record: FileRecord) {
        let transfer = self.transfer.clone();
        let endpoint = self.endpoint.clone();
        let events = self.events_tx.clone();
        let limiter = self.limiter.clone();
        let froms = self.swarm_peers(&rel, &record, from);
        let meter = self.ui.pull_meter(rel.as_str(), record.size);
        self.pull_meters.insert(rel.clone(), meter.clone());
        // Resume: persist the in-flight target so its already-fetched chunks
        // survive GC across a restart (the store skips chunks it has, so the
        // re-triggered pull continues instead of restarting).
        if self.state.pulling.get(&rel) != Some(&record) {
            self.state.pulling.insert(rel.clone(), record.clone());
            self.persist();
        }
        tokio::spawn(async move {
            let result = transfer
                .pull_stage(&endpoint, &froms, &rel, &record, Some(meter), &limiter)
                .await;
            let _ = events
                .send(Event::PullStaged {
                    rel,
                    from,
                    record,
                    result,
                })
                .await;
        });
    }

    fn on_pull_staged(
        &mut self,
        rel: RelPath,
        from: EndpointId,
        record: FileRecord,
        result: Result<Staged, TransferError>,
    ) {
        match result {
            Ok(staged) => {
                self.apply_remote(rel, from, record, Some(staged));
            }
            Err(e) => {
                warn!(path = %rel, peer = %from.fmt_short(), "pull failed: {e}");
                let retry = match self.pending_pulls.get_mut(&rel) {
                    Some(job) => {
                        job.attempts += 1;
                        job.attempts < 5 && self.peers.contains_key(&from)
                    }
                    None => false,
                };
                if retry {
                    let events = self.events_tx.clone();
                    let rel2 = rel.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        let _ = events.send(Event::PullRetry { rel: rel2 }).await;
                    });
                } else {
                    warn!(path = %rel, "giving up on pull; freshness stays gated by the peer index");
                    self.finish_pull(&rel);
                }
            }
        }
    }

    /// Applies a remote record: either a staged content file or a tombstone.
    fn apply_remote(
        &mut self,
        rel: RelPath,
        from: EndpointId,
        record: FileRecord,
        staged: Option<Staged>,
    ) {
        if self.busy.contains(&rel) {
            self.deferred.insert(
                rel,
                DeferredApply {
                    from,
                    record,
                    staged,
                },
            );
            return;
        }
        // Re-check causality against what we hold *now*.
        if let Some(mine) = self.state.files.get(&rel) {
            match vclock::compare(&mine.vv, &record.vv) {
                Causality::Equal | Causality::After => {
                    self.finish_pull(&rel);
                    return;
                }
                Causality::Before | Causality::Concurrent => {}
            }
        }
        let abs = rel.to_fs_path(&self.dir);
        // Golden Invariant: a synced file is read-only (0444), so a *writable*
        // file on disk carries an un-leased local edit. Preserve those bytes in
        // quarantine before the incoming version overwrites or deletes them —
        // this closes the tight race where the local write's watcher event has
        // not fired yet (or is about to be swallowed by this apply's mute), e.g.
        // both nodes autolock-writing the same path at once.
        if std::fs::metadata(&abs)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
        {
            match guard::quarantine(&self.dir, &rel, "edit-vs-remote") {
                Ok(q) => warn!(
                    path = %rel,
                    quarantine = %q.display(),
                    "preserved an un-leased local edit before applying the remote version"
                ),
                Err(e) => warn!(path = %rel, "could not preserve local edit before apply: {e}"),
            }
        }
        self.mute(&rel);
        let prev = self.state.files.get(&rel).cloned();
        if let Some(prev) = &prev {
            versions::push(&mut self.state, &rel, prev);
        }
        let merged_vv = match &prev {
            Some(p) => vclock::merge(&p.vv, &record.vv),
            None => record.vv.clone(),
        };
        // Strict mode (or a non-editor role) clamps the applied file
        // read-only; easy mode on an editor leaves it writable for in-place
        // edits without a lease.
        let strict = self.state.config.enforce_readonly();
        let apply_result: Result<(), String> = (|| {
            if record.deleted {
                // Ordering (Windows refuses to delete read-only files):
                // clear read-only → delete-with-retry; NotFound is success.
                let _ = guard::set_writable(&abs);
                crate::win_fs::remove_file(&abs).map_err(|e| e.to_string())?;
                Ok(())
            } else {
                let staged = staged.ok_or_else(|| "missing staged file".to_string())?;
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                // Ordering: clear read-only (Windows cannot rename over a
                // read-only file) → rename-over with retry → re-apply guard.
                let _ = guard::set_writable(&abs);
                crate::win_fs::persist_temp(staged.temp, &abs).map_err(|e| e.to_string())?;
                if strict {
                    guard::set_readonly(&abs).map_err(|e| e.to_string())?;
                }
                drop(staged.tags);
                Ok(())
            }
        })();
        match apply_result {
            Ok(()) => {
                let mut rec = record;
                rec.vv = merged_vv;
                let deleted = rec.deleted;
                self.state.files.insert(rel.clone(), rec);
                // Resume bookkeeping: the record is committed, so its chunks are
                // now GC-protected by state.files — drop the in-flight marker.
                self.state.pulling.remove(&rel);
                self.persist();
                info!(path = %rel, peer = %from.fmt_short(), deleted, "applied remote version");
                self.observe(
                    "pull-applied",
                    Some(rel.as_str()),
                    Some(&from.to_string()),
                    Some(if deleted {
                        "deleted".into()
                    } else {
                        "updated".into()
                    }),
                );
                // A settled ignore file re-applies the session's shared rules.
                if rel.as_str() == crate::sync::ignore::IGNORE_FILE {
                    self.rebuild_ignore();
                }
            }
            Err(e) => {
                error!(path = %rel, "failed to apply remote version: {e}");
            }
        }
        self.finish_pull(&rel);
    }

    fn finish_pull(&mut self, rel: &RelPath) {
        if let Some(mut job) = self.pending_pulls.remove(rel)
            && let Some((from, record)) = job.queued.take()
        {
            self.pending_pulls.insert(
                rel.clone(),
                PullJob {
                    from,
                    record: record.clone(),
                    attempts: 0,
                    queued: None,
                },
            );
            self.start_pull(rel.clone(), from, record);
        }
        // A slot may have freed; admit any waiting backlogged pulls.
        self.drain_pull_backlog();
    }

    // ---------------- watch pipeline ----------------

    fn mute(&mut self, rel: &RelPath) {
        self.muted.insert(rel.clone(), Instant::now());
    }

    fn is_muted(&mut self, rel: &RelPath) -> bool {
        let now = Instant::now();
        self.muted
            .retain(|_, t| now.duration_since(*t) < MUTE_WINDOW);
        self.muted.contains_key(rel)
    }

    fn on_watch(&mut self, ev: WatchEvent) {
        let rel = ev.rel;
        if self.is_muted(&rel) {
            return;
        }
        if self.busy.contains(&rel) {
            self.recheck.insert(rel);
            return;
        }
        // The ignore file is the session's shared contract: any settle of it
        // re-applies the rules immediately (and may release held records).
        if rel.as_str() == crate::sync::ignore::IGNORE_FILE {
            self.rebuild_ignore();
        } else if !self.state.files.get(&rel).is_some_and(|r| !r.deleted) {
            // P11 sync-scope gate — for paths NOT already carried: a held
            // path is left exactly where it is (no publish, no quarantine, no
            // read-only clamp). Already-indexed paths stay fully governed so
            // the strict guarantees never silently lapse for session content.
            let size = std::fs::metadata(rel.to_fs_path(&self.dir))
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            let verdict = self.ignore.verdict(&rel, size);
            if !verdict.is_sync() {
                debug!(path = %rel, reason = %verdict.reason(), "outside sync scope; left alone");
                if size.is_some() {
                    self.held_local.insert(rel, verdict.reason());
                } else {
                    self.held_local.remove(&rel);
                }
                return;
            }
            self.held_local.remove(&rel);
        }
        debug!(path = %rel, kind = ?ev.kind, "watch event");
        self.inspect(rel, InspectCause::Watch);
    }

    /// Spawns the disk-vs-index comparison off the actor thread.
    fn inspect(&mut self, rel: RelPath, cause: InspectCause) {
        self.busy.insert(rel.clone());
        let record = self.state.files.get(&rel).cloned();
        let transfer = self.transfer.clone();
        let dir = self.dir.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let abs = rel.to_fs_path(&dir);
            let exists = abs.is_file();
            let outcome = match (&record, exists) {
                (None, false) => Ok(Inspection::Clean),
                (None, true) => Ok(Inspection::Unindexed),
                (Some(rec), false) => {
                    if rec.deleted {
                        Ok(Inspection::Clean)
                    } else {
                        Ok(Inspection::MissingIndexed)
                    }
                }
                (Some(rec), true) => {
                    if rec.deleted {
                        Ok(Inspection::Unindexed)
                    } else {
                        transfer.disk_matches(&rel, rec).await.map(|same| {
                            if same {
                                Inspection::Clean
                            } else {
                                Inspection::Differs
                            }
                        })
                    }
                }
            };
            let _ = events
                .send(Event::Inspected {
                    rel,
                    outcome,
                    cause,
                })
                .await;
        });
    }

    fn on_inspected(
        &mut self,
        rel: RelPath,
        outcome: Result<Inspection, TransferError>,
        cause: InspectCause,
    ) {
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => {
                warn!(path = %rel, "inspection failed: {e}");
                if let InspectCause::Unlock(reply) = cause {
                    let _ = reply.send(IpcResponse::err("inspect_failed", e.to_string()));
                }
                self.unbusy(&rel);
                return;
            }
        };
        let leased = self.locks.is_held_by_me(&rel);
        match cause {
            InspectCause::Unlock(reply) => match outcome {
                Inspection::Clean => {
                    self.unbusy(&rel);
                    self.finish_unlock(&rel, true, reply);
                }
                Inspection::Differs | Inspection::Unindexed => {
                    self.spawn_publish(rel, PublishCause::Unlock(reply));
                }
                Inspection::MissingIndexed => {
                    self.commit_tombstone(&rel);
                    self.unbusy(&rel);
                    self.finish_unlock(&rel, true, reply);
                }
            },
            InspectCause::Watch => match (leased, outcome) {
                (_, Inspection::Clean) => self.unbusy(&rel),
                (true, Inspection::Differs) | (true, Inspection::Unindexed) => {
                    // Each write to an autolock-held lease resets its idle timer.
                    if let Some(d) = self.autolock_idle.get_mut(&rel) {
                        *d = Instant::now() + crate::consts::AUTOLOCK_IDLE_RELEASE;
                    }
                    self.spawn_publish(rel, PublishCause::Edit);
                }
                (true, Inspection::MissingIndexed) => {
                    self.commit_tombstone(&rel);
                    self.unbusy(&rel);
                }
                // Un-leased write on a *free* path with autolock on: try to
                // acquire instead of reverting. A path held by someone else, or
                // an un-leased delete, always takes the normal violation path.
                (false, Inspection::Differs) if self.autolock_eligible(&rel) => {
                    self.spawn_autolock(rel, false);
                }
                (false, Inspection::Unindexed) if self.autolock_eligible(&rel) => {
                    self.spawn_autolock(rel, true);
                }
                (false, Inspection::Differs) => self.spawn_violation(rel, true, "forced-write"),
                (false, Inspection::MissingIndexed) => self.spawn_violation(rel, false, ""),
                (false, Inspection::Unindexed) => {
                    self.violation_new_file(&rel);
                    self.unbusy(&rel);
                }
            },
        }
    }

    fn unbusy(&mut self, rel: &RelPath) {
        self.busy.remove(rel);
        if let Some(d) = self.deferred.remove(rel) {
            self.apply_remote(rel.clone(), d.from, d.record, d.staged);
        }
        if self.recheck.remove(rel) {
            self.inspect(rel.clone(), InspectCause::Watch);
        }
    }

    // ---------------- publishing (leased edits) ----------------

    fn spawn_publish(&mut self, rel: RelPath, cause: PublishCause) {
        self.busy.insert(rel.clone());
        let transfer = self.transfer.clone();
        let events = self.events_tx.clone();
        // A spinner is only worth showing when re-chunking takes real time.
        const SPINNER_MIN_BYTES: u64 = 8 * 1024 * 1024;
        let spinner = std::fs::metadata(rel.to_fs_path(&self.dir))
            .ok()
            .filter(|m| m.len() >= SPINNER_MIN_BYTES)
            .map(|_| self.ui.publish_spinner(rel.as_str()));
        tokio::spawn(async move {
            let result = transfer.publish_local(&rel).await;
            drop(spinner);
            let _ = events.send(Event::PublishDone { rel, result, cause }).await;
        });
    }

    fn on_publish_done(
        &mut self,
        rel: RelPath,
        result: Result<Published, TransferError>,
        cause: PublishCause,
    ) {
        let leased = self.locks.is_held_by_me(&rel);
        let allowed = leased || matches!(cause, PublishCause::Import);
        let published = match result {
            Ok(p) if allowed => Some(p),
            Ok(_) => {
                warn!(path = %rel, "lease lost before edit committed; discarding publish");
                None
            }
            Err(e) => {
                warn!(path = %rel, "publish failed: {e}");
                if matches!(cause, PublishCause::Edit) {
                    // The file may have changed mid-chunking; look again.
                    self.recheck.insert(rel.clone());
                }
                None
            }
        };
        // Whether an edit actually reached the mesh — an Unlock reports this so
        // the resolve flow never discards a quarantine copy on a failed publish.
        let publish_ok = published.is_some();
        if let Some(p) = published {
            let prev = self.state.files.get(&rel).cloned();
            let unchanged = prev
                .as_ref()
                .is_some_and(|r| !r.deleted && r.manifest == p.manifest && r.size == p.size);
            if !unchanged {
                let mut vv = prev.as_ref().map(|r| r.vv.clone()).unwrap_or_default();
                vclock::inc(&mut vv, &self.me_str);
                if let Some(prev) = &prev {
                    versions::push(&mut self.state, &rel, prev);
                }
                let record = FileRecord {
                    size: p.size,
                    manifest: p.manifest,
                    vv,
                    deleted: false,
                    updated_at_ms: crate::now_ms(),
                };
                self.state.files.insert(rel.clone(), record.clone());
                self.state.lamport += 1;
                self.persist();
                self.broadcast(Msg::FileMeta {
                    path: rel.clone(),
                    record,
                    lamport: self.state.lamport,
                });
                info!(path = %rel, "published local edit");
                self.observe("publish", Some(rel.as_str()), None, None);
            }
            drop(p.tags);
            // Strict checkout applies to the importer too: an imported
            // (genesis) file has no lease, so it goes read-only the moment its
            // publish lands — not only after a restart's enforce_all. Edit
            // stays writable (lease held); Unlock is handled in finish_unlock.
            if matches!(cause, PublishCause::Import) {
                let p = rel.to_fs_path(&self.dir);
                let settle = if self.state.config.enforce_readonly() {
                    guard::set_readonly(&p)
                } else {
                    guard::set_writable(&p)
                };
                if let Err(e) = settle {
                    warn!(path = %rel, "could not apply post-import permissions: {e}");
                }
            }
        }
        if let PublishCause::Unlock(reply) = cause {
            self.unbusy(&rel);
            self.finish_unlock(&rel, publish_ok, reply);
            return;
        }
        self.unbusy(&rel);
    }

    /// A delete under a self-held lease becomes a tombstone broadcast.
    fn commit_tombstone(&mut self, rel: &RelPath) {
        let prev = self.state.files.get(rel).cloned();
        let Some(prev_rec) = prev else {
            return;
        };
        if prev_rec.deleted {
            return;
        }
        versions::push(&mut self.state, rel, &prev_rec);
        let mut vv = prev_rec.vv.clone();
        vclock::inc(&mut vv, &self.me_str);
        let record = FileRecord::tombstone(vv, crate::now_ms());
        self.state.files.insert(rel.clone(), record.clone());
        self.state.lamport += 1;
        self.persist();
        self.broadcast(Msg::FileMeta {
            path: rel.clone(),
            record,
            lamport: self.state.lamport,
        });
        info!(path = %rel, "deleted under lease; tombstone broadcast");
    }

    // ---------------- violations ----------------

    /// Un-leased write or delete: quarantine any offending bytes, then
    /// restore the newest indexed version. Violating content is NEVER
    /// broadcast. `reason` is the conflicts-index slug recorded with the
    /// quarantined copy (only read when `quarantine_first`).
    fn spawn_violation(&mut self, rel: RelPath, quarantine_first: bool, reason: &'static str) {
        let Some(record) = self.state.files.get(&rel).cloned() else {
            self.unbusy(&rel);
            return;
        };
        self.busy.insert(rel.clone());
        let transfer = self.transfer.clone();
        let dir = self.dir.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let quarantined = if quarantine_first {
                match guard::quarantine(&dir, &rel, reason) {
                    Ok(q) => Some(q),
                    Err(e) => {
                        warn!(path = %rel, "quarantine failed: {e}");
                        None
                    }
                }
            } else {
                None
            };
            let result = transfer.materialize(&record.manifest, record.size).await;
            let _ = events
                .send(Event::ViolationStaged {
                    rel,
                    result,
                    quarantined,
                    preserve_required: quarantine_first,
                })
                .await;
        });
    }

    fn on_violation_staged(
        &mut self,
        rel: RelPath,
        result: Result<Staged, TransferError>,
        quarantined: Option<PathBuf>,
        preserve_required: bool,
    ) {
        // Golden Invariant: if there were offending bytes to preserve and the
        // quarantine failed, do NOT restore over them — leave the user's bytes
        // in place (writable) and say so loudly. A later pass retries once the
        // underlying condition (disk space, permissions) clears.
        if preserve_required && quarantined.is_none() {
            error!(
                path = %rel,
                "violation NOT reverted: preserving the offending bytes failed, \
                 so the restore was skipped to avoid destroying them"
            );
            self.unbusy(&rel);
            return;
        }
        match result {
            Ok(staged) => {
                let abs = rel.to_fs_path(&self.dir);
                self.mute(&rel);
                let strict = self.state.config.enforce_readonly();
                let restore: Result<(), String> = (|| {
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    let _ = guard::set_writable(&abs);
                    crate::win_fs::persist_temp(staged.temp, &abs).map_err(|e| e.to_string())?;
                    if strict {
                        guard::set_readonly(&abs).map_err(|e| e.to_string())?;
                    }
                    drop(staged.tags);
                    Ok(())
                })();
                match restore {
                    Ok(()) => {
                        let had_quarantine = quarantined.is_some();
                        let q = quarantined
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "-".to_string());
                        warn!(
                            path = %rel,
                            quarantine = %q,
                            "VIOLATION: un-leased change reverted; offending bytes quarantined. \
                             Lease the file first: tazamun lock {rel}"
                        );
                        if had_quarantine {
                            self.observe(
                                "quarantine",
                                Some(rel.as_str()),
                                None,
                                Some("un-leased edit".into()),
                            );
                        }
                    }
                    Err(e) => error!(path = %rel, "violation restore failed: {e}"),
                }
            }
            Err(e) => {
                error!(path = %rel, "violation restore could not materialize: {e}");
            }
        }
        self.unbusy(&rel);
    }

    /// A new file created without a lease: quarantine it, remove it, and tell
    /// the user how to do it properly.
    fn violation_new_file(&mut self, rel: &RelPath) {
        let abs = rel.to_fs_path(&self.dir);
        let quarantined = match guard::quarantine(&self.dir, rel, "new-file") {
            Ok(q) => q.display().to_string(),
            Err(e) => {
                warn!(path = %rel, "quarantine failed: {e}");
                return; // never delete bytes we could not preserve
            }
        };
        self.mute(rel);
        // Ordering: clear a possible read-only attribute first (Windows
        // refuses to delete read-only files), then delete with retry.
        let _ = guard::set_writable(&abs);
        if let Err(e) = crate::win_fs::remove_file(&abs) {
            warn!(path = %rel, "could not remove un-leased new file: {e}");
        }
        warn!(
            path = %rel,
            quarantine = %quarantined,
            "VIOLATION: new file created without a lease was quarantined and removed. \
             Lock the path first: tazamun lock {rel}"
        );
    }

    // ---------------- autolock (opt-in auto-lock-on-first-write) ----------------

    /// Extracts the blocked-precondition label from a diagnosed lock error.
    fn precondition_of(resp: &IpcResponse) -> String {
        resp.data
            .as_ref()
            .and_then(|d| d["diagnosis"]["precondition"].as_str())
            .unwrap_or("PRECONDITION")
            .to_string()
    }

    /// Autolock applies only when it is enabled and the path is currently free
    /// (a path held by someone else, or already mid-autolock, is not eligible).
    fn autolock_eligible(&self, rel: &RelPath) -> bool {
        // Easy mode (`strict = off`) auto-publishes local edits exactly as
        // `autolock` does — a free path with a local write acquires a lease
        // instead of being reverted. A path held by another peer still takes
        // the violation path in either mode. A non-editor role is never
        // eligible: it cannot publish, so its writes always revert-and-quarantine.
        self.state.config.role.can_edit()
            && (self.state.config.autolock || !self.state.config.strict)
            && self.locks.holder(rel).is_none()
            && !self.autolock_pending.contains_key(rel)
    }

    /// Step 1 of autolock: preserve the un-leased bytes (async, off the actor)
    /// before touching anything, honoring the Golden Invariant even if the
    /// acquire later fails.
    fn spawn_autolock(&mut self, rel: RelPath, new_file: bool) {
        self.busy.insert(rel.clone());
        let dir = self.dir.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let quarantined = match guard::quarantine(&dir, &rel, "autolock") {
                Ok(q) => Some(q),
                Err(e) => {
                    warn!(path = %rel, "autolock: preserving bytes failed: {e}");
                    None
                }
            };
            let _ = events
                .send(Event::AutolockReady {
                    rel,
                    quarantined,
                    new_file,
                })
                .await;
        });
    }

    /// Step 2: the bytes are preserved; begin a standard acquire (all three
    /// preconditions unchanged). If a precondition already fails, complete the
    /// violation now with an autolock-specific hint.
    fn begin_autolock_acquire(
        &mut self,
        rel: RelPath,
        quarantined: Option<PathBuf>,
        new_file: bool,
    ) {
        // `spawn_autolock` set `busy` while it preserved the bytes; that is our
        // own bookkeeping, so clear it before the precondition check (whose
        // busy-guard is meant for *other* in-flight ops) and re-set it through
        // the acquire to block re-inspection of the path.
        self.busy.remove(&rel);
        if let Some(err) = self.strict_edit_guard(&rel) {
            let pre = Self::precondition_of(&err);
            self.autolock_fail(&rel, quarantined, new_file, pre);
            return;
        }
        self.busy.insert(rel.clone());
        let lamport = self.state.lamport + 1;
        self.state.lamport = lamport;
        let voters: BTreeSet<String> = self.peers.keys().map(|id| id.to_string()).collect();
        let now = Instant::now();
        if self
            .locks
            .start_request(&rel, lamport, voters, now)
            .is_err()
        {
            self.autolock_fail(&rel, quarantined, new_file, "LEASE".to_string());
            return;
        }
        self.autolock_pending
            .insert(rel.clone(), (quarantined, new_file));
        self.broadcast(Msg::LockReq {
            path: rel.clone(),
            lamport,
            ttl_ms: self.timings.ttl.as_millis() as u64,
        });
        // Drive completion through the standard grant/deny/timeout path with a
        // throwaway reply channel (no CLI is waiting on an autolock acquire).
        let (tx, _rx) = oneshot::channel();
        self.pending_acquires.insert(rel.clone(), (lamport, tx));
        let events = self.events_tx.clone();
        let timeout = self.timings.acquire_timeout;
        let r2 = rel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let _ = events
                .send(Event::AcquireTimeout { rel: r2, lamport })
                .await;
        });
    }

    /// Autolock won the lease: the edited bytes are already on disk (and
    /// preserved in quarantine), so publish them and start the idle-release
    /// countdown. Returns whether this completed an autolock (so the grant
    /// handler skips its normal reply bookkeeping).
    fn autolock_succeed(&mut self, rel: &RelPath) -> bool {
        let Some((_q, _new_file)) = self.autolock_pending.remove(rel) else {
            return false;
        };
        self.autolock_idle.insert(
            rel.clone(),
            Instant::now() + crate::consts::AUTOLOCK_IDLE_RELEASE,
        );
        self.push_event(format!("autolock acquired {rel}; publishing"));
        info!(path = %rel, "autolock acquired lease; publishing the edit");
        // spawn_publish re-uses the busy flag already set by spawn_autolock.
        self.spawn_publish(rel.clone(), PublishCause::Edit);
        true
    }

    /// Autolock could not acquire (any precondition): the bytes stay in
    /// quarantine, the indexed version is restored read-only (or a new file is
    /// removed), and a diagnosis with an autolock hint is logged.
    fn autolock_fail(
        &mut self,
        rel: &RelPath,
        quarantined: Option<PathBuf>,
        new_file: bool,
        precondition: String,
    ) {
        self.autolock_pending.remove(rel);
        let q = quarantined
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        warn!(
            path = %rel,
            precondition = %precondition,
            quarantine = %q,
            "autolock could not acquire ({precondition}); your bytes are safe in conflicts/"
        );
        self.push_event(format!("autolock failed for {rel} ({precondition})"));
        // Golden Invariant: if preserving the bytes failed, never remove or
        // restore over them — leave them in place and say so.
        if quarantined.is_none() {
            error!(
                path = %rel,
                "autolock revert skipped: the bytes could not be preserved first; leaving them on disk"
            );
            self.unbusy(rel);
            return;
        }
        if new_file {
            // No indexed version to restore; the bytes are preserved, so drop
            // the on-disk copy exactly as the new-file violation path does
            // (clear read-only first, delete with retry).
            let abs = rel.to_fs_path(&self.dir);
            self.mute(rel);
            let _ = guard::set_writable(&abs);
            if let Err(e) = crate::win_fs::remove_file(&abs) {
                warn!(path = %rel, "autolock: could not remove un-leased new file: {e}");
            }
            self.unbusy(rel);
        } else {
            // Restore the indexed version read-only (materialize async), reusing
            // the violation-staging path but WITHOUT re-quarantining.
            let Some(record) = self.state.files.get(rel).cloned() else {
                self.unbusy(rel);
                return;
            };
            let transfer = self.transfer.clone();
            let events = self.events_tx.clone();
            let rel2 = rel.clone();
            tokio::spawn(async move {
                let result = transfer.materialize(&record.manifest, record.size).await;
                let _ = events
                    .send(Event::ViolationStaged {
                        rel: rel2,
                        result,
                        quarantined,
                        preserve_required: true,
                    })
                    .await;
            });
        }
    }

    // ---------------- IPC ----------------

    fn on_ipc(&mut self, req: IpcRequest, reply: oneshot::Sender<IpcResponse>) {
        match req {
            IpcRequest::Status => {
                let _ = reply.send(IpcResponse::ok(self.status_json()));
            }
            IpcRequest::Lock { path } => self.handle_lock(path, reply),
            IpcRequest::LockWait { path } => self.handle_lock_wait(path, reply),
            IpcRequest::Unlock { path } => self.handle_unlock(path, reply),
            IpcRequest::Invite { role, ttl_ms } => {
                self.handle_invite(role, ttl_ms, reply);
            }
            IpcRequest::Versions { path } => {
                let Ok(rel) = sanitize_rel_path(&path) else {
                    let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
                    return;
                };
                let entries: Vec<serde_json::Value> = versions::list(&self.state, &rel)
                    .into_iter()
                    .map(|(n, ts, size, tag, pinned)| {
                        serde_json::json!({
                            "n": n,
                            "ts": guard::utc_timestamp(ts),
                            "ts_ms": ts,
                            "size": size,
                            "tag": tag,
                            "pinned": pinned,
                        })
                    })
                    .collect();
                let (_, _, hbytes) = versions::footprint(&self.state);
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "path": rel.as_str(),
                    "versions": entries,
                    "history_bytes": versions::disk_bytes(&self.state, &rel),
                    "session_history_bytes": hbytes,
                })));
            }
            IpcRequest::Tag { path, n, name } => self.handle_tag(path, n, name, reply),
            IpcRequest::Pin { path, n, pinned } => self.handle_pin(path, n, pinned, reply),
            IpcRequest::Diff { path, n } => self.handle_diff(path, n, reply),
            IpcRequest::Restore { path, n } => self.handle_restore(path, n, reply),
            IpcRequest::Gc => self.start_gc(Some(reply)),
            IpcRequest::Doctor => {
                let _ = reply.send(IpcResponse::ok(self.doctor_json()));
            }
            IpcRequest::DashboardStart => {
                if !self.dashboard_started {
                    self.dashboard_started = true;
                    tokio::spawn(crate::dashboard::serve(
                        self.dashboard_ipc.clone(),
                        self.dashboard_token.clone(),
                        self.dashboard_port,
                        self.dashboard_bound.clone(),
                    ));
                    info!(port = self.dashboard_port, "dashboard started on demand");
                }
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "port": self.dashboard_port,
                    "token": self.dashboard_token,
                })));
            }
            IpcRequest::DashboardInfo => {
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "port": self.dashboard_bound.load(std::sync::atomic::Ordering::Relaxed),
                    "token": self.dashboard_token,
                })));
            }
            IpcRequest::DashboardState => {
                let _ = reply.send(IpcResponse::ok(self.dashboard_state_json()));
            }
            IpcRequest::ConfigSet { key, value } => self.handle_config_set(key, value, reply),
            IpcRequest::PeerName { id, name } => self.handle_peer_name(id, name, reply),
            IpcRequest::Shutdown => {
                info!("shutdown requested over IPC");
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "stopping": true,
                    "note": "releasing leases, saying goodbye, persisting state",
                })));
                self.shutdown_requested = true;
            }
            IpcRequest::Conflicts => {
                let (total, bytes) = self.conflicts_totals();
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "conflicts": self.list_conflicts(),
                    "conflicts_total": total,
                    "conflicts_bytes": bytes,
                })));
            }
            IpcRequest::ConflictApply { id, target } => {
                self.handle_conflict_apply(id, target, reply)
            }
            IpcRequest::ConflictDiscard { id } => {
                // The ONLY single-copy deletion path besides `conflicts prune`,
                // and it exists precisely because the user asked for it.
                match crate::conflicts::discard(&self.dir, &id) {
                    Ok(size) => {
                        info!(id = %id, "quarantined copy discarded (explicit resolution)");
                        self.push_event(format!("conflict copy discarded: {id}"));
                        let _ = reply.send(IpcResponse::ok(
                            serde_json::json!({ "discarded": id, "size": size }),
                        ));
                    }
                    Err(e) => {
                        let _ = reply.send(IpcResponse::err("discard_failed", e));
                    }
                }
            }
        }
    }

    /// The `api:1` dashboard snapshot: the `status` schema-1 payload plus mode,
    /// a config summary, the conflicts list, and per-path version entries — one
    /// snapshot that powers the whole UI without disturbing the schema-1 status
    /// contract.
    fn dashboard_state_json(&self) -> serde_json::Value {
        let mut v = self.status_json();
        let c = &self.state.config;
        v["mode"] = serde_json::json!(self.state.mode);
        v["airgap"] = serde_json::json!(self.airgap);
        v["config"] = serde_json::json!({
            "autolock": c.autolock,
            "strict": c.strict,
            "role": c.role.as_str(),
            "update_channel": c.update_channel,
            "lease_ttl_ms": c.lease_ttl_ms,
            "acquire_timeout_ms": c.acquire_timeout_ms,
            "wait_timeout_ms": c.wait_timeout_ms,
            "dashboard_port": c.dashboard_port,
            "relay": c.relay,
            "lan": c.lan,
            "max_down": c.max_down,
        });
        v["conflicts"] = serde_json::json!(self.list_conflicts());
        let (ctotal, cbytes) = self.conflicts_totals();
        v["conflicts_total"] = serde_json::json!(ctotal);
        v["conflicts_bytes"] = serde_json::json!(cbytes);
        let mut versions = serde_json::Map::new();
        for path in self.state.history.keys() {
            let entries: Vec<serde_json::Value> = versions::list(&self.state, path)
                .into_iter()
                .map(|(n, ts, size, tag, pinned)| {
                    serde_json::json!({ "n": n, "ts_ms": ts, "size": size, "tag": tag, "pinned": pinned })
                })
                .collect();
            if !entries.is_empty() {
                versions.insert(path.as_str().to_string(), serde_json::Value::Array(entries));
            }
        }
        v["versions"] = serde_json::Value::Object(versions);
        v
    }

    /// Lists quarantined copies, newest first (P18: structured — reason and
    /// original path from the conflicts index, plus a suggested keep-both
    /// name). Capped at [`crate::consts::CONFLICTS_LIST_MAX`] entries so the
    /// dashboard/IPC snapshot can never blow the 1 MiB IPC line; the total is
    /// reported alongside so nothing hides.
    fn list_conflicts(&self) -> Vec<serde_json::Value> {
        let now = crate::now_ms();
        let mut out = Vec::new();
        // Two caps so the single-line IPC/dashboard payload can never blow
        // IPC_LINE_MAX: at most CONFLICTS_LIST_MAX entries, and at most a byte
        // budget well under the 1 MiB line (each entry carries path + both_name,
        // so a count cap alone is not enough — a reviewer caught this).
        let mut budget: usize = 700 * 1024;
        for e in crate::conflicts::list(&self.dir) {
            if out.len() >= crate::consts::CONFLICTS_LIST_MAX {
                break;
            }
            let both = e.path.as_deref().map(|p| {
                crate::conflicts::both_name(p, now, |c| {
                    // A name we cannot sanitize is not a real target ⇒ NOT
                    // taken (returning `true` here could spin `both_name`).
                    crate::sync::index::sanitize_rel_path(c)
                        .map(|r| self.state.files.get(&r).is_some_and(|f| !f.deleted))
                        .unwrap_or(false)
                })
            });
            let cost = e.name.len()
                + e.path.as_deref().map(str::len).unwrap_or(0)
                + both.as_deref().map(str::len).unwrap_or(0)
                + 160;
            budget = budget.saturating_sub(cost);
            if budget == 0 && !out.is_empty() {
                break;
            }
            out.push(serde_json::json!({
                "name": e.name,
                "path": e.path,
                "reason": e.reason,
                "size": e.size,
                "ts_ms": e.ts_ms,
                "age_ms": now.saturating_sub(e.ts_ms),
                "both_name": both,
            }));
        }
        out
    }

    /// Total count + bytes of the quarantine (uncapped, for the badge/report).
    fn conflicts_totals(&self) -> (usize, u64) {
        let all = crate::conflicts::list(&self.dir);
        let bytes = all.iter().map(|e| e.size).sum();
        (all.len(), bytes)
    }

    /// P18 `conflicts resolve` write step: copy quarantined bytes into a
    /// working-tree path the caller already holds a lease on. Exactly
    /// Restore's precondition ladder — role, reachability, self-held lease,
    /// not busy — so the publish that follows (the normal leased-edit flow)
    /// is indistinguishable from the user pasting the bytes in by hand. The
    /// quarantined copy itself is NOT touched here; discarding it is a
    /// separate, explicit step after the publish succeeds.
    fn handle_conflict_apply(
        &mut self,
        id: String,
        target: String,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        let qpath = match crate::conflicts::copy_path(&self.dir, &id) {
            Ok(p) => p,
            Err(e) => {
                let _ = reply.send(IpcResponse::err("unknown_conflict", e));
                return;
            }
        };
        let Ok(rel) = sanitize_rel_path(&target) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        if let Some(err) = self.role_edit_guard() {
            let _ = reply.send(err);
            return;
        }
        if self.peers.is_empty() {
            let _ = reply.send(IpcResponse::err(
                "strict_offline",
                "resolving needs at least one connected peer (the publish must be seen)",
            ));
            return;
        }
        if !self.locks.is_held_by_me(&rel) {
            let _ = reply.send(IpcResponse::err(
                "not_held",
                format!("resolve requires a self-held lease (run `tazamun lock {rel}` first)"),
            ));
            return;
        }
        // Golden Invariant: refuse to overwrite bytes we cannot recover. An
        // on-disk file that is NOT a live indexed record has no history copy, so
        // clobbering it (e.g. `--keep both --into an-ignored-file`) would delete
        // the only copy. An indexed file is safe — the publish pushes its prior
        // manifest to history first.
        let abs = rel.to_fs_path(&self.dir);
        let indexed = self.state.files.get(&rel).is_some_and(|r| !r.deleted);
        if abs.exists() && !indexed {
            let _ = reply.send(IpcResponse::err(
                "target_exists",
                format!(
                    "{rel} already exists on disk but is not a synced file; \
                     refusing to overwrite unrecoverable bytes — choose another `--into` name"
                ),
            ));
            return;
        }
        if self.busy.contains(&rel) {
            let _ = reply.send(IpcResponse::err(
                "busy",
                "another operation is running on this path; retry in a moment",
            ));
            return;
        }
        self.busy.insert(rel.clone());
        let tmp_dir = crate::state::tmp_dir(&self.dir);
        let events = self.events_tx.clone();
        // Do the slow copy into a temp file OFF the actor; the actor does the
        // instant atomic rename in on_conflict_applied (mirrors restore), so a
        // failed/partial copy never leaves a torn working file.
        tokio::spawn(async move {
            let result = (|| {
                std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
                let named = tempfile::Builder::new()
                    .prefix("conflict-")
                    .tempfile_in(&tmp_dir)
                    .map_err(|e| e.to_string())?;
                let bytes = std::fs::copy(&qpath, named.path()).map_err(|e| e.to_string())?;
                Ok((named.into_temp_path(), bytes))
            })();
            let _ = events
                .send(Event::ConflictApplied { rel, result, reply })
                .await;
        });
    }

    /// Completion of the staged quarantine copy: atomically rename it into the
    /// working path. The lease is re-verified first — if it lapsed while
    /// staging, nothing is written (the temp is dropped) and the quarantined
    /// copy is untouched, so a lost lease can never destroy bytes.
    fn on_conflict_applied(
        &mut self,
        rel: RelPath,
        result: Result<(tempfile::TempPath, u64), String>,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        let (temp, bytes) = match result {
            Ok(t) => t,
            Err(e) => {
                self.unbusy(&rel);
                let _ = reply.send(IpcResponse::err("apply_failed", e));
                return;
            }
        };
        if !self.locks.is_held_by_me(&rel) {
            self.unbusy(&rel);
            // temp drops here → nothing written, quarantine copy untouched.
            let _ = reply.send(IpcResponse::err(
                "lease_lost",
                "the lease lapsed while staging; nothing was written and the quarantine copy is untouched",
            ));
            return;
        }
        let abs = rel.to_fs_path(&self.dir);
        // Mute our own write so the watcher event does not race as a violation;
        // the CLI/dashboard flow publishes it explicitly on unlock.
        self.mute(&rel);
        let applied: Result<(), String> = (|| {
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let _ = guard::set_writable(&abs);
            crate::win_fs::persist_temp(temp, &abs).map_err(|e| e.to_string())?;
            Ok(())
        })();
        self.unbusy(&rel);
        match applied {
            Ok(()) => {
                info!(path = %rel, bytes, "quarantined bytes applied under lease");
                self.push_event(format!("conflict bytes applied to {rel}"));
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "applied": rel.as_str(),
                    "bytes": bytes,
                })));
            }
            Err(e) => {
                let _ = reply.send(IpcResponse::err("apply_failed", e));
            }
        }
    }

    /// Mints an invite (P17). On a v2 session an editor node signs a role- and
    /// expiry-scoped grant; a viewer node (no admin secret) is refused. A legacy
    /// v1 session ignores role/ttl and returns the plain secret+bootstrap ticket.
    fn handle_invite(
        &self,
        role: Option<String>,
        ttl_ms: Option<u64>,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        let session_secret = match self.state.session_secret_bytes() {
            Ok(b) => b,
            Err(e) => {
                let _ = reply.send(IpcResponse::err("internal", e.to_string()));
                return;
            }
        };
        let bootstrap = vec![AddrWire::from_endpoint_addr(&self.endpoint.addr())];

        // Legacy v1 session (no admin key): plain ticket, role/ttl ignored.
        let (Some(admin_secret), Some(admin_pub)) = (
            self.state.admin_secret_key(),
            self.state.admin_public_bytes(),
        ) else {
            if self.state.enforcing_roles() {
                // A v2 viewer/archive node cannot sign grants, so it cannot invite.
                let _ = reply.send(IpcResponse::err(
                    "role_forbidden",
                    "only editors can mint invites (this node is a viewer/archive)",
                ));
                return;
            }
            let ticket = Ticket::new(SessionSecret(session_secret), bootstrap);
            let _ = reply.send(IpcResponse::ok(
                serde_json::json!({ "ticket": ticket.encode() }),
            ));
            return;
        };

        let role_code = match role.as_deref() {
            Some(r) => match crate::state::NodeRole::parse(r) {
                Ok(nr) => nr.code(),
                Err(e) => {
                    let _ = reply.send(IpcResponse::err("bad_role", e));
                    return;
                }
            },
            None => crate::session::ROLE_EDITOR,
        };
        let ttl = ttl_ms.unwrap_or(0);
        let now = crate::now_ms();
        let ticket = crate::session::mint_ticket(
            session_secret,
            Some((&admin_secret, admin_pub)),
            role_code,
            rand::random(),
            now,
            ttl,
            bootstrap,
        );
        let mut note = format!("this is an {} invite", crate::session::role_name(role_code));
        if ttl > 0 {
            note.push_str(&format!(
                ", expiring in {}",
                humantime::format_duration(Duration::from_millis(ttl))
            ));
        }
        note.push('.');
        let _ = reply.send(IpcResponse::ok(
            serde_json::json!({ "ticket": ticket.encode(), "note": note }),
        ));
    }

    /// Every endpoint id this node knows of (connected peers, seen members,
    /// bootstrap members, already-named peers), as lowercase hex strings.
    fn known_peer_ids(&self) -> BTreeSet<String> {
        let mut ids: BTreeSet<String> = self.peers.keys().map(|id| id.to_string()).collect();
        ids.extend(self.members.keys().map(|id| id.to_string()));
        ids.extend(
            self.state
                .known_members
                .values()
                .filter_map(|w| w.endpoint_id())
                .map(|id| id.to_string()),
        );
        ids.extend(self.state.peer_names.keys().cloned());
        ids.remove(&self.me_str);
        ids
    }

    /// Resolves a user-supplied id or short prefix to exactly one known peer id.
    fn resolve_peer_id(&self, needle: &str) -> Result<String, String> {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return Err("empty peer id".into());
        }
        let matches: Vec<String> = self
            .known_peer_ids()
            .into_iter()
            .filter(|id| id.starts_with(&needle))
            .collect();
        match matches.len() {
            1 => Ok(matches.into_iter().next().expect("one match")),
            0 => {
                // Accept a full, valid endpoint id even if we have not seen it.
                if crate::state::decode_hex32(&needle)
                    .and_then(|b| EndpointId::from_bytes(&b).ok())
                    .is_some()
                {
                    Ok(needle)
                } else {
                    Err(format!("no known peer matches {needle:?}"))
                }
            }
            n => Err(format!(
                "{needle:?} is ambiguous ({n} peers match — use more characters)"
            )),
        }
    }

    /// Sets or clears a local peer label (P17). Resolves a short id prefix,
    /// persists, and echoes the resolved id + name.
    fn handle_peer_name(
        &mut self,
        id: String,
        name: Option<String>,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        let full = match self.resolve_peer_id(&id) {
            Ok(f) => f,
            Err(e) => {
                let _ = reply.send(IpcResponse::err("unknown_peer", e));
                return;
            }
        };
        match name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
            Some(n) => {
                if n.len() > 64 {
                    let _ = reply.send(IpcResponse::err("bad_name", "name too long (max 64)"));
                    return;
                }
                self.state.peer_names.insert(full.clone(), n.clone());
                self.persist();
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "id": full, "name": n }),
                ));
            }
            None => {
                let existed = self.state.peer_names.remove(&full).is_some();
                self.persist();
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "id": full, "cleared": existed }),
                ));
            }
        }
    }

    /// Applies a live config change (dashboard `/api/config` and `ConfigSet`
    /// IPC), persisting it and applying the runtime effect for timing keys.
    fn handle_config_set(
        &mut self,
        key: String,
        value: String,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        match self.state.config.set_live_value(&key, &value) {
            Ok(note) => {
                // Timing keys take effect immediately for future leases.
                if key == "lease-ttl" || key == "acquire-timeout" {
                    let timings = LockTimings {
                        ttl: self.state.config.lease_ttl(),
                        renew: self.state.config.lease_renew(),
                        acquire_timeout: self.state.config.acquire_timeout(),
                    };
                    self.timings = timings;
                    self.locks.set_timings(timings);
                }
                // P15: the download governor retunes live — the next chunk
                // fetch draws from the new bucket without a daemon restart.
                if key == "max-down" {
                    self.limiter.set_rate(self.state.config.max_down);
                }
                // P19: toggling audit opens/closes the log live.
                if key == "audit" {
                    self.audit = if self.state.config.audit {
                        crate::audit::AuditLog::open(&self.dir).ok()
                    } else {
                        None
                    };
                }
                self.persist();
                info!(key = %key, "config set live via dashboard/IPC");
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "set": key, "note": note }),
                ));
            }
            Err(e) => {
                let _ = reply.send(IpcResponse::err("bad_config", e));
            }
        }
    }

    /// The daemon's live contribution to `tazamun doctor`: identity, bound
    /// sockets, relay policy, home-relay status, and per-peer connectivity
    /// (grade, conn, RTT, path changes, time-to-direct) from telemetry.
    fn doctor_json(&self) -> serde_json::Value {
        let now = Instant::now();
        let addr = self.endpoint.addr();
        let relay = addr.addrs.iter().find_map(|t| match t {
            iroh::TransportAddr::Relay(url) => Some(url.to_string()),
            _ => None,
        });
        let bound: Vec<String> = self
            .endpoint
            .bound_sockets()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let peers: Vec<serde_json::Value> = self
            .peers
            .keys()
            .map(|id| {
                let h = self.peer_health.get(id);
                serde_json::json!({
                    "id": id.to_string(),
                    "grade": h.map(|h| h.grade(now).to_string()).unwrap_or_else(|| "Offline".into()),
                    "conn": h.map(|h| h.conn.to_string()).unwrap_or_else(|| "None".into()),
                    "rtt_ms": h.map(|h| h.rtt_ms).unwrap_or(0.0),
                    "path_changes": h.map(|h| h.path_changes).unwrap_or(0),
                    "time_to_direct_ms": h.and_then(|h| h.time_to_direct).map(|d| d.as_millis() as u64),
                    "relay_url": h.and_then(|h| h.relay_url.clone()),
                    "via_lan": h.is_some_and(|h| h.on_lan),
                })
            })
            .collect();
        // Live home-relay connection status — the daemon's actual relay
        // handshake result, which doctor uses as its custom-relay probe.
        use iroh::Watcher;
        let relay_status: Vec<serde_json::Value> = self
            .endpoint
            .home_relay_status()
            .get()
            .iter()
            .map(|r| {
                serde_json::json!({
                    "url": r.url().to_string(),
                    "connected": r.is_connected(),
                })
            })
            .collect();
        serde_json::json!({
            "id": self.me.to_string(),
            "mode": if self.airgap { "airgap" } else { "normal" },
            "home_relay": relay,
            "relay_status": relay_status,
            "bound_sockets": bound,
            "relay_policy": self.relay_policy,
            "lan_discovery": self.lan_enabled,
            "known_members": self.state.known_members.len(),
            "connected_peers": self.peers.len(),
            "unapplied_count": self.state.unapplied.len(),
            "peers": peers,
        })
    }

    /// A per-peer diagnosis row: identity, health grade, connection, and role
    /// in the pending decision.
    fn peer_diag(
        &self,
        id: &EndpointId,
        now: Instant,
        role: &str,
        answered: Option<bool>,
    ) -> serde_json::Value {
        let health = self.peer_health.get(id);
        let grade = health.map(|h| h.grade(now)).unwrap_or(HealthGrade::Offline);
        let (conn, rtt) = match health {
            Some(h) => (h.conn.to_string(), h.rtt_ms),
            None => ("None".to_string(), 0.0),
        };
        serde_json::json!({
            "id": id.to_string(),
            "grade": grade.to_string(),
            "conn": conn,
            "rtt_ms": rtt,
            "role": role,
            "answered": answered,
        })
    }

    /// Wraps a lock-failure error with a network-terms diagnosis payload:
    /// which precondition blocked it, the peers consulted with their grades,
    /// and an actionable hint.
    fn lock_error(
        &self,
        code: &str,
        message: impl Into<String>,
        precondition: &str,
        peers: Vec<serde_json::Value>,
        hint: &str,
    ) -> IpcResponse {
        let mut resp = IpcResponse::err(code, message);
        resp.data = Some(serde_json::json!({
            "diagnosis": {
                "precondition": precondition,
                "peers": peers,
                "hint": hint,
            }
        }));
        resp
    }

    /// Checks REACHABILITY and FRESHNESS, returning a diagnosed error when
    /// either fails. `None` means both hold and the acquire may proceed.
    /// Rebuilds the P11 sync-scope policy from the on-disk `.tazamunignore`
    /// plus the persisted config, then re-reconciles any held remote record
    /// the new rules no longer hold — so relaxing the rules releases files
    /// without a restart. Cheap (one small file read + glob compile).
    fn rebuild_ignore(&mut self) {
        self.ignore = load_ignore_set(&self.dir, &self.state.config);
        // Local holds are path-derived: re-verdict them under the new rules.
        self.held_local.retain(|rel, _| {
            let size = std::fs::metadata(rel.to_fs_path(&self.dir))
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            !self.ignore.verdict(rel, size).is_sync()
        });
        // Remote holds: release entries whose scope reason no longer applies.
        let release: Vec<RelPath> = self
            .state
            .unapplied
            .iter()
            .filter(|(rel, e)| {
                is_scope_reason(&e.reason)
                    && self.ignore.verdict(rel, Some(e.record.size)).is_sync()
            })
            .map(|(rel, _)| rel.clone())
            .collect();
        if release.is_empty() {
            return;
        }
        for rel in release {
            if self.state.unapplied.remove(&rel).is_some() {
                info!(path = %rel, "sync-scope rules relaxed; releasing held record");
                self.push_event(format!("released from hold: {rel}"));
            }
            // Re-reconcile from whichever peer advertises the path.
            let advert = self
                .peer_index
                .iter()
                .find_map(|(id, files)| files.get(&rel).map(|r| (*id, r.clone())));
            if let Some((from, record)) = advert {
                self.reconcile_one(from, &rel, &record);
            }
        }
        self.persist();
    }

    /// The sync-scope verdict for a local on-disk path (stat for the size
    /// ceiling): `Some(reason)` when it must be held, `None` when it syncs.
    fn scope_hold_reason(&self, rel: &RelPath) -> Option<String> {
        let size = std::fs::metadata(rel.to_fs_path(&self.dir))
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len());
        let verdict = self.ignore.verdict(rel, size);
        (!verdict.is_sync()).then(|| verdict.reason())
    }

    /// P10 role policy, checked before anything else on every local edit path
    /// (lock, unlock, restore) — a non-editor folder is refused with the same
    /// clear error whether online or offline. Local enforcement only: this
    /// node's daemon refusing this node's own edits. Peers refusing a rogue
    /// binary is mesh-wide enforcement, and the error text says which phase
    /// owns that.
    fn role_edit_guard(&self) -> Option<IpcResponse> {
        let role = self.state.config.role;
        if role.can_edit() {
            return None;
        }
        Some(IpcResponse::err(
            "role_forbidden",
            format!(
                "this folder's role is {role} (set in `tazamun setup`): it syncs and reads \
                 but never locks, edits, or publishes. To make it writable, run \
                 `tazamun config set role editor` and restart the daemon. (Role is enforced \
                 locally today; mesh-wide enforcement by peers arrives in a later phase.)",
                role = role.as_str()
            ),
        ))
    }

    fn strict_edit_guard(&self, rel: &RelPath) -> Option<IpcResponse> {
        let now = Instant::now();
        if self.peers.is_empty() {
            // Name the members we know about but cannot currently reach, so the
            // user sees who they are waiting on.
            let mut known: BTreeSet<EndpointId> = self.peer_health.keys().copied().collect();
            known.extend(
                self.state
                    .known_members
                    .keys()
                    .filter_map(|k| k.parse::<EndpointId>().ok()),
            );
            known.remove(&self.me);
            let peers: Vec<serde_json::Value> = known
                .iter()
                .map(|id| self.peer_diag(id, now, "offline", Some(false)))
                .collect();
            let message = if known.is_empty() {
                "strict mode: no authenticated peer is connected, so edits are refused".to_string()
            } else {
                let names: Vec<String> =
                    known.iter().map(|id| id.fmt_short().to_string()).collect();
                format!(
                    "strict mode: no peer is currently reachable (last known: {})",
                    names.join(", ")
                )
            };
            return Some(self.lock_error(
                "strict_offline",
                message,
                "REACHABILITY",
                peers,
                "wait for at least one peer to reconnect (check `tazamun status`)",
            ));
        }
        for id in self.peers.keys() {
            if !self.index_received.contains(id) {
                let peers = self
                    .peers
                    .keys()
                    .map(|p| {
                        let answered = self.index_received.contains(p);
                        self.peer_diag(p, now, "syncing", Some(answered))
                    })
                    .collect();
                return Some(self.lock_error(
                    "syncing",
                    "still exchanging indexes with a peer; retry in a moment",
                    "FRESHNESS",
                    peers,
                    "index exchange is still in progress; retry shortly",
                ));
            }
        }
        if self.pending_pulls.contains_key(rel) || self.deferred.contains_key(rel) {
            let peers = self
                .peers
                .keys()
                .map(|p| self.peer_diag(p, now, "voter", None))
                .collect();
            return Some(self.lock_error(
                "not_fresh",
                "a newer version of this path is still being pulled",
                "FRESHNESS",
                peers,
                "let the in-flight pull finish, then retry",
            ));
        }
        if self.busy.contains(rel) {
            return Some(self.lock_error(
                "busy",
                "an operation on this path is in progress; retry in a moment",
                "FRESHNESS",
                vec![],
                "an operation on this path is in progress; retry shortly",
            ));
        }
        let local_vv = self
            .state
            .files
            .get(rel)
            .map(|r| r.vv.clone())
            .unwrap_or_default();
        for (id, index) in &self.peer_index {
            if !self.peers.contains_key(id) {
                continue;
            }
            if let Some(theirs) = index.get(rel) {
                match vclock::compare(&local_vv, &theirs.vv) {
                    Causality::Equal | Causality::After => {}
                    Causality::Before | Causality::Concurrent => {
                        return Some(self.lock_error(
                            "not_fresh",
                            format!("peer {} advertises a newer version", id.fmt_short()),
                            "FRESHNESS",
                            vec![self.peer_diag(id, now, "ahead", None)],
                            "this peer has a newer version; wait for it to sync in, then retry",
                        ));
                    }
                }
            }
        }
        None
    }

    fn handle_lock(&mut self, path: String, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        if let Some(err) = self.role_edit_guard() {
            let _ = reply.send(err);
            return;
        }
        if self.locks.is_held_by_me(&rel) {
            let _ = reply.send(IpcResponse::ok(serde_json::json!({
                "locked": rel.as_str(),
                "already": true,
            })));
            return;
        }
        if let Some(holder) = self.locks.holder(&rel).cloned() {
            let now = Instant::now();
            let holder_peer = holder
                .parse::<EndpointId>()
                .ok()
                .map(|id| vec![self.peer_diag(&id, now, "holder", None)])
                .unwrap_or_default();
            let mut resp = self.lock_error(
                "lease_held",
                format!("lease is held by {holder}"),
                "LEASE",
                holder_peer,
                "wait for the current holder to unlock or its TTL to expire, or pass --wait",
            );
            if let Some(diag) = resp
                .data
                .as_mut()
                .and_then(|d| d.get_mut("diagnosis"))
                .and_then(|d| d.as_object_mut())
            {
                diag.insert("held_by".into(), serde_json::json!(holder));
            }
            let _ = reply.send(resp);
            return;
        }
        if self.pending_acquires.contains_key(&rel) {
            let _ = reply.send(IpcResponse::err(
                "busy",
                "a lock request for this path is already pending",
            ));
            return;
        }
        if let Some(err) = self.strict_edit_guard(&rel) {
            let _ = reply.send(err);
            return;
        }
        let lamport = self.state.lamport + 1;
        self.state.lamport = lamport;
        let voters: BTreeSet<String> = self.peers.keys().map(|id| id.to_string()).collect();
        let now = Instant::now();
        if let Err(e) = self.locks.start_request(&rel, lamport, voters, now) {
            let _ = reply.send(IpcResponse::err("lease_held", e.to_string()));
            return;
        }
        self.broadcast(Msg::LockReq {
            path: rel.clone(),
            lamport,
            ttl_ms: self.timings.ttl.as_millis() as u64,
        });
        self.pending_acquires.insert(rel.clone(), (lamport, reply));
        let events = self.events_tx.clone();
        let timeout = self.timings.acquire_timeout;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let _ = events.send(Event::AcquireTimeout { rel, lamport }).await;
        });
    }

    fn on_acquire_timeout(&mut self, rel: RelPath, lamport: u64) {
        let matches_waiter = self
            .pending_acquires
            .get(&rel)
            .is_some_and(|(l, _)| *l == lamport);
        if !matches_waiter {
            return;
        }
        // Autolock acquire timed out (no quorum): revert with REACHABILITY.
        if let Some((q, nf)) = self.autolock_pending.remove(&rel) {
            self.pending_acquires.remove(&rel);
            self.locks.on_deny(&rel);
            self.broadcast(Msg::LockRelease { path: rel.clone() });
            self.autolock_fail(&rel, q, nf, "REACHABILITY".to_string());
            return;
        }
        let now = Instant::now();
        // Name the voters who never answered, with their current conn state.
        let peers: Vec<serde_json::Value> = match self.locks.pending_votes(&rel) {
            Some((needed, granted)) => needed
                .iter()
                .filter_map(|id_str| id_str.parse::<EndpointId>().ok())
                .map(|id| {
                    let answered = granted.contains(&id.to_string());
                    self.peer_diag(&id, now, "voter", Some(answered))
                })
                .collect(),
            None => vec![],
        };
        let unanswered: Vec<String> = peers
            .iter()
            .filter(|p| p["answered"].as_bool() == Some(false))
            .filter_map(|p| p["id"].as_str().map(|s| s.chars().take(10).collect()))
            .collect();
        self.locks.on_deny(&rel);
        self.broadcast(Msg::LockRelease { path: rel.clone() });
        if let Some((_, reply)) = self.pending_acquires.remove(&rel) {
            let msg = if unanswered.is_empty() {
                "not every peer answered the lock request in time".to_string()
            } else {
                format!("no answer in time from: {}", unanswered.join(", "))
            };
            self.observe(
                "lock-denied",
                Some(rel.as_str()),
                None,
                Some(format!("timeout: {msg}")),
            );
            let _ = reply.send(self.lock_error(
                "timeout",
                msg,
                "REACHABILITY",
                peers,
                "a consulted peer did not respond — check its link in `tazamun status`",
            ));
        }
    }

    fn handle_unlock(&mut self, path: String, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        if let Some(err) = self.role_edit_guard() {
            let _ = reply.send(err);
            return;
        }
        if !self.locks.is_held_by_me(&rel) {
            let _ = reply.send(IpcResponse::err(
                "not_held",
                "you do not hold a lease on this path",
            ));
            return;
        }
        if self.busy.contains(&rel) || self.pending_unlocks.contains_key(&rel) {
            let _ = reply.send(IpcResponse::err(
                "busy",
                "your edit is still syncing; retry in a moment",
            ));
            return;
        }
        // Flush any last un-debounced edit before releasing the lease.
        self.pending_unlocks.insert(rel.clone(), ());
        self.inspect(rel, InspectCause::Unlock(reply));
    }

    /// Releases a self-held lease and re-applies permissions. `published_ok` is
    /// false only when an edit was present but its publish failed (or the lease
    /// lapsed mid-publish): the lease is still released and the file settled,
    /// but the reply is an error so the caller knows nothing synced — which is
    /// what stops `conflicts resolve` from discarding the quarantine copy after
    /// a publish that never landed.
    fn finish_unlock(
        &mut self,
        rel: &RelPath,
        published_ok: bool,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        self.pending_unlocks.remove(rel);
        let me = self.me_str.clone();
        self.locks.on_release(rel, &me);
        self.autolock_idle.remove(rel);
        let abs = rel.to_fs_path(&self.dir);
        let settle = if self.state.config.enforce_readonly() {
            guard::set_readonly(&abs)
        } else {
            guard::set_writable(&abs)
        };
        if let Err(e) = settle {
            warn!(path = %rel, "could not re-apply permissions on unlock: {e}");
        }
        self.broadcast(Msg::LockRelease { path: rel.clone() });
        self.announce_freed(rel);
        if published_ok {
            info!(path = %rel, "lease released");
            self.observe("unlock", Some(rel.as_str()), None, None);
            let _ = reply.send(IpcResponse::ok(
                serde_json::json!({ "unlocked": rel.as_str() }),
            ));
        } else {
            warn!(path = %rel, "lease released but the edit did not publish");
            let _ = reply.send(IpcResponse::err(
                "publish_failed",
                "the edit could not be published (lease released, bytes kept locally); nothing synced",
            ));
        }
    }

    /// `LockWait`: register interest in a held path so the holder is told and
    /// this node shows up as a waiter; the CLI re-attempts the acquire.
    fn handle_lock_wait(&mut self, path: String, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        match self.locks.holder(&rel).cloned() {
            None => {
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "waiting": false, "reason": "free" }),
                ));
            }
            Some(h) if h == self.me_str => {
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "waiting": false, "reason": "mine" }),
                ));
            }
            Some(holder) => {
                let deadline = Instant::now() + self.state.config.wait_timeout();
                self.my_waits
                    .insert(rel.clone(), (holder.clone(), deadline));
                self.broadcast(Msg::LockInterest { path: rel.clone() });
                let short: String = holder.chars().take(10).collect();
                self.push_event(format!("waiting for {rel} (behind {short})"));
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "waiting": true, "behind": holder }),
                ));
            }
        }
    }

    /// Broadcasts that a path is now free so any waiter re-attempts its acquire.
    fn announce_freed(&self, rel: &RelPath) {
        self.broadcast(Msg::LockFreed { path: rel.clone() });
    }

    /// Releases a lease we hold with no IPC reply (autolock idle-release and
    /// other internal releases): frees the table, restores read-only, and tells
    /// peers + waiters.
    fn release_own_lease(&mut self, rel: &RelPath) {
        if !self.locks.is_held_by_me(rel) {
            return;
        }
        let me = self.me_str.clone();
        self.locks.on_release(rel, &me);
        self.autolock_idle.remove(rel);
        let p = rel.to_fs_path(&self.dir);
        let settle = if self.state.config.enforce_readonly() {
            guard::set_readonly(&p)
        } else {
            guard::set_writable(&p)
        };
        if let Err(e) = settle {
            warn!(path = %rel, "could not re-apply permissions on release: {e}");
        }
        self.broadcast(Msg::LockRelease { path: rel.clone() });
        self.announce_freed(rel);
        info!(path = %rel, "lease released");
        self.observe(
            "unlock",
            Some(rel.as_str()),
            None,
            Some("auto-release".into()),
        );
    }

    /// `tag`: name (or clear) version `n` of a path. Local metadata — no lease,
    /// no peer, works for any role.
    fn handle_tag(
        &mut self,
        path: String,
        n: usize,
        name: Option<String>,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        if versions::tag(&mut self.state, &rel, n, name.clone()) {
            self.persist();
            let _ = reply.send(IpcResponse::ok(serde_json::json!({
                "path": rel.as_str(), "n": n, "tag": name,
            })));
        } else {
            let _ = reply.send(IpcResponse::err(
                "no_history",
                format!("no history entry {n} for {rel}"),
            ));
        }
    }

    /// `pin`/`unpin`: mark version `n` immune to depth pruning (its blobs are
    /// already GC-protected while it stays in history).
    fn handle_pin(
        &mut self,
        path: String,
        n: usize,
        pinned: bool,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        if versions::set_pinned(&mut self.state, &rel, n, pinned) {
            self.persist();
            let _ = reply.send(IpcResponse::ok(serde_json::json!({
                "path": rel.as_str(), "n": n, "pinned": pinned,
            })));
        } else {
            let _ = reply.send(IpcResponse::err(
                "no_history",
                format!("no history entry {n} for {rel}"),
            ));
        }
    }

    /// `diff`: chunk-aware compare of the current file against version `n`.
    /// Resolves both manifests from the local store off the actor thread.
    fn handle_diff(&mut self, path: String, n: usize, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        let Some(cur) = self.state.files.get(&rel).filter(|r| !r.deleted) else {
            let _ = reply.send(IpcResponse::err(
                "no_current",
                format!("{rel} has no current synced version to diff"),
            ));
            return;
        };
        let Some(ver) = versions::entry(&self.state, &rel, n) else {
            let _ = reply.send(IpcResponse::err(
                "no_history",
                format!("no history entry {n} for {rel}"),
            ));
            return;
        };
        let (cur_manifest, cur_size) = (cur.manifest.clone(), cur.size);
        let transfer = self.transfer.clone();
        let rel_s = rel.as_str().to_string();
        tokio::spawn(async move {
            let old = transfer
                .local_manifest_chunks(&ver.manifest, ver.size)
                .await;
            let new = transfer
                .local_manifest_chunks(&cur_manifest, cur_size)
                .await;
            let resp = match (old, new) {
                (Ok(old), Ok(new)) => {
                    let d = versions::diff_chunks(&old, &new);
                    IpcResponse::ok(serde_json::json!({
                        "path": rel_s,
                        "n": n,
                        "version_tag": ver.tag,
                        "version_ts_ms": ver.ts_ms,
                        "old_chunks": d.old_chunks,
                        "new_chunks": d.new_chunks,
                        "identical": d.identical,
                        "added": d.added,
                        "removed": d.removed,
                        "moved": d.moved,
                        "old_bytes": d.old_bytes,
                        "new_bytes": d.new_bytes,
                        "transfer_bytes": d.transfer_bytes,
                        "changed_pct": d.changed_pct(),
                        "identical_content": d.identical_content(),
                    }))
                }
                (Err(e), _) | (_, Err(e)) => {
                    IpcResponse::err("diff_failed", format!("could not resolve a manifest: {e}"))
                }
            };
            let _ = reply.send(resp);
        });
    }

    fn handle_restore(&mut self, path: String, n: usize, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
        if let Some(err) = self.role_edit_guard() {
            let _ = reply.send(err);
            return;
        }
        if self.peers.is_empty() {
            let _ = reply.send(IpcResponse::err(
                "strict_offline",
                "strict mode: no authenticated peer is connected, so edits are refused",
            ));
            return;
        }
        if !self.locks.is_held_by_me(&rel) {
            let _ = reply.send(IpcResponse::err(
                "not_held",
                "restore requires a self-held lease (run `tazamun lock` first)",
            ));
            return;
        }
        if self.busy.contains(&rel) {
            let _ = reply.send(IpcResponse::err(
                "busy",
                "an operation on this path is in progress; retry in a moment",
            ));
            return;
        }
        let Some(entry) = versions::entry(&self.state, &rel, n) else {
            let _ = reply.send(IpcResponse::err(
                "no_history",
                format!("no history entry {n} for this path"),
            ));
            return;
        };
        self.busy.insert(rel.clone());
        let transfer = self.transfer.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let result = transfer.materialize(&entry.manifest, entry.size).await;
            let _ = events
                .send(Event::RestoreStaged {
                    rel,
                    entry,
                    result,
                    reply,
                })
                .await;
        });
    }

    fn on_restore_staged(
        &mut self,
        rel: RelPath,
        entry: VersionEntry,
        result: Result<Staged, TransferError>,
        reply: oneshot::Sender<IpcResponse>,
    ) {
        if !self.locks.is_held_by_me(&rel) {
            let _ = reply.send(IpcResponse::err(
                "not_held",
                "lease was lost while restoring",
            ));
            self.unbusy(&rel);
            return;
        }
        let staged = match result {
            Ok(s) => s,
            Err(e) => {
                let _ = reply.send(IpcResponse::err("restore_failed", e.to_string()));
                self.unbusy(&rel);
                return;
            }
        };
        let abs = rel.to_fs_path(&self.dir);
        self.mute(&rel);
        let apply: Result<(), String> = (|| {
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let _ = guard::set_writable(&abs);
            crate::win_fs::persist_temp(staged.temp, &abs).map_err(|e| e.to_string())?;
            // The path stays writable: the lease is still ours.
            drop(staged.tags);
            Ok(())
        })();
        match apply {
            Ok(()) => {
                let prev = self.state.files.get(&rel).cloned();
                let mut vv = prev.as_ref().map(|r| r.vv.clone()).unwrap_or_default();
                vclock::inc(&mut vv, &self.me_str);
                if let Some(prev) = &prev {
                    versions::push(&mut self.state, &rel, prev);
                }
                let record = FileRecord {
                    size: entry.size,
                    manifest: entry.manifest,
                    vv,
                    deleted: false,
                    updated_at_ms: crate::now_ms(),
                };
                self.state.files.insert(rel.clone(), record.clone());
                self.state.lamport += 1;
                self.persist();
                self.broadcast(Msg::FileMeta {
                    path: rel.clone(),
                    record,
                    lamport: self.state.lamport,
                });
                info!(path = %rel, "restored historical version as a new edit");
                self.observe("restore", Some(rel.as_str()), None, None);
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "restored": rel.as_str(),
                    "size": entry.size,
                })));
            }
            Err(e) => {
                let _ = reply.send(IpcResponse::err("restore_failed", e));
            }
        }
        self.unbusy(&rel);
    }

    /// Recomputes the GC-protected blob set from committed state. The store
    /// itself sweeps unprotected blobs on its configured interval; refreshing
    /// after every commit keeps the snapshot exact.
    fn start_gc(&mut self, reply: Option<oneshot::Sender<IpcResponse>>) {
        if self.gc_running {
            if let Some(reply) = reply {
                let _ = reply.send(IpcResponse::err("busy", "gc refresh already running"));
            }
            return;
        }
        self.gc_running = true;
        let transfer = self.transfer.clone();
        let files = self.state.files.clone();
        let history = self.state.history.clone();
        let pulling = self.state.pulling.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let result = transfer
                .compute_live(&files, &history, &pulling)
                .await
                .map(|live| transfer.set_protected(live));
            let _ = events.send(Event::GcDone { result, reply }).await;
        });
    }

    fn status_json(&self) -> serde_json::Value {
        let now = Instant::now();
        let mut member_ids: BTreeSet<EndpointId> = self.members.keys().copied().collect();
        member_ids.extend(self.peers.keys().copied());
        member_ids.extend(
            self.state
                .known_members
                .values()
                .filter_map(|w| w.endpoint_id()),
        );
        member_ids.remove(&self.me);
        let members: Vec<serde_json::Value> = member_ids
            .into_iter()
            .map(|id| {
                let connected = self.peers.get(&id);
                let online = connected.is_some()
                    || self.members.get(&id).is_some_and(|m| {
                        now.duration_since(m.last_seen) <= crate::consts::ONLINE_WINDOW
                    });
                let (conn, rtt_ms) = match connected.and_then(|h| path_info(&h.conn)) {
                    Some((kind, rtt)) => (kind.to_string(), Some(rtt.as_millis() as u64)),
                    None => ("None".to_string(), None),
                };
                // Connected and past the initial index exchange — i.e. an
                // authenticated peer whose Index we have processed, so it can
                // participate as a lease voter.
                let synced = connected.is_some() && self.index_received.contains(&id);
                let health = self.peer_health.get(&id);
                let grade = health.map(|h| h.grade(now)).unwrap_or(HealthGrade::Offline);
                serde_json::json!({
                    "id": id.to_string(),
                    "name": self.state.peer_names.get(&id.to_string()),
                    "online": online,
                    "synced": synced,
                    "conn": conn,
                    "rtt_ms": rtt_ms,
                    "grade": grade.to_string(),
                    "rtt_jitter_ms": health.map(|h| h.rtt_jitter_ms).unwrap_or(0.0),
                    "path_changes": health.map(|h| h.path_changes).unwrap_or(0),
                    "flaps_per_min": health.map(|h| h.flaps_last_minute(now)).unwrap_or(0),
                    "relay_url": health.and_then(|h| h.relay_url.clone()),
                    "via_lan": health.is_some_and(|h| h.on_lan),
                    "rate_tx_bps": health.map(|h| h.rate_tx).unwrap_or(0.0),
                    "rate_rx_bps": health.map(|h| h.rate_rx).unwrap_or(0.0),
                    "bytes_tx": health.map(|h| h.bytes_tx).unwrap_or(0),
                    "bytes_rx": health.map(|h| h.bytes_rx).unwrap_or(0),
                    "time_to_direct_ms": health
                        .and_then(|h| h.time_to_direct)
                        .map(|d| d.as_millis() as u64),
                })
            })
            .collect();
        let leases: Vec<serde_json::Value> = self
            .locks
            .held_leases(now)
            .into_iter()
            .map(|(path, holder, _, left, age)| {
                let waiters: Vec<String> = self
                    .interest
                    .get(&path)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                serde_json::json!({
                    "path": path.as_str(),
                    "holder": holder,
                    "mine": holder == self.me_str,
                    "expires_in_ms": left.as_millis() as u64,
                    "age_ms": age.as_millis() as u64,
                    "waiters": waiters,
                })
            })
            .collect();
        // Paths this node is itself waiting for (behind another holder).
        let waiting: Vec<serde_json::Value> = self
            .my_waits
            .iter()
            .map(|(path, (holder, _))| {
                serde_json::json!({ "path": path.as_str(), "behind": holder })
            })
            .collect();
        let live_files: Vec<(&RelPath, &FileRecord)> = self
            .state
            .files
            .iter()
            .filter(|(_, r)| !r.deleted)
            .collect();
        // P20: cap the embedded file map so this single-line IPC/dashboard
        // payload can never overflow IPC_LINE_MAX on a big folder. The true
        // total rides along as `files_total`/`files_truncated`; the full index
        // still syncs via sharded IndexParts, and `file_count` stays exact.
        let (files_json, files_total, files_truncated) =
            files_json_capped(&self.state.files, crate::consts::FILES_LIST_MAX);
        // Transfer rows: one object per active pull, with live percentage
        // and average rate fed by the same meters that drive the bars.
        let pending_pulls: Vec<serde_json::Value> = self
            .pending_pulls
            .keys()
            .map(|p| {
                let meter = self.pull_meters.get(p);
                serde_json::json!({
                    "path": p.as_str(),
                    "percent": meter.map(|m| m.percent()).unwrap_or(0),
                    "bytes_done": meter.map(|m| m.bytes_done()).unwrap_or(0),
                    "bytes_total": meter.map(|m| m.bytes_total()).unwrap_or(0),
                    "rate_bytes_per_sec": meter.map(|m| m.rate()).unwrap_or(0),
                })
            })
            .collect();
        let events: Vec<serde_json::Value> = self
            .events_ring
            .iter()
            .map(|(seq, text)| serde_json::json!({ "seq": seq, "text": text }))
            .collect();
        serde_json::json!({
            "schema": 1,
            "id": self.me.to_string(),
            "dir": self.dir.display().to_string(),
            // P17: local peer labels (id → name), so a client can resolve the
            // ids in `leases`/`waiting`/`members` to friendly names.
            "names": self.state.peer_names,
            "members": members,
            "leases": leases,
            "waiting": waiting,
            "unapplied": self
                .state
                .unapplied
                .iter()
                .map(|(p, e)| serde_json::json!({ "path": p.as_str(), "reason": e.reason }))
                .collect::<Vec<_>>(),
            "held_local": self
                .held_local
                .iter()
                .map(|(p, reason)| serde_json::json!({ "path": p.as_str(), "reason": reason }))
                .collect::<Vec<_>>(),
            "pending_pulls": pending_pulls,
            // P15 transfer engine: live governor + resume/backlog depth.
            "transfer": {
                "download_limit_bps": self.state.config.max_down,
                "backlog": self.pull_backlog.len(),
                "resuming": self.state.pulling.len(),
                "swarm_peers": crate::consts::SWARM_PEERS,
            },
            "events": events,
            "file_count": live_files.len(),
            "total_bytes": live_files.iter().map(|(_, r)| r.size).sum::<u64>(),
            "files": files_json,
            "files_total": files_total,
            "files_truncated": files_truncated,
        })
    }

    // ---------------- timers ----------------

    fn on_sweep(&mut self) {
        let now = Instant::now();
        // Autolock idle-release: a lease auto-acquired on first write is let go
        // once it has been idle (no writes) for AUTOLOCK_IDLE_RELEASE.
        let idle: Vec<RelPath> = self
            .autolock_idle
            .iter()
            .filter(|(rel, deadline)| **deadline <= now && self.locks.is_held_by_me(rel))
            .map(|(rel, _)| rel.clone())
            .collect();
        for rel in idle {
            info!(path = %rel, "autolock lease idle past timeout; releasing");
            self.push_event(format!("autolock released {rel} (idle)"));
            self.release_own_lease(&rel);
        }
        self.autolock_idle.retain(|_, d| *d > now);

        let swept = self.locks.sweep(now);
        for (rel, holder) in swept.expired {
            if holder == self.me_str {
                warn!(path = %rel, "own lease expired without renewal; re-applying permissions");
                let p = rel.to_fs_path(&self.dir);
                let _ = if self.state.config.enforce_readonly() {
                    guard::set_readonly(&p)
                } else {
                    guard::set_writable(&p)
                };
            } else {
                info!(path = %rel, holder, "lease expired (holder silent past TTL)");
            }
            self.autolock_idle.remove(&rel);
            // Every node observing the expiry announces the path free so waiters
            // retry promptly (LockRelease is not sent on a silent-holder expiry).
            self.announce_freed(&rel);
        }
        // Expire our own waitlist entries.
        let expired_waits: Vec<RelPath> = self
            .my_waits
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(rel, _)| rel.clone())
            .collect();
        for rel in expired_waits {
            self.my_waits.remove(&rel);
            self.push_event(format!("gave up waiting for {rel} (wait-timeout)"));
        }
        for rel in swept.timed_out {
            self.broadcast(Msg::LockRelease { path: rel.clone() });
            let pending = self.pending_acquires.remove(&rel);
            if let Some((q, nf)) = self.autolock_pending.remove(&rel) {
                self.autolock_fail(&rel, q, nf, "REACHABILITY".to_string());
                continue;
            }
            if let Some((_, reply)) = pending {
                let _ = reply.send(IpcResponse::err(
                    "timeout",
                    "not every peer answered the lock request in time",
                ));
            }
        }
        // Presence-gap discrepancy: a live control connection keeps a peer
        // authoritatively online even when presence beacons lapse (control is
        // the source of truth). Note it at debug so the divergence is visible.
        for id in self.peers.keys() {
            if let Some(h) = self.peer_health.get(id)
                && now.duration_since(h.last_seen) > crate::consts::ONLINE_WINDOW
            {
                debug!(
                    peer = %id.fmt_short(),
                    "presence beacons lapsed but control connection is live; staying online"
                );
            }
        }
    }

    fn on_renew_tick(&mut self) {
        let now = Instant::now();
        for rel in self.locks.self_held_paths() {
            self.locks.renew_own(&rel, now);
            self.broadcast(Msg::LockRenew {
                path: rel,
                lamport: self.state.lamport,
                ttl_ms: self.timings.ttl.as_millis() as u64,
            });
        }
    }

    // ---------------- startup & shutdown ----------------

    fn startup_scan(&mut self) {
        let mut on_disk = Vec::new();
        walk_files(&self.dir, &self.dir, &mut on_disk);
        let genesis = self.state.files.is_empty()
            && self.state.known_members.is_empty()
            && self.state.history.is_empty();
        if genesis {
            for rel in on_disk {
                // Founding content passes the same sync-scope gate as
                // everything after it: held paths stay on disk, unpublished.
                if let Some(reason) = self.scope_hold_reason(&rel) {
                    info!(path = %rel, reason = %reason, "genesis: outside the sync scope; left alone");
                    self.held_local.insert(rel, reason);
                    continue;
                }
                info!(path = %rel, "genesis import");
                self.spawn_publish(rel, PublishCause::Import);
            }
            return;
        }
        let disk_set: BTreeSet<RelPath> = on_disk.iter().cloned().collect();
        for rel in &on_disk {
            // An unindexed on-disk path outside the sync scope is not a
            // violation — it is simply not session content. Leave it alone.
            if !self.state.files.get(rel).is_some_and(|r| !r.deleted)
                && let Some(reason) = self.scope_hold_reason(rel)
            {
                debug!(path = %rel, reason = %reason, "startup: outside sync scope; left alone");
                self.held_local.insert(rel.clone(), reason);
                continue;
            }
            match self.state.files.get(rel) {
                None => {
                    self.busy.insert(rel.clone());
                    self.violation_new_file(rel);
                    self.unbusy(rel);
                }
                Some(rec) if rec.deleted => {
                    self.busy.insert(rel.clone());
                    self.violation_new_file(rel);
                    self.unbusy(rel);
                }
                Some(rec) => {
                    let size_on_disk = std::fs::metadata(rel.to_fs_path(&self.dir))
                        .map(|m| m.len())
                        .unwrap_or(u64::MAX);
                    if size_on_disk != rec.size {
                        warn!(path = %rel, "offline modification detected at startup");
                        self.spawn_violation(rel.clone(), true, "offline-edit");
                    }
                }
            }
        }
        let missing: Vec<RelPath> = self
            .state
            .files
            .iter()
            .filter(|(p, r)| !r.deleted && !disk_set.contains(*p))
            .map(|(p, _)| p.clone())
            .collect();
        for rel in missing {
            warn!(path = %rel, "indexed file missing at startup; restoring");
            self.busy.insert(rel.clone());
            self.spawn_violation(rel, false, "");
        }
        // P15 resume: drop any pending-pull targets a prior run already
        // satisfied (the indexed record now dominates the pull's target). The
        // rest stay recorded so reconciliation re-drives them once the source
        // peer reconnects; their staged chunks are GC-protected meanwhile.
        let stale: Vec<RelPath> = self
            .state
            .pulling
            .iter()
            .filter(|(p, target)| {
                self.state.files.get(*p).is_some_and(|cur| {
                    matches!(
                        crate::sync::vclock::compare(&cur.vv, &target.vv),
                        crate::sync::vclock::Causality::Equal
                            | crate::sync::vclock::Causality::After
                    )
                })
            })
            .map(|(p, _)| p.clone())
            .collect();
        if !stale.is_empty() {
            for rel in stale {
                debug!(path = %rel, "startup: resume target already satisfied; clearing");
                self.state.pulling.remove(&rel);
            }
            self.persist();
        }
    }

    async fn graceful_shutdown(&mut self) {
        info!("shutting down");
        let strict = self.state.config.enforce_readonly();
        for rel in self.locks.self_held_paths() {
            let p = rel.to_fs_path(&self.dir);
            let _ = if strict {
                guard::set_readonly(&p)
            } else {
                guard::set_writable(&p)
            };
            self.broadcast(Msg::LockRelease { path: rel });
        }
        self.broadcast(Msg::Bye);
        // Give the writer tasks a moment to flush.
        tokio::time::sleep(Duration::from_millis(200)).await;
        for (_, h) in std::mem::take(&mut self.peers) {
            h.close();
        }
        self.persist();
        let _ = self.router.shutdown().await;
        self.transfer.shutdown().await;
        self.endpoint.close().await;
    }

    fn persist(&mut self) {
        if let Err(e) = self.state.save(&self.dir) {
            error!("failed to persist state: {e}");
        }
        // Keep the GC-protected snapshot in lockstep with committed state.
        if self.gc_running {
            self.gc_dirty = true;
        } else {
            self.start_gc(None);
        }
    }
}

/// Recursively lists session files (skipping `.tazamun` and non-files).
/// Reads `.tazamunignore` (missing file = no user rules) and compiles the
/// P11 sync-scope policy from it plus the persisted config.
fn load_ignore_set(
    dir: &Path,
    config: &crate::state::SessionConfig,
) -> crate::sync::ignore::IgnoreSet {
    let text =
        std::fs::read_to_string(dir.join(crate::sync::ignore::IGNORE_FILE)).unwrap_or_default();
    crate::sync::ignore::IgnoreSet::build(
        &text,
        config.junk_filter,
        &config.sync_only,
        &config.sync_skip,
        config.max_file_size,
    )
}

/// Whether an `unapplied` reason came from the P11 sync-scope verdicts (as
/// opposed to a non-portable-path reason) — used to release holds when the
/// rules relax. Matches the exact prefixes `Verdict::reason` produces.
/// P20 wire-boundary guard: reject a peer-supplied record whose version vector
/// is implausibly large (a corrupt or hostile advertisement trying to bloat our
/// stored vv via union-merge). A legitimate vv has one entry per distinct writer
/// — a handful — so [`MAX_VV_ENTRIES`](crate::consts::MAX_VV_ENTRIES) is very
/// generous; the cap keeps every stored/re-advertised record small enough to fit
/// a single frame, so index sharding and `FileMeta` can never self-brick.
fn record_acceptable(record: &FileRecord) -> bool {
    record.vv.len() <= crate::consts::MAX_VV_ENTRIES
}

/// P20: the capped `files` map for a status snapshot — at most `cap` entries
/// (the first `cap` by sorted path, since `files` is a `BTreeMap`) — plus the
/// true total and whether it was truncated. Pure, so the single-line-payload
/// bound is unit-tested without a running daemon.
pub fn files_json_capped(
    files: &BTreeMap<RelPath, FileRecord>,
    cap: usize,
) -> (serde_json::Map<String, serde_json::Value>, usize, bool) {
    let total = files.len();
    let map = files
        .iter()
        .take(cap)
        .map(|(p, r)| {
            (
                p.as_str().to_string(),
                serde_json::json!({ "size": r.size, "deleted": r.deleted, "vv": r.vv }),
            )
        })
        .collect();
    (map, total, total > cap)
}

fn is_scope_reason(reason: &str) -> bool {
    reason.starts_with("junk filter")
        || reason.starts_with("ignored by")
        || reason.starts_with("outside this node's")
        || reason.starts_with("file is ")
}

/// Caps an audit/hook/notify `detail` string to a bounded length on a char
/// boundary (never panics on multibyte input — a String::truncate mid-codepoint
/// aborts), with an ellipsis marker. P19: bounds the peer-offline lease list and
/// any other detail so no single line / hook payload / notification is unbounded.
fn cap_detail(mut d: String) -> String {
    const MAX: usize = 512;
    if d.len() > MAX {
        let mut end = MAX;
        while !d.is_char_boundary(end) {
            end -= 1;
        }
        d.truncate(end);
        d.push('…');
    }
    d
}

fn walk_files(root: &Path, current: &Path, out: &mut Vec<RelPath>) {
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(e) => {
            warn!("cannot scan {}: {e}", current.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().eq_ignore_ascii_case(META_DIR) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            walk_files(root, &path, out);
        } else if meta.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            let parts: Vec<String> = rel
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                    _ => None,
                })
                .collect();
            if let Ok(rel) = sanitize_rel_path(&parts.join("/")) {
                out.push(rel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cap_detail;

    #[test]
    fn cap_detail_never_panics_on_multibyte_at_the_boundary() {
        // ASCII shorter than the cap is untouched.
        assert_eq!(cap_detail("short".into()), "short");
        // A multibyte char (Arabic, emoji) straddling byte 512 must not panic
        // (String::truncate mid-codepoint would abort — this is the regression
        // an adversarial review caught).
        for filler in ["ب", "€", "😀"] {
            let mut s = "a".repeat(511);
            s.push_str(&filler.repeat(20));
            let capped = cap_detail(s);
            assert!(capped.len() <= 512 + '…'.len_utf8());
            assert!(capped.ends_with('…'));
            // Re-capping stays bounded and never panics.
            assert!(cap_detail(capped).len() <= 512 + '…'.len_utf8());
        }
    }
}
