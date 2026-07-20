//! P16 multi-session supervisor: one process, every registered folder.
//!
//! `tazamun start --all` hosts every registered, non-paused session in a single
//! process by spawning one daemon actor per folder. Each hosted session keeps
//! its own per-folder IPC socket, so every existing per-folder command
//! (`status`, `lock`, `versions`, …) works against a supervised session exactly
//! as it does against a standalone one — the supervisor is purely additive.
//!
//! A single device-global control socket answers the cross-session commands
//! that a per-folder socket cannot: pause, resume, and list. `tazamun pause
//! <dir>` can therefore suspend one folder live (graceful shutdown of just that
//! session) without disturbing the rest.
//!
//! The supervisor owns no session state. Each hosted [`DaemonHandle`] remains
//! the single writer for its folder, so the Golden Invariant and the three
//! lease preconditions are untouched — this is topology, not surgery. The
//! supervisor only holds the *set* of hosted sessions and mediates their
//! lifecycle (spawn / graceful shutdown).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::tokio::Stream as IpcStream;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::cli::{CliError, NetFlags};
use crate::daemon::DaemonConfig;
use crate::ipc::{self, IpcError, IpcResponse};
use crate::locks::LockTimings;
use crate::registry::Registry;
use crate::state::AppState;
use crate::ui::progress::Ui;

/// Requests carried over the device-global control socket. Distinct from the
/// per-folder [`ipc::IpcRequest`]: these operate on the *set* of sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "args", rename_all = "lowercase")]
pub enum ControlRequest {
    /// The supervisor's hosted and paused sets (for `tazamun ls` annotation).
    List,
    /// Pause a folder: persist the flag and gracefully stop its hosted session.
    Pause { path: String },
    /// Resume a folder: clear the flag and spawn its session if not hosted.
    Resume { path: String },
}

// ─── control socket address ──────────────────────────────────────────────────

/// Absolute path of the device-global control socket file (Unix).
#[cfg(unix)]
fn control_socket_file() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    runtime.join("tazamun-control.sock")
}

/// The device-global control socket name: one per user session on Unix (a file
/// in the runtime dir), a fixed namespaced pipe on Windows.
fn control_name() -> Result<Name<'static>, IpcError> {
    #[cfg(unix)]
    {
        Ok(control_socket_file()
            .to_fs_name::<GenericFilePath>()?
            .into_owned())
    }
    #[cfg(not(unix))]
    {
        Ok("tazamun-control"
            .to_ns_name::<GenericNamespaced>()?
            .into_owned())
    }
}

// ─── control client (used by `pause` / `resume` / `ls`) ──────────────────────

/// True when a supervisor is answering on the control socket.
pub async fn control_alive() -> bool {
    request(&ControlRequest::List, std::time::Duration::from_secs(2))
        .await
        .is_ok()
}

/// Sends one control request to a running supervisor and awaits the response.
pub async fn request(
    req: &ControlRequest,
    timeout: std::time::Duration,
) -> Result<IpcResponse, IpcError> {
    let name = control_name()?;
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
        let Some(line) = ipc::read_line_capped(&mut reader).await? else {
            return Err(IpcError::Protocol(
                "supervisor closed without replying".into(),
            ));
        };
        serde_json::from_str(&line).map_err(|e| IpcError::Protocol(e.to_string()))
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(res) => res,
        Err(_) => Err(IpcError::Protocol("control request timed out".into())),
    }
}

// ─── control listener ────────────────────────────────────────────────────────

/// Binds the control socket, replacing a dead Unix socket file if necessary.
/// Fails with [`IpcError::AlreadyRunning`] when a supervisor already answers.
async fn bind_control() -> Result<interprocess::local_socket::tokio::Listener, IpcError> {
    if control_alive().await {
        return Err(IpcError::AlreadyRunning);
    }
    #[cfg(unix)]
    {
        let file = control_socket_file();
        if file.exists() {
            debug!("removing stale control socket {}", file.display());
            let _ = std::fs::remove_file(&file);
        }
    }
    let listener = ListenerOptions::new()
        .name(control_name()?)
        .create_tokio()?;
    Ok(listener)
}

/// Serves control connections, forwarding each request to the supervisor actor
/// over `tx` and writing back the reply.
async fn serve_control(
    listener: interprocess::local_socket::tokio::Listener,
    tx: mpsc::Sender<(ControlRequest, oneshot::Sender<IpcResponse>)>,
) {
    loop {
        let stream = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("control accept: {e}");
                continue;
            }
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_control_conn(stream, tx).await {
                debug!("control conn ended: {e}");
            }
        });
    }
}

