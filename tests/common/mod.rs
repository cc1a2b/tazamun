//! Integration-test harness: fully offline in-process nodes.
//!
//! Every node binds to 127.0.0.1 with relays and address lookup disabled;
//! peers learn each other exclusively through explicit invite tickets, which
//! embed direct socket addresses.

#![allow(dead_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tazamun::daemon::{DaemonConfig, DaemonHandle, spawn};
use tazamun::ipc::{IpcRequest, IpcResponse};
use tazamun::locks::LockTimings;
use tazamun::net::control::{handshake_acceptor, handshake_initiator};
use tazamun::net::endpoint::{NetConfig, RelayChoice};
use tazamun::proto::{Msg, read_msg, write_msg};
use tazamun::session::{SessionKeys, SessionSecret, Ticket};

pub const TEST_BIND: &str = "127.0.0.1:0";

pub fn test_timings() -> LockTimings {
    LockTimings {
        ttl: Duration::from_secs(2),
        renew: Duration::from_millis(500),
        acquire_timeout: Duration::from_secs(3),
    }
}

pub fn test_net() -> NetConfig {
    NetConfig {
        relay: RelayChoice::Disabled,
        lan: false,
        airgap: false,
        test_bind: Some(TEST_BIND.parse::<SocketAddr>().expect("valid bind addr")),
        test_relay: None,
    }
}

pub struct TestNode {
    pub dir: tempfile::TempDir,
    pub handle: DaemonHandle,
}

impl TestNode {
    /// Initializes a fresh session in a temp folder and starts its daemon.
    /// Files written into the folder before this call become the genesis
    /// import.
    pub async fn init() -> TestNode {
        let dir = tempfile::tempdir().expect("tempdir");
        tazamun::cli::init(dir.path()).expect("init");
        Self::start(dir).await
    }

