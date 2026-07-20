//! Local IPC between the CLI and a running daemon.
//!
//! Invariant: one JSON object per line, capped at
//! [`IPC_LINE_MAX`](crate::consts::IPC_LINE_MAX) bytes; the socket lives
//! inside `.tazamun/` (Unix) or the per-folder named pipe namespace
//! (Windows), so only local users with folder access can reach the daemon.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::tokio::Stream as IpcStream;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::consts::IPC_LINE_MAX;

/// Requests the CLI can issue over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "args", rename_all = "lowercase")]
pub enum IpcRequest {
    Status,
    Lock {
        path: String,
    },
    /// Register interest in a currently-held path so the daemon announces
    /// `LockInterest` to the holder and the requester is fast-woken on release.
    LockWait {
        path: String,
    },
    Unlock {
        path: String,
    },
    /// Mint an invite. `role`/`ttl_ms` apply on a v2 (role-enforcing) session;
    /// a legacy session ignores them and returns the plain ticket. Both `None`
    /// (the bare `Invite`) means an editor invite that never expires.
    Invite {
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        ttl_ms: Option<u64>,
    },
    Versions {
        path: String,
    },
    Restore {
        path: String,
        n: usize,
    },
    /// P14: name (or clear, with `name: None`) version `n` of a path.
    Tag {
        path: String,
        n: usize,
        name: Option<String>,
    },
    /// P14: pin/unpin version `n` so it survives depth pruning and GC.
    Pin {
        path: String,
        n: usize,
        pinned: bool,
    },
    /// P14: chunk-aware diff of the current file against version `n`.
    Diff {
        path: String,
        n: usize,
    },
    Gc,
    /// The daemon's contribution to `tazamun doctor`: identity, bound sockets,
    /// relay policy/status, and per-peer connectivity from telemetry.
    Doctor,
    /// Start the loopback web dashboard on demand (idempotent). The daemon does
    /// not bind it at startup, so nothing holds the port until `tazamun
    /// dashboard` asks — which sends this before reading `DashboardInfo`.
    DashboardStart,
    /// Dashboard bootstrap: the loopback port and session token the browser
    /// needs. Local-only, returned over the 0700 IPC socket.
    DashboardInfo,
    /// The full `api:1` dashboard snapshot (status schema-1 plus mode, config
    /// summary, conflicts, and per-path version counts).
    DashboardState,
    /// Set a live-settable config key through the running daemon (applies now
    /// where possible and persists). Non-network keys only.
    ConfigSet {
        key: String,
        value: String,
    },
    /// P17: set (or clear, with `name: None`) a local friendly label for a peer.
    /// `id` may be a short id prefix; the daemon resolves it to a known peer.
    PeerName {
        id: String,
        name: Option<String>,
    },
    /// P18: list quarantined copies (structured: reason, original path, size,
    /// age, suggested keep-both name) plus uncapped totals.
    Conflicts,
    /// P18: write the bytes of quarantined copy `id` into the working-tree
    /// path `target`, which must be leased by this node (exactly Restore's
    /// preconditions). Used by `conflicts resolve` for keep-mine (target =
    /// original path) and keep-both (target = a fresh conflict-named file).
    ConflictApply {
        id: String,
        target: String,
    },
    /// P18: delete ONE quarantined copy — the explicit `keep theirs` (or the
    /// final step after a successful keep-mine/keep-both publish). The only
    /// way a quarantined byte ever goes away besides `conflicts prune`.
    ConflictDiscard {
        id: String,
    },
    /// P21: gracefully stop this daemon (release leases, say goodbye, persist)
    /// — the programmatic equivalent of Ctrl-C, used by `tazamun gui`'s Stop
    /// button. The reply is sent before the shutdown begins.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcErrorBody>,
}