async fn serve_control_conn(
    stream: IpcStream,
    tx: mpsc::Sender<(ControlRequest, oneshot::Sender<IpcResponse>)>,
) -> Result<(), IpcError> {
    let (recv, mut send) = tokio::io::split(stream);
    let mut reader = BufReader::new(recv);
    while let Some(line) = ipc::read_line_capped(&mut reader).await? {
        if line.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<ControlRequest>(&line) {
            Ok(req) => {
                let (rtx, rrx) = oneshot::channel();
                if tx.send((req, rtx)).await.is_err() {
                    IpcResponse::err("shutting_down", "supervisor is shutting down")
                } else {
                    rrx.await.unwrap_or_else(|_| {
                        IpcResponse::err("internal", "supervisor dropped the request")
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

// ─── supervisor actor ────────────────────────────────────────────────────────

/// The single-process host for every session. Owns only the hosted set; every
/// hosted [`DaemonHandle`] is still the sole writer for its own folder.
struct Supervisor {
    /// Absolute folder path → its running daemon handle.
    hosted: BTreeMap<String, crate::daemon::DaemonHandle>,
    /// Per-run network overrides applied on top of each session's saved config.
    net: NetFlags,
}

impl Supervisor {
    /// Spawns a daemon for `dir` and records the handle. Idempotent: a folder
    /// already hosted is left alone. Errors (unreadable state, or a standalone
    /// daemon already holding the per-folder socket) propagate to the caller.
    async fn spawn_session(&mut self, dir: &Path) -> Result<(), CliError> {
        let abs = abs_string(dir);
        if self.hosted.contains_key(&abs) {
            return Ok(());
        }
        let saved = AppState::load(dir)?.config;
        let net = crate::cli::resolve_net_config(&saved, &self.net)?;
        let timings = LockTimings {
            ttl: saved.lease_ttl(),
            renew: saved.lease_renew(),
            acquire_timeout: saved.acquire_timeout(),
        };
        let cfg = DaemonConfig {
            dir: dir.to_path_buf(),
            net,
            timings,
            // Supervised sessions never draw progress bars — many folders share
            // one terminal, and unattended is the common case.
            ui: Ui::disabled(),
        };
        let handle = crate::daemon::spawn(cfg).await?;
        info!(folder = %dir.display(), peer = %handle.id(), "hosting session");
        self.hosted.insert(abs, handle);
        Ok(())
    }

    /// Gracefully stops one hosted session (releasing its leases), if hosted.
    async fn stop_session(&mut self, dir: &Path) -> bool {
        let abs = abs_string(dir);
        if let Some(handle) = self.hosted.remove(&abs) {
            info!(folder = %dir.display(), "stopping hosted session");
            handle.shutdown().await;
            true
        } else {
            false
        }
    }

    /// Handles one control request against the live hosted set.
    async fn handle_control(&mut self, req: ControlRequest) -> IpcResponse {
        match req {
            ControlRequest::List => {
                let reg = Registry::load();
                let hosted: Vec<String> = self.hosted.keys().cloned().collect();
                let paused: Vec<String> = reg
                    .sessions
                    .iter()
                    .filter(|s| s.paused)
                    .map(|s| s.path.clone())
                    .collect();
                IpcResponse::ok(serde_json::json!({ "hosted": hosted, "paused": paused }))
            }
            ControlRequest::Pause { path } => {
                let dir = PathBuf::from(&path);
                let mut reg = Registry::load();
                if reg.set_paused(&dir, true).is_none() {
                    return IpcResponse::err(
                        "unknown_session",
                        format!("{path} is not registered"),
                    );
                }
                let _ = reg.save();
                let stopped = self.stop_session(&dir).await;
                IpcResponse::ok(serde_json::json!({ "paused": path, "was_running": stopped }))
            }
            ControlRequest::Resume { path } => {
                let dir = PathBuf::from(&path);
                let mut reg = Registry::load();
                if reg.set_paused(&dir, false).is_none() {
                    return IpcResponse::err(
                        "unknown_session",
                        format!("{path} is not registered"),
                    );
                }
                let _ = reg.save();
                match self.spawn_session(&dir).await {
                    Ok(()) => {
                        IpcResponse::ok(serde_json::json!({ "resumed": path, "hosted": true }))
                    }
                    Err(e) => IpcResponse::err("spawn_failed", e.to_string()),
                }
            }
        }
    }

    /// Gracefully stops every hosted session.
    async fn shutdown_all(&mut self) {
        let handles = std::mem::take(&mut self.hosted);
        for (path, handle) in handles {
            debug!(folder = %path, "supervisor: shutting down session");
            handle.shutdown().await;
        }
    }
}

// ─── entry point ─────────────────────────────────────────────────────────────

/// Runs the multi-session supervisor until Ctrl-C: hosts every registered,
/// non-paused session in this process, serves the control socket, then shuts
/// every session down gracefully.
pub async fn run(net: NetFlags) -> Result<(), CliError> {
    // Bind the control socket first so a second `start --all` fails fast.
    let listener = bind_control().await.map_err(|e| match e {
        IpcError::AlreadyRunning => {
            CliError::Refused("a tazamun supervisor is already running on this device".into())
        }
        other => CliError::Ipc(other),
    })?;

    let mut reg = Registry::load();
    let pruned = reg.prune(AppState::exists);
    if !pruned.is_empty() {
        let _ = reg.save();
    }

    let (ctl_tx, mut ctl_rx) = mpsc::channel::<(ControlRequest, oneshot::Sender<IpcResponse>)>(32);
    let control_task = tokio::spawn(serve_control(listener, ctl_tx));

    let mut sup = Supervisor {
        hosted: BTreeMap::new(),
        net,
    };

    // Bring up every registered, non-paused, loadable session. A folder that
    // fails to host (already running standalone, unreadable state) is skipped
    // with a reason, never aborting the whole supervisor.
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut paused = 0usize;
    for s in &reg.sessions {
        if s.paused {
            paused += 1;
            continue;
        }
        let dir = PathBuf::from(&s.path);
        if let Err(e) = sup.spawn_session(&dir).await {
            warn!(folder = %s.path, error = %e, "supervisor: not hosting");
            skipped.push((s.path.clone(), e.to_string()));
        }
    }

    print_summary(&reg, sup.hosted.len(), paused, &skipped);

    // Serve control requests until Ctrl-C.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping: releasing every session's leases and saying goodbye…");
                break;
            }
            msg = ctl_rx.recv() => {
                match msg {
                    Some((req, reply)) => {
                        let resp = sup.handle_control(req).await;
                        let _ = reply.send(resp);
                    }
                    None => break,
                }
            }
        }
    }

    control_task.abort();
    sup.shutdown_all().await;
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(control_socket_file());
    }
    println!("Stopped cleanly.");
    Ok(())
}

/// Prints the one-screen startup summary for the supervisor.
fn print_summary(reg: &Registry, hosted: usize, paused: usize, skipped: &[(String, String)]) {
    println!("tazamun supervisor running (one daemon, many folders)");
    if reg.sessions.is_empty() {
        println!("  no registered sessions yet — `tazamun init` or `tazamun join tzm1…` first");
        println!("\nPress Ctrl-C to stop.");
        return;
    }
    println!(
        "  hosting: {hosted} session(s){}{}",
        if paused > 0 {
            format!(" · paused: {paused}")
        } else {
            String::new()
        },
        if !skipped.is_empty() {
            format!(" · skipped: {}", skipped.len())
        } else {
            String::new()
        }
    );
    for (path, reason) in skipped {
        println!("  skipped {path}: {reason}");
    }
    println!("  control: device-global socket (tazamun pause/resume/ls)");
    println!("\nPress Ctrl-C to stop. `tazamun ls` shows every folder from another shell.");
}

/// Absolute-path string key for the hosted map and registry lookups.
fn abs_string(dir: &Path) -> String {
    std::path::absolute(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_request_wire_format() {
        let s = serde_json::to_string(&ControlRequest::Pause {
            path: "/x/proj".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"op":"pause","args":{"path":"/x/proj"}}"#);
        let s = serde_json::to_string(&ControlRequest::List).unwrap();
        assert_eq!(s, r#"{"op":"list"}"#);
        let back: ControlRequest =
            serde_json::from_str(r#"{"op":"resume","args":{"path":"/y"}}"#).unwrap();
        assert!(matches!(back, ControlRequest::Resume { path } if path == "/y"));
    }

    #[test]
    fn control_name_resolves() {
        // Must never panic on any platform; the address is process-independent.
        assert!(control_name().is_ok());
    }
}
