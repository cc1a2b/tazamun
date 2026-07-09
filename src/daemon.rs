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
    },
    RestoreStaged {
        rel: RelPath,
        entry: VersionEntry,
        result: Result<Staged, TransferError>,
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
    Shutdown(oneshot::Sender<()>),
}

/// Accept handler for the control ALPN: handshake, then hand the peer to the
/// actor.
#[derive(Debug, Clone)]
struct CtlAccept {
    keys: std::sync::Arc<SessionKeys>,
    me: EndpointId,
    events: mpsc::Sender<Event>,
}

impl ProtocolHandler for CtlAccept {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
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
    aux: Vec<tokio::task::JoinHandle<()>>,
}

impl DaemonHandle {
    pub fn id(&self) -> EndpointId {
        self.id
    }

    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
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
    guard::enforce_all(&dir, &state).map_err(|e| {
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
    let router = Router::builder(endpoint.clone())
        .accept(
            crate::consts::CTL_ALPN,
            CtlAccept {
                keys: std::sync::Arc::new(keys.clone()),
                me,
                events: events_tx.clone(),
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

    // IPC server.
    let (ipc_tx, mut ipc_rx) = mpsc::channel::<(IpcRequest, oneshot::Sender<IpcResponse>)>(64);
    let ipc_task = tokio::spawn(ipc_listener.serve(ipc_tx));
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

    // Timers.
    let timer_task = {
        let events = events_tx.clone();
        let renew_every = cfg.timings.renew;
        tokio::spawn(async move {
            let mut sweep = tokio::time::interval(Duration::from_millis(250));
            let mut renew = tokio::time::interval(renew_every);
            let mut gc = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            gc.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first immediate ticks are harmless no-ops.
            loop {
                let ev = tokio::select! {
                    _ = sweep.tick() => Event::Sweep,
                    _ = renew.tick() => Event::Renew,
                    _ = gc.tick() => Event::GcTick,
                };
                if events.send(ev).await.is_err() {
                    break;
                }
            }
        })
    };

    let me_str = me.to_string();
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
        index_received: BTreeSet::new(),
        members: BTreeMap::new(),
        dialing: BTreeSet::new(),
        redial_scheduled: BTreeSet::new(),
        backoff: BTreeMap::new(),
        pending_pulls: BTreeMap::new(),
        pull_meters: BTreeMap::new(),
        deferred: BTreeMap::new(),
        pending_acquires: BTreeMap::new(),
        pending_unlocks: BTreeMap::new(),
        muted: HashMap::new(),
        busy: BTreeSet::new(),
        recheck: BTreeSet::new(),
        gc_running: false,
        gc_dirty: false,
        ui: cfg.ui.clone(),
        keys,
        events_tx: events_tx.clone(),
        member_cmds: member_cmds_tx,
        _watcher: watcher,
    };

    let actor_task = tokio::spawn(actor.run(events_rx));

    Ok(DaemonHandle {
        id: me,
        endpoint,
        events: events_tx,
        actor: actor_task,
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
    index_received: BTreeSet<EndpointId>,
    members: BTreeMap<EndpointId, MemberInfo>,
    dialing: BTreeSet<EndpointId>,
    redial_scheduled: BTreeSet<EndpointId>,
    backoff: BTreeMap<EndpointId, Duration>,
    pending_pulls: BTreeMap<RelPath, PullJob>,
    pull_meters: BTreeMap<RelPath, std::sync::Arc<Meter>>,
    deferred: BTreeMap<RelPath, DeferredApply>,
    pending_acquires: BTreeMap<RelPath, (u64, oneshot::Sender<IpcResponse>)>,
    pending_unlocks: BTreeMap<RelPath, ()>,
    muted: HashMap<RelPath, Instant>,
    busy: BTreeSet<RelPath>,
    recheck: BTreeSet<RelPath>,
    gc_running: bool,
    gc_dirty: bool,
    ui: Ui,
    keys: SessionKeys,
    events_tx: mpsc::Sender<Event>,
    member_cmds: mpsc::Sender<MemberCmd>,
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
                } => self.on_violation_staged(rel, result, quarantined),
                Event::RestoreStaged {
                    rel,
                    entry,
                    result,
                    reply,
                } => self.on_restore_staged(rel, entry, result, reply),
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
                Event::Shutdown(reply) => {
                    self.graceful_shutdown().await;
                    let _ = reply.send(());
                    return;
                }
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
        self.dialing.remove(&id);
        self.backoff.remove(&id);
        self.members
            .entry(id)
            .or_insert_with(|| MemberInfo {
                addr: EndpointAddr::new(id),
                last_seen: Instant::now(),
            })
            .last_seen = Instant::now();
        let index = self.build_index();
        handle.send(index);
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

    fn on_peer_gone(&mut self, id: EndpointId, conn_id: usize) {
        let current = self.peers.get(&id).map(|h| h.conn_id());
        if current != Some(conn_id) {
            return; // stale event from a replaced connection
        }
        if let Some(h) = self.peers.remove(&id) {
            h.close();
        }
        self.peer_index.remove(&id);
        self.index_received.remove(&id);
        info!(peer = %id.fmt_short(), "peer disconnected");
        let aborted = self.locks.on_peer_down(&id.to_string());
        for rel in aborted {
            self.broadcast(Msg::LockRelease { path: rel.clone() });
            if let Some((_, reply)) = self.pending_acquires.remove(&rel) {
                let _ = reply.send(IpcResponse::err(
                    "voter_lost",
                    "a peer disconnected while voting on the lease",
                ));
            }
        }
        if self.members.contains_key(&id) || self.known_member_addr(&id).is_some() {
            self.schedule_redial(id);
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
                self.members.insert(
                    id,
                    MemberInfo {
                        addr,
                        last_seen: Instant::now(),
                    },
                );
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

    fn build_index(&mut self) -> Msg {
        let now = Instant::now();
        let leases = self
            .locks
            .held_leases(now)
            .into_iter()
            .map(|(path, holder, lamport, left)| LeaseInfo {
                path,
                holder,
                lamport,
                expires_in_ms: left.as_millis() as u64,
            })
            .collect();
        Msg::Index {
            lamport: self.state.lamport,
            files: self
                .state
                .files
                .iter()
                .map(|(p, r)| (p.clone(), r.clone()))
                .collect(),
            leases,
        }
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
                let mut map = BTreeMap::new();
                for (path, record) in files {
                    match sanitize_rel_path(path.as_str()) {
                        Ok(clean) => {
                            map.insert(clean, record);
                        }
                        Err(e) => {
                            warn!(peer = %from.fmt_short(), path = %path, "dropping record with hostile path: {e}");
                        }
                    }
                }
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
                let decision =
                    self.locks
                        .on_remote_request(&rel, &from_str, lamport, ttl_ms, Instant::now());
                let reply = match decision {
                    Decision::Grant => Msg::LockGrant { path: rel },
                    Decision::GrantAndAbortMine => {
                        if let Some((_, waiter)) = self.pending_acquires.remove(&rel) {
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
                    if let Some((_, waiter)) = self.pending_acquires.remove(&rel) {
                        let _ = waiter.send(IpcResponse::ok(serde_json::json!({
                            "locked": rel.as_str(),
                            "ttl_ms": self.timings.ttl.as_millis() as u64,
                        })));
                    }
                    info!(path = %rel, "lease acquired");
                }
            }
            Msg::LockDeny { path, reason } => {
                let Ok(rel) = sanitize_rel_path(path.as_str()) else {
                    return;
                };
                if self.locks.on_deny(&rel) {
                    self.broadcast(Msg::LockRelease { path: rel.clone() });
                    if let Some((_, waiter)) = self.pending_acquires.remove(&rel) {
                        let response = match reason {
                            DenyReason::Held { by } => {
                                IpcResponse::err("lease_held", format!("lease is held by {by}"))
                            }
                            DenyReason::TieLost => IpcResponse::err(
                                "tie_lost",
                                "a concurrent request won the tie-break",
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
        }
    }

    fn reconcile(&mut self, from: EndpointId) {
        let Some(remote) = self.peer_index.get(&from) else {
            return;
        };
        let remote_vec: Vec<(RelPath, FileRecord)> =
            remote.iter().map(|(p, r)| (p.clone(), r.clone())).collect();
        let d = diff(&self.state.files, &remote_vec);
        let records: BTreeMap<RelPath, FileRecord> = remote_vec.into_iter().collect();
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

    fn reconcile_one(&mut self, from: EndpointId, rel: &RelPath, record: &FileRecord) {
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
                match guard::quarantine(&self.dir, rel) {
                    Ok(q) => warn!(path = %rel, "local copy quarantined at {}", q.display()),
                    Err(e) => warn!(path = %rel, "quarantine failed: {e}"),
                }
            }
            self.maybe_pull(rel, from, record);
        }
    }

    // ---------------- pulls ----------------

    fn maybe_pull(&mut self, rel: &RelPath, from: EndpointId, record: FileRecord) {
        if self.locks.is_held_by_me(rel) {
            return;
        }
        if record.deleted {
            self.apply_remote(rel.clone(), from, record, None);
            return;
        }
        if let Some(job) = self.pending_pulls.get_mut(rel) {
            if job.record.manifest == record.manifest && job.record.vv == record.vv {
                return; // duplicate
            }
            job.queued = Some((from, record));
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

    fn start_pull(&mut self, rel: RelPath, from: EndpointId, record: FileRecord) {
        let transfer = self.transfer.clone();
        let endpoint = self.endpoint.clone();
        let events = self.events_tx.clone();
        // Dial with the full known address so pulls work without discovery.
        let from_addr = self
            .known_member_addr(&from)
            .unwrap_or_else(|| EndpointAddr::new(from));
        let meter = self.ui.pull_meter(rel.as_str(), record.size);
        self.pull_meters.insert(rel.clone(), meter.clone());
        tokio::spawn(async move {
            let result = transfer
                .pull_stage(&endpoint, from_addr, &rel, &record, Some(meter))
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
        self.mute(&rel);
        let prev = self.state.files.get(&rel).cloned();
        if let Some(prev) = &prev {
            versions::push(&mut self.state, &rel, prev);
        }
        let merged_vv = match &prev {
            Some(p) => vclock::merge(&p.vv, &record.vv),
            None => record.vv.clone(),
        };
        let apply_result: Result<(), String> = (|| {
            if record.deleted {
                let _ = guard::set_writable(&abs);
                match std::fs::remove_file(&abs) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
                Ok(())
            } else {
                let staged = staged.ok_or_else(|| "missing staged file".to_string())?;
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                // Windows cannot rename over a read-only file.
                let _ = guard::set_writable(&abs);
                staged.temp.persist(&abs).map_err(|e| e.to_string())?;
                guard::set_readonly(&abs).map_err(|e| e.to_string())?;
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
                self.persist();
                info!(path = %rel, peer = %from.fmt_short(), deleted, "applied remote version");
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
                    self.finish_unlock(&rel, reply);
                }
                Inspection::Differs | Inspection::Unindexed => {
                    self.spawn_publish(rel, PublishCause::Unlock(reply));
                }
                Inspection::MissingIndexed => {
                    self.commit_tombstone(&rel);
                    self.unbusy(&rel);
                    self.finish_unlock(&rel, reply);
                }
            },
            InspectCause::Watch => match (leased, outcome) {
                (_, Inspection::Clean) => self.unbusy(&rel),
                (true, Inspection::Differs) | (true, Inspection::Unindexed) => {
                    self.spawn_publish(rel, PublishCause::Edit);
                }
                (true, Inspection::MissingIndexed) => {
                    self.commit_tombstone(&rel);
                    self.unbusy(&rel);
                }
                (false, Inspection::Differs) => self.spawn_violation(rel, true),
                (false, Inspection::MissingIndexed) => self.spawn_violation(rel, false),
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
            }
            drop(p.tags);
        }
        if let PublishCause::Unlock(reply) = cause {
            self.unbusy(&rel);
            self.finish_unlock(&rel, reply);
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
    /// broadcast.
    fn spawn_violation(&mut self, rel: RelPath, quarantine_first: bool) {
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
                match guard::quarantine(&dir, &rel) {
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
                })
                .await;
        });
    }

    fn on_violation_staged(
        &mut self,
        rel: RelPath,
        result: Result<Staged, TransferError>,
        quarantined: Option<PathBuf>,
    ) {
        match result {
            Ok(staged) => {
                let abs = rel.to_fs_path(&self.dir);
                self.mute(&rel);
                let restore: Result<(), String> = (|| {
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    let _ = guard::set_writable(&abs);
                    staged.temp.persist(&abs).map_err(|e| e.to_string())?;
                    guard::set_readonly(&abs).map_err(|e| e.to_string())?;
                    drop(staged.tags);
                    Ok(())
                })();
                match restore {
                    Ok(()) => {
                        let q = quarantined
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "-".to_string());
                        warn!(
                            path = %rel,
                            quarantine = %q,
                            "VIOLATION: un-leased change reverted; offending bytes quarantined. \
                             Lease the file first: tazamun lock {rel}"
                        );
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
        let quarantined = match guard::quarantine(&self.dir, rel) {
            Ok(q) => q.display().to_string(),
            Err(e) => {
                warn!(path = %rel, "quarantine failed: {e}");
                return; // never delete bytes we could not preserve
            }
        };
        self.mute(rel);
        if let Err(e) = std::fs::remove_file(&abs)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %rel, "could not remove un-leased new file: {e}");
        }
        warn!(
            path = %rel,
            quarantine = %quarantined,
            "VIOLATION: new file created without a lease was quarantined and removed. \
             Lock the path first: tazamun lock {rel}"
        );
    }

    // ---------------- IPC ----------------

    fn on_ipc(&mut self, req: IpcRequest, reply: oneshot::Sender<IpcResponse>) {
        match req {
            IpcRequest::Status => {
                let _ = reply.send(IpcResponse::ok(self.status_json()));
            }
            IpcRequest::Lock { path } => self.handle_lock(path, reply),
            IpcRequest::Unlock { path } => self.handle_unlock(path, reply),
            IpcRequest::Invite => {
                let ticket = Ticket::new(
                    SessionSecret(match self.state.session_secret_bytes() {
                        Ok(b) => b,
                        Err(e) => {
                            let _ = reply.send(IpcResponse::err("internal", e.to_string()));
                            return;
                        }
                    }),
                    vec![AddrWire::from_endpoint_addr(&self.endpoint.addr())],
                );
                let _ = reply.send(IpcResponse::ok(
                    serde_json::json!({ "ticket": ticket.encode() }),
                ));
            }
            IpcRequest::Versions { path } => {
                let Ok(rel) = sanitize_rel_path(&path) else {
                    let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
                    return;
                };
                let entries: Vec<serde_json::Value> = versions::list(&self.state, &rel)
                    .into_iter()
                    .map(|(n, ts, size)| {
                        serde_json::json!({
                            "n": n,
                            "ts": guard::utc_timestamp(ts),
                            "ts_ms": ts,
                            "size": size,
                        })
                    })
                    .collect();
                let _ = reply.send(IpcResponse::ok(serde_json::json!({
                    "path": rel.as_str(),
                    "versions": entries,
                })));
            }
            IpcRequest::Restore { path, n } => self.handle_restore(path, n, reply),
            IpcRequest::Gc => self.start_gc(Some(reply)),
        }
    }

    fn strict_edit_guard(&self, rel: &RelPath) -> Option<IpcResponse> {
        if self.peers.is_empty() {
            return Some(IpcResponse::err(
                "strict_offline",
                "strict mode: no authenticated peer is connected, so edits are refused",
            ));
        }
        for id in self.peers.keys() {
            if !self.index_received.contains(id) {
                return Some(IpcResponse::err(
                    "syncing",
                    "still exchanging indexes with a peer; retry in a moment",
                ));
            }
        }
        if self.pending_pulls.contains_key(rel) || self.deferred.contains_key(rel) {
            return Some(IpcResponse::err(
                "not_fresh",
                "a newer version of this path is still being pulled",
            ));
        }
        if self.busy.contains(rel) {
            return Some(IpcResponse::err(
                "busy",
                "an operation on this path is in progress; retry in a moment",
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
                        return Some(IpcResponse::err(
                            "not_fresh",
                            format!("peer {} advertises a newer version", id.fmt_short()),
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
        if self.locks.is_held_by_me(&rel) {
            let _ = reply.send(IpcResponse::ok(serde_json::json!({
                "locked": rel.as_str(),
                "already": true,
            })));
            return;
        }
        if let Some(holder) = self.locks.holder(&rel) {
            let _ = reply.send(IpcResponse::err(
                "lease_held",
                format!("lease is held by {holder}"),
            ));
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
        self.locks.on_deny(&rel);
        self.broadcast(Msg::LockRelease { path: rel.clone() });
        if let Some((_, reply)) = self.pending_acquires.remove(&rel) {
            let _ = reply.send(IpcResponse::err(
                "timeout",
                "not every peer answered the lock request in time",
            ));
        }
    }

    fn handle_unlock(&mut self, path: String, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
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

    fn finish_unlock(&mut self, rel: &RelPath, reply: oneshot::Sender<IpcResponse>) {
        self.pending_unlocks.remove(rel);
        let me = self.me_str.clone();
        self.locks.on_release(rel, &me);
        let abs = rel.to_fs_path(&self.dir);
        if let Err(e) = guard::set_readonly(&abs) {
            warn!(path = %rel, "could not re-apply read-only: {e}");
        }
        self.broadcast(Msg::LockRelease { path: rel.clone() });
        info!(path = %rel, "lease released");
        let _ = reply.send(IpcResponse::ok(
            serde_json::json!({ "unlocked": rel.as_str() }),
        ));
    }

    fn handle_restore(&mut self, path: String, n: usize, reply: oneshot::Sender<IpcResponse>) {
        let Ok(rel) = sanitize_rel_path(&path) else {
            let _ = reply.send(IpcResponse::err("bad_path", "invalid relative path"));
            return;
        };
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
            staged.temp.persist(&abs).map_err(|e| e.to_string())?;
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
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let result = transfer
                .compute_live(&files, &history)
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
                serde_json::json!({
                    "id": id.to_string(),
                    "online": online,
                    "synced": synced,
                    "conn": conn,
                    "rtt_ms": rtt_ms,
                })
            })
            .collect();
        let leases: Vec<serde_json::Value> = self
            .locks
            .held_leases(now)
            .into_iter()
            .map(|(path, holder, _, left)| {
                serde_json::json!({
                    "path": path.as_str(),
                    "holder": holder,
                    "mine": holder == self.me_str,
                    "expires_in_ms": left.as_millis() as u64,
                })
            })
            .collect();
        let live_files: Vec<(&RelPath, &FileRecord)> = self
            .state
            .files
            .iter()
            .filter(|(_, r)| !r.deleted)
            .collect();
        let files_json: serde_json::Map<String, serde_json::Value> = self
            .state
            .files
            .iter()
            .map(|(p, r)| {
                (
                    p.as_str().to_string(),
                    serde_json::json!({ "size": r.size, "deleted": r.deleted, "vv": r.vv }),
                )
            })
            .collect();
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
        serde_json::json!({
            "id": self.me.to_string(),
            "dir": self.dir.display().to_string(),
            "members": members,
            "leases": leases,
            "pending_pulls": pending_pulls,
            "file_count": live_files.len(),
            "total_bytes": live_files.iter().map(|(_, r)| r.size).sum::<u64>(),
            "files": files_json,
        })
    }

    // ---------------- timers ----------------

    fn on_sweep(&mut self) {
        let now = Instant::now();
        let swept = self.locks.sweep(now);
        for (rel, holder) in swept.expired {
            if holder == self.me_str {
                warn!(path = %rel, "own lease expired without renewal; re-applying read-only");
                let _ = guard::set_readonly(&rel.to_fs_path(&self.dir));
            } else {
                info!(path = %rel, holder, "lease expired (holder silent past TTL)");
            }
        }
        for rel in swept.timed_out {
            self.broadcast(Msg::LockRelease { path: rel.clone() });
            if let Some((_, reply)) = self.pending_acquires.remove(&rel) {
                let _ = reply.send(IpcResponse::err(
                    "timeout",
                    "not every peer answered the lock request in time",
                ));
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
                info!(path = %rel, "genesis import");
                self.spawn_publish(rel, PublishCause::Import);
            }
            return;
        }
        let disk_set: BTreeSet<RelPath> = on_disk.iter().cloned().collect();
        for rel in &on_disk {
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
                        self.spawn_violation(rel.clone(), true);
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
            self.spawn_violation(rel, false);
        }
    }

    async fn graceful_shutdown(&mut self) {
        info!("shutting down");
        for rel in self.locks.self_held_paths() {
            let _ = guard::set_readonly(&rel.to_fs_path(&self.dir));
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