impl IpcResponse {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(code: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(IpcErrorBody {
                code: code.to_string(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("no running daemon for this folder (start one with `tazamun start`)")]
    NoDaemon,
    #[error("a daemon is already running for this folder")]
    AlreadyRunning,
    #[error(
        "this folder is on a filesystem that cannot host the sync daemon: \
         binding its control socket was refused.\n  On WSL this means a Windows \
         drive mounted over 9p (/mnt/c, /mnt/e, …), which supports neither Unix \
         sockets nor reliable change events.\n  Fix: keep the session on your \
         native Linux home (e.g. ~/tazamun/<folder>), or sync the Windows drive \
         with the native Windows build of tazamun as its own peer."
    )]
    Unsupported,
    #[error("ipc io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipc line exceeds {IPC_LINE_MAX} bytes")]
    LineTooLong,
    #[error("ipc protocol: {0}")]
    Protocol(String),
}

/// Unix: socket file inside `.tazamun/`. Windows: a named pipe derived from
/// the BLAKE3 hash of the absolute folder path.
pub fn socket_name(dir: &Path) -> Result<Name<'static>, IpcError> {
    #[cfg(unix)]
    {
        Ok(socket_file(dir)
            .to_fs_name::<GenericFilePath>()?
            .into_owned())
    }
    #[cfg(not(unix))]
    {
        let abs = std::path::absolute(dir)?;
        let digest = blake3::hash(abs.to_string_lossy().as_bytes());
        let hex = data_encoding::HEXLOWER.encode(&digest.as_bytes()[..4]);
        let name = format!("tazamun-{hex}");
        Ok(name.to_ns_name::<GenericNamespaced>()?.into_owned())
    }
}

/// `sockaddr_un` caps socket paths at ~107 bytes, so deeply nested session
/// folders cannot host the socket in `.tazamun/`. Both daemon and CLI derive
/// the same fallback deterministically from the absolute folder path.
#[cfg(unix)]
fn socket_file(dir: &Path) -> PathBuf {
    const SUN_PATH_BUDGET: usize = 100;
    let in_folder = crate::state::AppState::meta_dir(dir).join("daemon.sock");
    if in_folder.as_os_str().len() <= SUN_PATH_BUDGET {
        return in_folder;
    }
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = blake3::hash(abs.to_string_lossy().as_bytes());
    let hex = data_encoding::HEXLOWER.encode(&digest.as_bytes()[..8]);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    runtime.join(format!("tazamun-{hex}.sock"))
}

/// One line in, one line out, with the line cap enforced on read. Shared with
/// the device-global supervisor control socket ([`crate::supervisor`]).
pub(crate) async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<String>, IpcError> {
    let mut buf = Vec::new();
    let n = reader
        .take(IPC_LINE_MAX as u64 + 1)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > IPC_LINE_MAX {
        return Err(IpcError::LineTooLong);
    }
    Ok(Some(String::from_utf8_lossy(&buf).trim().to_string()))
}

/// Sends one request to the daemon serving `dir` and awaits the response.
pub async fn request(dir: &Path, req: &IpcRequest) -> Result<IpcResponse, IpcError> {
    request_with_timeout(dir, req, std::time::Duration::from_secs(30)).await
}

pub async fn request_with_timeout(
    dir: &Path,
    req: &IpcRequest,
    timeout: std::time::Duration,
) -> Result<IpcResponse, IpcError> {
    let name = socket_name(dir)?;
    let stream = match IpcStream::connect(name).await {
        Ok(s) => s,
        Err(_) => return Err(IpcError::NoDaemon),
    };
    let fut = async move {
        let (recv, mut send) = tokio::io::split(stream);
        let line = serde_json::to_string(req).map_err(|e| IpcError::Protocol(e.to_string()))?;
        send.write_all(line.as_bytes()).await?;
        send.write_all(b"\n").await?;
        send.flush().await?;
        let mut reader = BufReader::new(recv);
        let Some(line) = read_line_capped(&mut reader).await? else {
            return Err(IpcError::Protocol("daemon closed without replying".into()));
        };
        serde_json::from_str(&line).map_err(|e| IpcError::Protocol(e.to_string()))
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(res) => res,
        Err(_) => Err(IpcError::Protocol("request timed out".into())),
    }
}