    /// Initializes the session folder but lets the caller stage files before
    /// the daemon starts.
    pub fn init_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        tazamun::cli::init(dir.path()).expect("init");
        dir
    }

    /// Joins an existing session from a ticket and starts the daemon.
    pub async fn join(ticket: &str) -> TestNode {
        let dir = tempfile::tempdir().expect("tempdir");
        tazamun::cli::join(dir.path(), ticket).expect("join");
        Self::start(dir).await
    }

    /// Starts a daemon over an already prepared session folder.
    pub async fn start(dir: tempfile::TempDir) -> TestNode {
        Self::start_with(dir, test_net()).await
    }

    /// Starts a daemon with a specific [`NetConfig`] (relay/airgap tests).
    pub async fn start_with(dir: tempfile::TempDir, net: NetConfig) -> TestNode {
        Self::start_with_timings(dir, net, test_timings()).await
    }

    /// Starts a daemon with explicit lease timings (lease-ergonomics tests that
    /// need a long TTL or a specific acquire window).
    pub async fn start_with_timings(
        dir: tempfile::TempDir,
        net: NetConfig,
        timings: LockTimings,
    ) -> TestNode {
        let handle = spawn(DaemonConfig {
            dir: dir.path().to_path_buf(),
            net,
            timings,
            ui: tazamun::ui::progress::Ui::disabled(),
        })
        .await
        .expect("daemon spawn");
        TestNode { dir, handle }
    }

    pub fn id(&self) -> EndpointId {
        self.handle.id()
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// A live invite ticket embedding this node's direct addresses.
    pub async fn invite(&self) -> String {
        let resp = self.handle.request(IpcRequest::Invite).await;
        assert!(resp.ok, "invite failed: {resp:?}");
        resp.data
            .and_then(|d| d.get("ticket").and_then(|t| t.as_str().map(String::from)))
            .expect("ticket in invite response")
    }

    pub async fn status(&self) -> serde_json::Value {
        let resp = self.handle.request(IpcRequest::Status).await;
        assert!(resp.ok, "status failed: {resp:?}");
        resp.data.expect("status data")
    }

    pub async fn lock(&self, path: &str) -> IpcResponse {
        self.handle
            .request(IpcRequest::Lock { path: path.into() })
            .await
    }

    pub async fn unlock(&self, path: &str) -> IpcResponse {
        self.handle
            .request(IpcRequest::Unlock { path: path.into() })
            .await
    }

    /// Sends `req`, retrying the daemon's explicitly-transient states
    /// (busy/syncing — "retry in a moment") for up to [`WAIT`], exactly as a
    /// real script would, so heavy in-flight publishes on slow CI don't flake.
    async fn request_retrying(&self, req: IpcRequest) -> IpcResponse {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let resp = self.handle.request(req.clone()).await;
            let transient = matches!(
                resp.error.as_ref().map(|e| e.code.as_str()),
                Some("busy") | Some("syncing")
            );
            if resp.ok || !transient || tokio::time::Instant::now() >= deadline {
                return resp;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Locks, asserting success (retries transient states).
    pub async fn lock_ok(&self, path: &str) {
        let resp = self
            .request_retrying(IpcRequest::Lock { path: path.into() })
            .await;
        assert!(resp.ok, "lock {path} failed: {resp:?}");
    }

    pub async fn unlock_ok(&self, path: &str) {
        let resp = self
            .request_retrying(IpcRequest::Unlock { path: path.into() })
            .await;
        assert!(resp.ok, "unlock {path} failed: {resp:?}");
    }

    /// Number of authenticated + online members reported by status.
    pub async fn online_peers(&self) -> usize {
        self.status().await["members"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter(|e| e["online"].as_bool() == Some(true))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Number of members with a live authenticated control connection
    /// (`conn` is Direct or Relayed, not None).
    pub async fn connected_peers(&self) -> usize {
        self.status().await["members"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter(|e| e["conn"].as_str().is_some_and(|c| c != "None"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Number of connected peers whose index we have received — i.e. peers that
    /// can act as lease voters (the FRESHNESS precondition is satisfiable).
    pub async fn synced_peers(&self) -> usize {
        self.status().await["members"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter(|e| e["synced"].as_bool() == Some(true))
                    .count()
            })
            .unwrap_or(0)
    }

    pub async fn file_count(&self) -> u64 {
        self.status().await["file_count"].as_u64().unwrap_or(0)
    }

    pub fn abs(&self, rel: &str) -> PathBuf {
        let mut p = self.dir.path().to_path_buf();
        for seg in rel.split('/') {
            p.push(seg);
        }
        p
    }

    pub fn write_file(&self, rel: &str, data: &[u8]) {
        let p = self.abs(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        // Leased files are writable; new files are simply created.
        std::fs::write(&p, data).expect("write");
    }

    pub fn read_file(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.abs(rel)).ok()
    }

    /// Force a write to a possibly read-only synced file — an un-leased edit,
    /// as if a user bypassed the lease. Clears the read-only bit first so the
    /// OS permits the write; the watcher then sees a real change.
    pub fn force_write(&self, rel: &str, data: &[u8]) {
        let p = self.abs(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        if let Ok(meta) = std::fs::metadata(&p) {
            let mut perms = meta.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o644);
            }
            #[cfg(not(unix))]
            {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
            }
            let _ = std::fs::set_permissions(&p, perms);
        }
        std::fs::write(&p, data).expect("force write");
    }

    /// Count quarantine copies preserved under `.tazamun/conflicts/`.
    pub fn conflict_count(&self) -> usize {
        std::fs::read_dir(self.dir.path().join(".tazamun").join("conflicts"))
            .map(|rd| rd.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    }

    /// The bytes of every quarantine copy under `.tazamun/conflicts/`.
    pub fn conflict_contents(&self) -> Vec<Vec<u8>> {
        std::fs::read_dir(self.dir.path().join(".tazamun").join("conflicts"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| std::fs::read(e.path()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn delete_file(&self, rel: &str) {
        std::fs::remove_file(self.abs(rel)).expect("remove");
    }

    pub fn is_readonly(&self, rel: &str) -> bool {
        std::fs::metadata(self.abs(rel))
            .map(|m| m.permissions().readonly())
            .unwrap_or(false)
    }

    pub fn blobs_dir_size(&self) -> u64 {
        dir_size(&self.dir.path().join(".tazamun").join("blobs"))
    }
}

/// Total byte size of every file under `path`, recursively.
/// Enables (or disables) autolock in a session folder's persisted config
/// before its daemon starts (the daemon reads config at spawn).
pub fn set_autolock(dir: &Path, on: bool) {
    let mut st = tazamun::state::AppState::load(dir).expect("load state");
    st.config.autolock = on;
    st.save(dir).expect("save state");
}

pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_size(&entry.path());
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Polls `pred` until it returns true or `timeout` elapses.
pub async fn wait_until<F, Fut>(mut pred: F, timeout: Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if pred().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Generous poll budget for convergence assertions. `wait_until` returns as
/// soon as the predicate holds, so a larger value only adds slack on slow or
/// loaded CI runners without slowing the passing path.
pub const WAIT: Duration = Duration::from_secs(30);

/// Longer budget for multi-node gossip mesh formation (three nodes discovering
/// one another through presence beacons).
pub const WAIT_MESH: Duration = Duration::from_secs(60);

/// Asserts both folders hold identical visible files (ignoring `.tazamun`).
pub async fn assert_converged(a: &TestNode, b: &TestNode) {
    let same = wait_until(
        || async { folder_snapshot(a.root()) == folder_snapshot(b.root()) },
        WAIT,
    )
    .await;
    assert!(
        same,
        "folders did not converge:\n a: {:?}\n b: {:?}",
        folder_snapshot(a.root()),
        folder_snapshot(b.root())
    );
}

/// Map of rel path → BLAKE3(content) for every visible file.
pub fn folder_snapshot(root: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    snapshot_walk(root, root, &mut out);
    out
}

fn snapshot_walk(
    root: &Path,
    current: &Path,
    out: &mut std::collections::BTreeMap<String, String>,
) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == ".tazamun" {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            snapshot_walk(root, &path, out);
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let hash = std::fs::read(&path)
                .map(|d| blake3::hash(&d).to_hex().to_string())
                .unwrap_or_else(|_| "unreadable".into());
            out.insert(rel, hash);
        }
    }
}

/// A protocol-level peer driven manually by tests: real endpoint, real
/// handshake, scripted messages. Used for mute-voter and hostile-input tests.
pub struct RawPeer {
    pub endpoint: Endpoint,
    pub conn: iroh::endpoint::Connection,
    pub send: iroh::endpoint::SendStream,
    pub recv: iroh::endpoint::RecvStream,
}

impl RawPeer {
    async fn build_endpoint() -> Endpoint {
        tazamun::net::endpoint::build_endpoint(iroh::SecretKey::generate(), &test_net())
            .await
            .expect("raw endpoint")
    }

    fn keys_from_ticket(ticket: &str) -> SessionKeys {
        let t = Ticket::decode(ticket).expect("decode ticket");
        SessionKeys::derive(&t.secret)
    }

    fn addr_from_ticket(ticket: &str) -> EndpointAddr {
        let t = Ticket::decode(ticket).expect("decode ticket");
        t.bootstrap
            .first()
            .and_then(|w| w.to_endpoint_addr())
            .expect("bootstrap addr in ticket")
    }

    /// Connects to the node behind `ticket` and completes the handshake with
    /// the correct session secret. Sends one empty Index so the node counts
    /// us as a synced voter.
    pub async fn connect_authed(ticket: &str) -> RawPeer {
        let endpoint = Self::build_endpoint().await;
        let keys = Self::keys_from_ticket(ticket);
        let addr = Self::addr_from_ticket(ticket);
        let conn = endpoint
            .connect(addr, tazamun::consts::CTL_ALPN)
            .await
            .expect("raw connect");
        let me = endpoint.id();
        let (mut send, recv) = handshake_initiator(&conn, &keys, me)
            .await
            .expect("raw handshake");
        write_msg(
            &mut send,
            &Msg::Index {
                lamport: 0,
                files: vec![],
                leases: vec![],
            },
        )
        .await
        .expect("send empty index");
        RawPeer {
            endpoint,
            conn,
            send,
            recv,
        }
    }

    /// Attempts the full initiator flow with a WRONG secret, deliberately
    /// pushing a forged proof to test the node's rejection. Returns whether
    /// an `Index` (i.e. post-auth data) was ever received.
    pub async fn probe_with_wrong_secret(ticket: &str) -> bool {
        let endpoint = Self::build_endpoint().await;
        let addr = Self::addr_from_ticket(ticket);
        let wrong = SessionKeys::derive(&SessionSecret([0xEE; 32]));
        let Ok(conn) = endpoint.connect(addr, tazamun::consts::CTL_ALPN).await else {
            return false;
        };
        let result = async {
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            let nonce: [u8; 16] = rand::random();
            write_msg(&mut send, &Msg::Hello { nonce }).await.ok()?;
            // Read the acceptor's HelloAck (we cannot verify it — wrong key —
            // and we do not care), then push a forged proof.
            let Msg::HelloAck { nonce: nonce_b, .. } =
                tokio::time::timeout(Duration::from_secs(5), read_msg(&mut recv))
                    .await
                    .ok()?
                    .ok()?
            else {
                return None;
            };
            let forged = tazamun::net::control::proof(
                &wrong.auth,
                b"init",
                &endpoint.id(),
                &conn.remote_id(),
                &nonce,
                &nonce_b,
            );
            write_msg(&mut send, &Msg::Proof { proof: forged })
                .await
                .ok()?;
            // If the node accepted us it now sends its Index.
            match tokio::time::timeout(Duration::from_secs(2), read_msg(&mut recv)).await {
                Ok(Ok(Msg::Index { .. })) => Some(true),
                _ => Some(false),
            }
        }
        .await;
        endpoint.close().await;
        result.unwrap_or(false)
    }

    /// Listens for one inbound control connection and answers the handshake
    /// with a WRONG secret. Returns whether the dialer ever completed the
    /// handshake (it must not).
    pub async fn accept_with_wrong_secret(endpoint: Endpoint) -> bool {
        let wrong = SessionKeys::derive(&SessionSecret([0xDD; 32]));
        let me = endpoint.id();
        let Some(incoming) = endpoint.accept().await else {
            return false;
        };
        let Ok(accepting) = incoming.accept() else {
            return false;
        };
        let Ok(conn) = accepting.await else {
            return false;
        };
        handshake_acceptor(&conn, &wrong, me).await.is_ok()
    }

    pub async fn send_msg(&mut self, msg: &Msg) {
        write_msg(&mut self.send, msg).await.expect("raw send");
    }

    /// Sends a raw frame header claiming `len` bytes followed by `body`.
    pub async fn send_raw_frame(
        &mut self,
        len: u32,
        body: &[u8],
    ) -> Result<(), iroh::endpoint::WriteError> {
        self.send.write_all(&len.to_be_bytes()).await?;
        self.send.write_all(body).await
    }

    /// Reads the next control message with a timeout; `None` on close/error.
    pub async fn recv_msg(&mut self, timeout: Duration) -> Option<Msg> {
        tokio::time::timeout(timeout, read_msg(&mut self.recv))
            .await
            .ok()?
            .ok()
    }

    pub fn close(&self) {
        self.conn
            .close(iroh::endpoint::VarInt::from_u32(0), b"test done");
    }

    /// Replay attack: complete one valid handshake and record the `Proof` bytes
    /// (and `nonce_a`), then open a second connection and replay that recorded
    /// proof against the node's *fresh* `nonce_b` (reusing the old `nonce_a` for
    /// the strongest replay). Returns `(first_authenticated, replay_rejected)`.
    /// A correct node accepts the first and rejects the replay because the proof
    /// binds *both* nonces.
    pub async fn capture_then_replay(ticket: &str) -> (bool, bool) {
        let endpoint = Self::build_endpoint().await;
        let keys = Self::keys_from_ticket(ticket);
        let addr = Self::addr_from_ticket(ticket);
        let me = endpoint.id();

        // Connection 1: a full, valid handshake. Record proof_old + nonce_a.
        let mut recorded: Option<([u8; 16], [u8; 32])> = None;
        let mut first_authed = false;
        if let Ok(conn) = endpoint
            .connect(addr.clone(), tazamun::consts::CTL_ALPN)
            .await
        {
            let remote = conn.remote_id();
            if let Ok((mut send, mut recv)) = conn.open_bi().await {
                let na: [u8; 16] = rand::random();
                if write_msg(&mut send, &Msg::Hello { nonce: na })
                    .await
                    .is_ok()
                    && let Ok(Msg::HelloAck { nonce: nb, .. }) = read_msg(&mut recv).await
                {
                    let mine =
                        tazamun::net::control::proof(&keys.auth, b"init", &me, &remote, &na, &nb);
                    if write_msg(&mut send, &Msg::Proof { proof: mine })
                        .await
                        .is_ok()
                    {
                        first_authed = matches!(
                            tokio::time::timeout(Duration::from_secs(3), read_msg(&mut recv)).await,
                            Ok(Ok(Msg::Index { .. }))
                        );
                        recorded = Some((na, mine));
                    }
                }
            }
            conn.close(iroh::endpoint::VarInt::from_u32(0), b"done");
        }
        let Some((na_old, proof_old)) = recorded else {
            endpoint.close().await;
            return (first_authed, true);
        };

        // Connection 2 (same identity, fresh session): replay proof_old.
        let mut replay_rejected = true;
        if let Ok(conn) = endpoint.connect(addr, tazamun::consts::CTL_ALPN).await {
            if let Ok((mut send, mut recv)) = conn.open_bi().await {
                // Reuse the old nonce_a; the node still sends a fresh nonce_b,
                // against which proof_old cannot verify.
                if write_msg(&mut send, &Msg::Hello { nonce: na_old })
                    .await
                    .is_ok()
                    && matches!(read_msg(&mut recv).await, Ok(Msg::HelloAck { .. }))
                    && write_msg(&mut send, &Msg::Proof { proof: proof_old })
                        .await
                        .is_ok()
                {
                    let accepted = matches!(
                        tokio::time::timeout(Duration::from_secs(2), read_msg(&mut recv)).await,
                        Ok(Ok(Msg::Index { .. }))
                    );
                    replay_rejected = !accepted;
                }
            }
            conn.close(iroh::endpoint::VarInt::from_u32(0), b"done");
        }
        endpoint.close().await;
        (first_authed, replay_rejected)
    }

    /// Drives one handshake up to the node's `HelloAck` and returns the node's
    /// `nonce_b`, so a test can sample many and assert freshness. Never
    /// completes the handshake.
    pub async fn capture_acceptor_nonce(ticket: &str) -> Option<[u8; 16]> {
        let endpoint = Self::build_endpoint().await;
        let addr = Self::addr_from_ticket(ticket);
        let conn = endpoint
            .connect(addr, tazamun::consts::CTL_ALPN)
            .await
            .ok()?;
        let nonce = async {
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            let na: [u8; 16] = rand::random();
            write_msg(&mut send, &Msg::Hello { nonce: na }).await.ok()?;
            match read_msg(&mut recv).await.ok()? {
                Msg::HelloAck { nonce, .. } => Some(nonce),
                _ => None,
            }
        }
        .await;
        conn.close(iroh::endpoint::VarInt::from_u32(0), b"done");
        endpoint.close().await;
        nonce
    }
}

/// Runs one control handshake between two fresh endpoints with the given keys
/// on each side, concurrently, and returns `(initiator_result, acceptor_result)`
/// as `Ok(())`/`Err(display)`. Lets the wrong-secret matrix assert both fail
/// closed with the generic error (no oracle).
pub async fn handshake_outcome(
    init_keys: SessionKeys,
    acc_keys: SessionKeys,
) -> (Result<(), String>, Result<(), String>) {
    let acc_ep = RawPeer::build_endpoint().await;
    let init_ep = RawPeer::build_endpoint().await;
    let acc_addr = acc_ep.addr();
    let acc_id = acc_ep.id();
    let init_id = init_ep.id();

    let acceptor = tokio::spawn(async move {
        let res = async {
            let Some(incoming) = acc_ep.accept().await else {
                return Err("no inbound connection".to_string());
            };
            let conn = incoming
                .accept()
                .map_err(|e| e.to_string())?
                .await
                .map_err(|e| e.to_string())?;
            handshake_acceptor(&conn, &acc_keys, acc_id)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        .await;
        acc_ep.close().await;
        res
    });

    // Hold the initiator's Connection open until the acceptor has finished:
    // handshake_initiator sends its Proof and returns immediately, so dropping
    // the connection right away would race the acceptor's read of that Proof
    // (a "connection lost" on the acceptor side). The real daemon keeps the
    // connection alive via PeerHandle; mirror that. On initiator failure, drop
    // early so the acceptor fails fast instead of waiting out the deadline.
    let (init_res, hold) = match init_ep.connect(acc_addr, tazamun::consts::CTL_ALPN).await {
        Ok(conn) => {
            let r = handshake_initiator(&conn, &init_keys, init_id)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
            let hold = if r.is_ok() { Some(conn) } else { None };
            (r, hold)
        }
        Err(e) => (Err(e.to_string()), None),
    };
    let acc_res = acceptor.await.unwrap_or_else(|e| Err(format!("join: {e}")));
    drop(hold);
    init_ep.close().await;
    (init_res, acc_res)
}

/// Seeds a prepared (not yet started) session folder with a known member, so
/// its daemon dials that address on startup.
pub fn seed_known_member(dir: &Path, id: EndpointId, addr: &EndpointAddr) {
    let mut state = tazamun::state::AppState::load(dir).expect("load state");
    state.known_members.insert(
        id.to_string(),
        tazamun::session::AddrWire::from_endpoint_addr(addr),
    );
    state.save(dir).expect("save state");
}

/// Deterministic pseudo-random bytes for large test files.
pub fn pseudo_random(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(n);
    out
}
