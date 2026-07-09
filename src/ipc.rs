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
    Unlock {
        path: String,
    },
    Invite,
    Versions {
        path: String,
    },
    Restore {
        path: String,
        n: usize,
    },
    Gc,
    /// The daemon's contribution to `tazamun doctor`: identity, bound sockets,
    /// relay policy/status, and per-peer connectivity from telemetry.
    Doctor,
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

/// One line in, one line out, with the line cap enforced on read.
async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(
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
        .create_tokio()?;
    Ok(IpcListener { listener })
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