/// Returns true when a live daemon answers on this folder's socket.
pub async fn daemon_alive(dir: &Path) -> bool {
    request_with_timeout(dir, &IpcRequest::Status, std::time::Duration::from_secs(2))
        .await
        .is_ok()
}

/// Binds the IPC listener, replacing a dead Unix socket file if necessary.
/// Fails with [`IpcError::AlreadyRunning`] when a live daemon answers.
pub async fn bind(dir: &Path) -> Result<IpcListener, IpcError> {
    if daemon_alive(dir).await {
        return Err(IpcError::AlreadyRunning);
    }
    #[cfg(unix)]
    {
        let file = socket_file(dir);
        if file.exists() {
            debug!("removing stale ipc socket {}", file.display());
            let _ = std::fs::remove_file(&file);
        }
    }
    let listener = ListenerOptions::new()
        .name(socket_name(dir)?)
        .create_tokio()
        .map_err(map_bind_error)?;
    Ok(IpcListener { listener })
}

/// A socket bind refused with `EOPNOTSUPP`/`ENOTSUP` (errno 95 on Linux) means
/// the underlying filesystem does not support Unix domain sockets at all —
/// 9p/drvfs (WSL's `/mnt/*`) being the case users actually hit. Turn the raw
/// errno into the guidance in [`IpcError::Unsupported`]; pass everything else
/// through unchanged.
fn map_bind_error(err: std::io::Error) -> IpcError {
    if socket_unsupported(&err) {
        IpcError::Unsupported
    } else {
        IpcError::Io(err)
    }
}

/// True when the error is the filesystem refusing a socket bind outright.
/// `EOPNOTSUPP` and `ENOTSUP` share value 95 on Linux; a few kernels report
/// `EPERM` (1) for the same condition on 9p, so both are treated as terminal.
pub fn socket_unsupported(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(95) | Some(1))
}

/// Probes whether `dir`'s filesystem can host the daemon's control socket, by
/// binding a throwaway socket beside where the real one would live and
/// immediately dropping it. `Ok(())` when the daemon can run here;
/// [`IpcError::Unsupported`] when the filesystem refuses. Cheap, side-effect
/// free (the probe file is removed), and the one honest way to tell a user at
/// `init` time that a session created here could never start.
pub fn probe_can_host(dir: &Path) -> Result<(), IpcError> {
    #[cfg(unix)]
    {
        let meta = crate::state::AppState::meta_dir(dir);
        std::fs::create_dir_all(&meta)?;
        let probe = meta.join(".daemon.sock.probe");
        let _ = std::fs::remove_file(&probe);
        let name = probe.as_os_str().to_fs_name::<GenericFilePath>()?;
        // The bind is what the probe tests; the listener is dropped here at end
        // of scope, releasing it, before the socket file is unlinked below.
        let outcome = match ListenerOptions::new().name(name).create_sync() {
            Ok(_listener) => Ok(()),
            Err(e) => Err(map_bind_error(e)),
        };
        let _ = std::fs::remove_file(&probe);
        outcome
    }
    #[cfg(not(unix))]
    {
        // Windows named pipes live in the kernel namespace, never on the
        // folder's filesystem, so there is nothing to probe.
        let _ = dir;
        Ok(())
    }
}

pub struct IpcListener {
    listener: interprocess::local_socket::tokio::Listener,
}

impl IpcListener {
    /// Serves connections forever, forwarding each request to the daemon via
    /// the event channel and writing back the reply.
    pub async fn serve(self, events: mpsc::Sender<(IpcRequest, oneshot::Sender<IpcResponse>)>) {
        loop {
            let stream = match self.listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("ipc accept: {e}");
                    continue;
                }
            };
            let events = events.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_conn(stream, events).await {
                    debug!("ipc conn ended: {e}");
                }
            });
        }
    }
}

async fn serve_conn(
    stream: IpcStream,
    events: mpsc::Sender<(IpcRequest, oneshot::Sender<IpcResponse>)>,
) -> Result<(), IpcError> {
    let (recv, mut send) = tokio::io::split(stream);
    let mut reader = BufReader::new(recv);
    while let Some(line) = read_line_capped(&mut reader).await? {
        if line.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<IpcRequest>(&line) {
            Ok(req) => {
                let (tx, rx) = oneshot::channel();
                if events.send((req, tx)).await.is_err() {
                    IpcResponse::err("shutting_down", "daemon is shutting down")
                } else {
                    rx.await.unwrap_or_else(|_| {
                        IpcResponse::err("internal", "daemon dropped the request")
                    })
                }
            }
            Err(e) => IpcResponse::err("bad_request", format!("unparseable request: {e}")),
        };
        let mut out =
            serde_json::to_string(&response).map_err(|e| IpcError::Protocol(e.to_string()))?;
        // Defense in depth (P20): never emit a line the client's read_line_capped
        // would reject as too long (which reads as a connection error). If a
        // response somehow exceeds the cap, send a small typed error instead.
        // In practice the status/dashboard payloads are bounded at the source.
        if out.len() >= IPC_LINE_MAX {
            let err = IpcResponse::err(
                "response_too_large",
                "the daemon's response exceeded the IPC line limit",
            );
            out = serde_json::to_string(&err).unwrap_or_else(|_| {
                r#"{"ok":false,"error":{"code":"response_too_large","message":"oversized"}}"#
                    .to_string()
            });
        }
        out.push('\n');
        send.write_all(out.as_bytes()).await?;
        send.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_unsupported_recognizes_the_filesystem_refusals() {
        // 9p/drvfs (WSL /mnt/*) reports EOPNOTSUPP == 95; a few kernels report
        // EPERM == 1 for the same condition. Both mean "no socket here".
        for errno in [95, 1] {
            assert!(socket_unsupported(&std::io::Error::from_raw_os_error(
                errno
            )));
        }
        // Ordinary errors are not this condition and must pass through as Io.
        for errno in [
            2,  /* ENOENT */
            13, /* EACCES */
            98, /* EADDRINUSE */
        ] {
            assert!(!socket_unsupported(&std::io::Error::from_raw_os_error(
                errno
            )));
        }
        assert!(!socket_unsupported(&std::io::Error::other(
            "not an os error"
        )));
    }

    #[test]
    fn a_normal_temp_dir_can_host_the_daemon() {
        // The probe must succeed on tmpfs/ext4 — the whole test suite runs on
        // one, so a false positive here would be caught immediately.
        let dir = tempfile::tempdir().unwrap();
        assert!(probe_can_host(dir.path()).is_ok());
    }

    #[test]
    fn request_wire_format_matches_spec() {
        let s = serde_json::to_string(&IpcRequest::Lock {
            path: "a.txt".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"op":"lock","args":{"path":"a.txt"}}"#);
        let s = serde_json::to_string(&IpcRequest::Status).unwrap();
        assert_eq!(s, r#"{"op":"status"}"#);
        let back: IpcRequest =
            serde_json::from_str(r#"{"op":"restore","args":{"path":"x","n":2}}"#).unwrap();
        assert!(matches!(back, IpcRequest::Restore { n: 2, .. }));
    }

    #[test]
    fn response_shape() {
        let r = IpcResponse::err("strict_offline", "no peers");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""ok":false"#));
        assert!(s.contains(r#""code":"strict_offline""#));
        let r = IpcResponse::ok(serde_json::json!({"x": 1}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""ok":true"#));
        assert!(!s.contains("error"));
    }
}
