//! P22: native desktop GUI (egui/eframe). A real application window — not a
//! loopback web page — that runs on Windows, macOS, and Linux and compiles into
//! the single `tazamun` binary (no webview, no runtime, no npm).
//!
//! Architecture: eframe owns the main (UI) thread and calls [`App::ui`] each
//! repaint. All I/O — the async iroh daemons, IPC sockets, the device registry —
//! runs on a background Tokio runtime. The UI and the worker communicate through
//! a command channel (UI → worker) and a shared, mutex-guarded snapshot (worker
//! → UI); the worker calls [`egui::Context::request_repaint`] whenever fresh data
//! lands. Every mutation the UI can trigger is forwarded to the target folder's
//! daemon over its existing IPC socket, so the daemon's lease-checked handlers
//! stay the only code that moves user bytes (the Golden Invariant holds exactly
//! as it does for the CLI and dashboard).

mod a11y;
mod balance;
mod ceremony;
mod chrome;
mod colophon;
mod components;
mod constellation;
mod controls;
mod copy;
mod dropzone;
mod fields;
mod figures;
mod focusnav;
mod folderpick;
mod grouping;
mod health;
mod marginalia;
mod onboarding;
mod ornament;
mod prefs;
mod rhythm;
mod selection;
mod shortcuts;
mod statusbar;
mod sysopen;
mod telemetry;
mod theme;
mod toasts;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use egui::containers::{CentralPanel, Panel};
use tokio::sync::mpsc;

use crate::cli::CliError;
use crate::daemon::DaemonHandle;
use crate::ipc::{self, IpcRequest};
use crate::registry::{Registry, SessionKind};
use crate::state::AppState;

use theme::{BAD, DIM, GOLD as ACCENT, GOOD, INK, WARN};

const REFRESH: Duration = Duration::from_millis(1500);

/// Seconds between preference writes while something keeps changing.
const PREFS_DEBOUNCE: f64 = 1.5;

/// How long the peer sky takes to draw itself on.
const SKY_REVEAL: f64 = 0.5;
/// Upper bound on any single graceful-shutdown await, so a wedged actor cannot
/// freeze the worker (on stop) or hang process exit (on teardown).
const GUI_SHUTDOWN: Duration = Duration::from_secs(10);

// ─── typed data model (worker → UI snapshot) ─────────────────────────────────

#[derive(Clone, Default)]
struct Overview {
    version: String,
    supervisor: bool,
    sessions: Vec<SessionRow>,
}

#[derive(Clone)]
struct SessionRow {
    path: String,
    name: String,
    running: bool,
    paused: bool,
    hosted_by_gui: bool,
    readable: bool,
    role: String,
    strict: bool,
    files: usize,
    total_bytes: u64,
    conflicts: usize,
    peers_online: usize,
    peers_total: usize,
    id_short: String,
}

#[derive(Clone, Default)]
struct Detail {
    dir: String,
    running: bool,
    role: String,
    strict: bool,
    invite: Option<String>,
    members: Vec<Member>,
    files: Vec<FileRow>,
    files_total: usize,
    files_truncated: bool,
    conflicts: Vec<ConflictRow>,
    leases: Vec<LeaseRow>,
    audit: Vec<AuditRow>,
    config: Option<ConfigView>,
    versions: BTreeMap<String, Vec<VersionRow>>,
    pulls: Vec<PullRow>,
    backlog: usize,
    resuming: usize,
    download_limit_bps: u64,
    events: Vec<EventRow>,
    error: Option<String>,
}

/// The daemon's config summary (from the DashboardState payload).
#[derive(Clone, Default)]
struct ConfigView {
    autolock: bool,
    strict: bool,
    role: String,
    update_channel: String,
    lease_ttl_ms: u64,
    acquire_timeout_ms: u64,
    wait_timeout_ms: u64,
    dashboard_port: u16,
    relay: Option<String>,
    lan: bool,
    max_down: u64,
}

#[derive(Clone)]
struct VersionRow {
    n: u64,
    ts_ms: u64,
    size: u64,
    tag: Option<String>,
    pinned: bool,
}

#[derive(Clone)]
struct PullRow {
    path: String,
    percent: u64,
    bytes_done: u64,
    bytes_total: u64,
    rate: u64,
}

#[derive(Clone)]
struct EventRow {
    text: String,
}

#[derive(Clone)]
struct Member {
    id_short: String,
    name: Option<String>,
    online: bool,
    grade: String,
    conn: String,
    rtt_ms: Option<u64>,
    via_lan: bool,
    jitter_ms: f64,
    rate_tx: u64,
    rate_rx: u64,
    bytes_tx: u64,
    bytes_rx: u64,
    relay_url: Option<String>,
    ttd_ms: Option<u64>,
    flaps: u64,
}

#[derive(Clone)]
struct FileRow {
    path: String,
    size: u64,
    locked_by: Option<String>,
    mine_lock: bool,
}

#[derive(Clone)]
struct ConflictRow {
    name: String,
    path: String,
    reason: String,
    ts_ms: u64,
    size: u64,
}

#[derive(Clone)]
struct LeaseRow {
    path: String,
    holder: String,
    mine: bool,
    expires_in_ms: u64,
}

#[derive(Clone)]
struct AuditRow {
    ts_ms: u64,
    kind: String,
    path: Option<String>,
    peer: Option<String>,
    detail: Option<String>,
}

/// The worker → UI snapshot, plus any toasts the UI has not drained yet.
#[derive(Default)]
struct Shared {
    overview: Option<Overview>,
    detail: Option<Detail>,
    /// A queue, not a slot: a bulk action produces several messages between
    /// two UI frames, and a slot would keep only the last of them.
    toasts: Vec<Toast>,
    picked: Option<(PickTarget, String)>,
    /// Bumped once per completed refresh so the UI can sample telemetry
    /// per poll, not per frame.
    tick: u64,
    busy: bool,
}

#[derive(Clone)]
struct Toast {
    text: String,
    error: bool,
}

/// Which text field a native folder-picker result lands in.
#[derive(Clone, Copy)]
enum PickTarget {
    Init,
    Join,
}

// ─── commands (UI → worker) ──────────────────────────────────────────────────

enum Cmd {
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
}

// ─── entry point ─────────────────────────────────────────────────────────────

/// `tazamun gui` entry point: open the native window and run until it is closed.
pub fn run() -> Result<(), CliError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::Refused(format!("could not start the async runtime: {e}")))?;

    let (tx, rx) = mpsc::unbounded_channel::<Cmd>();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let started: Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>> =
        Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));

    let handle = rt.handle().clone();
    let worker_shared = shared.clone();
    let worker_started = started.clone();
    let app_tx = tx.clone();

    let saved = prefs::load();
    let size = saved.window.unwrap_or([1180.0, 760.0]);

    // Frameless + transparent: the OS chrome is off and `chrome.rs` draws the
    // whole window — a rounded, self-decorated container on every platform.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_maximized(saved.maximized)
            .with_min_inner_size([820.0, 520.0])
            .with_app_id("tazamun")
            .with_title("tazamun")
            .with_decorations(false)
            .with_transparent(true)
            .with_icon(chrome::window_icon()),
        ..Default::default()
    };

    // The worker's JoinHandle is stashed so teardown can wait for it to drain any
    // command queued just before the window closed (e.g. a Start) before sweeping.
    let worker_join: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(None));
    let worker_join_setter = worker_join.clone();

    let run_result = eframe::run_native(
        "tazamun",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            theme::install(&ctx);
            let jh = handle.spawn(worker(rx, worker_shared, worker_started, ctx));
            if let Ok(mut slot) = worker_join_setter.lock() {
                *slot = Some(jh);
            }
            Ok(Box::new(App::new(app_tx, shared, saved)) as Box<dyn eframe::App>)
        }),
    );

    // Window closed: stop the worker, then gracefully shut down GUI-hosted
    // sessions. Every await is bounded so a wedged actor can't hang the exit.
    let _ = tx.send(Cmd::Quit);
    let worker_jh = worker_join.lock().ok().and_then(|mut s| s.take());
    rt.block_on(async {
        if let Some(jh) = worker_jh {
            let _ = tokio::time::timeout(GUI_SHUTDOWN, jh).await;
        }
        let mut hosted = started.lock().await;
        for (path, h) in std::mem::take(&mut *hosted) {
            tracing::debug!(session = %path, "gui: shutting down hosted session");
            let _ = tokio::time::timeout(GUI_SHUTDOWN, h.shutdown()).await;
        }
    });
    // The teardown above is GUI_SHUTDOWN-bounded; do not let the runtime's drop
    // wait indefinitely on any straggling blocking work after that.
    rt.shutdown_background();
    run_result.map_err(|e| {
        // A window that will not open is almost always the GL stack, and the
        // bare winit/glow message says nothing a user can act on.
        let hint = if sysopen::is_wsl() {
            "\n  on WSL this usually means WSLg is not running or the GL driver \
             fell back and failed — try `wsl --update` from Windows, or set \
             LIBGL_ALWAYS_SOFTWARE=1 to force software rendering"
        } else if std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none()
        {
            "\n  no display server was found (DISPLAY and WAYLAND_DISPLAY are \
             both unset) — the GUI needs a desktop session; over SSH try \
             `tazamun dashboard` instead"
        } else {
            "\n  this is usually the OpenGL driver — try LIBGL_ALWAYS_SOFTWARE=1 \
             to force software rendering"
        };
        CliError::Refused(format!("could not open the GUI window: {e}{hint}"))
    })
}

// ─── async worker ────────────────────────────────────────────────────────────

async fn worker(
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    shared: Arc<Mutex<Shared>>,
    started: Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    ctx: egui::Context,
) {
    let mut selected: Option<PathBuf> = None;
    let picking = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Prime the overview immediately so the window isn't blank on first paint.
    refresh(&shared, &started, &selected).await;
    ctx.request_repaint();

    loop {
        let cmd = tokio::select! {
            c = rx.recv() => match c { Some(c) => c, None => break },
            _ = tokio::time::sleep(REFRESH) => Cmd::Refresh,
        };

        set_busy(&shared, true);
        ctx.request_repaint();

        match cmd {
            Cmd::Quit => break,
            Cmd::Refresh => {}
            Cmd::Select(d) => selected = d,
            Cmd::Lock { dir, path } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::Lock { path },
                    "locked",
                    "lock refused",
                )
                .await
            }
            Cmd::Unlock { dir, path } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::Unlock { path },
                    "unlocked (published)",
                    "unlock failed",
                )
                .await
            }
            Cmd::ConfigSet { dir, key, value } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::ConfigSet { key, value },
                    "setting saved",
                    "could not set",
                )
                .await
            }
            Cmd::ResolveMine { dir, id, target } => {
                resolve_keep_mine(&shared, &dir, &id, &target).await
            }
            Cmd::Restore { dir, path, n } => restore_guided(&shared, &dir, &path, n).await,
            Cmd::Tag { dir, path, n, name } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::Tag { path, n, name },
                    "tag saved",
                    "tag failed",
                )
                .await
            }
            Cmd::Pin {
                dir,
                path,
                n,
                pinned,
            } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::Pin { path, n, pinned },
                    if pinned { "pinned" } else { "unpinned" },
                    "pin failed",
                )
                .await
            }
            Cmd::ConflictDiscard { dir, id } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::ConflictDiscard { id },
                    "discarded",
                    "discard failed",
                )
                .await
            }
            Cmd::PeerName { dir, id, name } => {
                ipc_action(
                    &shared,
                    &dir,
                    IpcRequest::PeerName { id, name },
                    "peer name saved",
                    "could not name peer",
                )
                .await
            }
            Cmd::Start(dir) => start_session(&shared, &started, &dir).await,
            Cmd::Stop(dir) => stop_session(&shared, &started, &dir).await,
            Cmd::Pause(dir) => set_paused(&shared, &dir, true).await,
            Cmd::Resume(dir) => set_paused(&shared, &dir, false).await,
            Cmd::Init(dir) => match crate::cli::init(&dir) {
                Ok(()) => {
                    selected = Some(absolute(&dir));
                    toast(&shared, format!("created {}", dir.display()), false);
                }
                Err(e) => toast(&shared, format!("init failed: {e}"), true),
            },
            Cmd::PickFolder(target) => {
                // The portal/native dialog is blocking — run it on a DETACHED
                // std thread, not the runtime's blocking pool: dropping a tokio
                // runtime waits indefinitely for spawn_blocking tasks, so a
                // dialog left open would hang process exit. A plain thread dies
                // with the process. One dialog at a time (double-click guard).
                if picking
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    let shared2 = shared.clone();
                    let ctx2 = ctx.clone();
                    let picking2 = picking.clone();
                    std::thread::spawn(move || {
                        match folderpick::pick("Choose a folder to sync") {
                            Ok(p) => {
                                if let Ok(mut s) = shared2.lock() {
                                    s.picked = Some((target, p.to_string_lossy().into_owned()));
                                }
                            }
                            // Closing the dialog is not an error.
                            Err(folderpick::PickError::Cancelled) => {}
                            // Anything else must be said out loud: a dialog that
                            // cannot open looks exactly like a broken button.
                            Err(folderpick::PickError::NoBackend(why)) => {
                                toast(&shared2, why, true);
                            }
                        }
                        picking2.store(false, std::sync::atomic::Ordering::SeqCst);
                        ctx2.request_repaint();
                    });
                }
            }
            Cmd::Join(dir, ticket) => match crate::cli::join(&dir, ticket.trim()) {
                Ok(()) => {
                    selected = Some(absolute(&dir));
                    toast(&shared, format!("joined into {}", dir.display()), false);
                }
                Err(e) => toast(&shared, format!("join failed: {e}"), true),
            },
        }

        refresh(&shared, &started, &selected).await;
        set_busy(&shared, false);
        ctx.request_repaint();
    }
}

fn set_busy(shared: &Arc<Mutex<Shared>>, busy: bool) {
    if let Ok(mut s) = shared.lock() {
        s.busy = busy;
    }
}

/// Bound on the worker-side backlog. Reached only if the UI stops draining
/// (a frozen or minimised window); dropping the oldest keeps the newest, which
/// is what a user coming back to the window wants to see.
const TOAST_BACKLOG: usize = 32;

fn toast(shared: &Arc<Mutex<Shared>>, text: String, error: bool) {
    if let Ok(mut s) = shared.lock() {
        s.toasts.push(Toast { text, error });
        let excess = s.toasts.len().saturating_sub(TOAST_BACKLOG);
        s.toasts.drain(..excess);
    }
}

/// Forward an IPC request to a folder's daemon and toast the outcome.
async fn ipc_action(
    shared: &Arc<Mutex<Shared>>,
    dir: &Path,
    req: IpcRequest,
    ok_msg: &str,
    err_prefix: &str,
) {
    match ipc::request(dir, &req).await {
        Ok(r) if r.ok => toast(shared, ok_msg.to_string(), false),
        Ok(r) => {
            let msg = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| err_prefix.to_string());
            toast(shared, format!("{err_prefix}: {msg}"), true);
        }
        Err(e) => toast(shared, format!("{err_prefix}: {e}"), true),
    }
}

/// keep-mine: the guided lock → apply → unlock → discard sequence. The
/// quarantined copy is discarded ONLY after the apply AND its publish succeed, so
/// a failure at any step leaves the preserved copy untouched (Golden Invariant).
/// The daemon's `ConflictApply` refuses without a self-held lease, hence the
/// explicit lock/unlock around it — exactly the CLI's `resolve --keep mine` path.
async fn resolve_keep_mine(shared: &Arc<Mutex<Shared>>, dir: &Path, id: &str, target: &str) {
    // 1/4 lock
    match ipc::request(
        dir,
        &IpcRequest::Lock {
            path: target.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "lock refused".into());
            toast(
                shared,
                format!("keep-mine: lock refused ({m}). The copy is untouched."),
                true,
            );
            return;
        }
        Err(e) => {
            toast(
                shared,
                format!("keep-mine: lock failed ({e}). The copy is untouched."),
                true,
            );
            return;
        }
    }
    // 2/4 apply the preserved bytes
    match ipc::request(
        dir,
        &IpcRequest::ConflictApply {
            id: id.to_string(),
            target: target.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "apply failed".into());
            let held = !unlock_ok(dir, target).await;
            let note = if held {
                format!(" The lease on {target} may still be held — release it from Files.")
            } else {
                String::new()
            };
            toast(
                shared,
                format!("keep-mine: apply failed ({m}). The copy is untouched.{note}"),
                true,
            );
            return;
        }
        Err(e) => {
            let held = !unlock_ok(dir, target).await;
            let note = if held {
                format!(" The lease on {target} may still be held — release it from Files.")
            } else {
                String::new()
            };
            toast(
                shared,
                format!("keep-mine: apply failed ({e}). The copy is untouched.{note}"),
                true,
            );
            return;
        }
    }
    // 3/4 unlock (publish)
    match ipc::request(
        dir,
        &IpcRequest::Unlock {
            path: target.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "publish failed".into());
            toast(
                shared,
                format!(
                    "keep-mine: publish failed ({m}). Bytes applied but not published and the lease is still held — retry unlock from Files. The copy is untouched."
                ),
                true,
            );
            return;
        }
        Err(e) => {
            toast(
                shared,
                format!(
                    "keep-mine: publish failed ({e}). The lease is still held — retry unlock from Files. The copy is untouched."
                ),
                true,
            );
            return;
        }
    }
    // 4/4 discard the (now-superseded) quarantined copy
    match ipc::request(dir, &IpcRequest::ConflictDiscard { id: id.to_string() }).await {
        Ok(r) if r.ok => toast(shared, format!("resolved into {target}"), false),
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "discard failed".into());
            toast(
                shared,
                format!(
                    "keep-mine: published, but the copy discard failed ({m}) — it is still in quarantine."
                ),
                true,
            );
        }
        Err(e) => toast(
            shared,
            format!(
                "keep-mine: published, but the copy discard failed ({e}) — it is still in quarantine."
            ),
            true,
        ),
    }
}

/// Best-effort unlock used on a rollback path; returns whether the lease is now
/// released (so the caller can warn the user if it is still held).
async fn unlock_ok(dir: &Path, target: &str) -> bool {
    matches!(
        ipc::request(dir, &IpcRequest::Unlock { path: target.to_string() }).await,
        Ok(r) if r.ok
    )
}

/// Guided restore: lock → restore → unlock. The daemon refuses Restore without
/// a self-held lease; on success it pushes the replaced content to history
/// FIRST, so a restore never loses bytes. Failure branches release the lease
/// best-effort and say so honestly when they cannot.
async fn restore_guided(shared: &Arc<Mutex<Shared>>, dir: &Path, path: &str, n: usize) {
    match ipc::request(
        dir,
        &IpcRequest::Lock {
            path: path.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "lock refused".into());
            toast(
                shared,
                format!("restore: lock refused ({m}). Nothing changed."),
                true,
            );
            return;
        }
        Err(e) => {
            toast(
                shared,
                format!("restore: lock failed ({e}). Nothing changed."),
                true,
            );
            return;
        }
    }
    match ipc::request(
        dir,
        &IpcRequest::Restore {
            path: path.to_string(),
            n,
        },
    )
    .await
    {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "restore failed".into());
            let held = !unlock_ok(dir, path).await;
            let note = if held {
                format!(" The lease on {path} may still be held — release it from Files.")
            } else {
                String::new()
            };
            toast(
                shared,
                format!("restore failed ({m}). Nothing changed.{note}"),
                true,
            );
            return;
        }
        Err(e) => {
            let held = !unlock_ok(dir, path).await;
            let note = if held {
                format!(" The lease on {path} may still be held — release it from Files.")
            } else {
                String::new()
            };
            toast(
                shared,
                format!("restore failed ({e}). Nothing changed.{note}"),
                true,
            );
            return;
        }
    }
    match ipc::request(
        dir,
        &IpcRequest::Unlock {
            path: path.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => toast(
            shared,
            format!("restored version {n} of {path} (previous content kept in history)"),
            false,
        ),
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "publish failed".into());
            toast(
                shared,
                format!(
                    "restored, but publish failed ({m}) — the lease is still held; retry unlock from Files."
                ),
                true,
            );
        }
        Err(e) => toast(
            shared,
            format!(
                "restored, but publish failed ({e}) — the lease is still held; retry unlock from Files."
            ),
            true,
        ),
    }
}

async fn start_session(
    shared: &Arc<Mutex<Shared>>,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    dir: &Path,
) {
    let key = dir.to_string_lossy().to_string();
    // Hold the map lock across check→spawn→insert so two starts can't both spawn
    // and orphan a handle (DaemonHandle has no Drop).
    let mut hosted = started.lock().await;
    if hosted.contains_key(&key) || ipc::daemon_alive(dir).await {
        toast(shared, copy::TOAST_ALREADY_RUNNING.into(), true);
        return;
    }
    let saved = match AppState::load(dir) {
        Ok(st) => st.config,
        Err(e) => {
            toast(shared, format!("cannot start: {e}"), true);
            return;
        }
    };
    let net = match crate::cli::resolve_net_config(&saved, &crate::cli::NetFlags::default()) {
        Ok(n) => n,
        Err(e) => {
            toast(shared, format!("network config error: {e}"), true);
            return;
        }
    };
    let cfg = crate::daemon::DaemonConfig {
        dir: dir.to_path_buf(),
        net,
        timings: crate::locks::LockTimings {
            ttl: saved.lease_ttl(),
            renew: saved.lease_renew(),
            acquire_timeout: saved.acquire_timeout(),
        },
        ui: crate::ui::progress::Ui::disabled(),
    };
    match crate::daemon::spawn(cfg).await {
        Ok(handle) => {
            hosted.insert(key, handle);
            toast(shared, copy::TOAST_STARTED_HOSTED.into(), false);
        }
        Err(e) => toast(shared, format!("could not start: {e}"), true),
    }
}

async fn stop_session(
    shared: &Arc<Mutex<Shared>>,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    dir: &Path,
) {
    let key = dir.to_string_lossy().to_string();
    if let Some(handle) = started.lock().await.remove(&key) {
        // Bounded so a wedged actor can't freeze the single worker task forever.
        if tokio::time::timeout(GUI_SHUTDOWN, handle.shutdown())
            .await
            .is_err()
        {
            toast(shared, copy::TOAST_STOP_TIMEOUT.into(), true);
        } else {
            toast(shared, "stopped".into(), false);
        }
        return;
    }
    if !ipc::daemon_alive(dir).await {
        toast(shared, copy::TOAST_NOT_RUNNING.into(), true);
        return;
    }
    match ipc::request(dir, &IpcRequest::Shutdown).await {
        Ok(r) if r.ok => toast(shared, copy::TOAST_STOPPED.into(), false),
        Ok(r) => toast(
            shared,
            r.error
                .map(|e| e.message)
                .unwrap_or_else(|| "shutdown refused".into()),
            true,
        ),
        Err(e) => toast(shared, format!("stop failed: {e}"), true),
    }
}

async fn set_paused(shared: &Arc<Mutex<Shared>>, dir: &Path, pause: bool) {
    if AppState::load(dir).is_err() {
        toast(shared, copy::TOAST_NOT_SESSION_FOLDER.into(), true);
        return;
    }
    let abs = absolute(dir);
    {
        let mut reg = Registry::load();
        if !reg.sessions.iter().any(|s| s.path == abs.to_string_lossy()) {
            reg.register(dir, SessionKind::Init, crate::now_ms());
            let _ = reg.save();
        }
    }
    let path = abs.to_string_lossy().to_string();
    if crate::supervisor::control_alive().await {
        let req = if pause {
            crate::supervisor::ControlRequest::Pause { path }
        } else {
            crate::supervisor::ControlRequest::Resume { path }
        };
        match crate::supervisor::request(&req, Duration::from_secs(30)).await {
            Ok(r) if r.ok => toast(
                shared,
                if pause {
                    copy::TOAST_PAUSED_LIVE
                } else {
                    copy::TOAST_RESUMED_LIVE
                }
                .into(),
                false,
            ),
            Ok(r) => toast(
                shared,
                r.error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "supervisor refused".into()),
                true,
            ),
            Err(e) => toast(shared, format!("failed: {e}"), true),
        }
    } else {
        let mut reg = Registry::load();
        let _ = reg.set_paused(dir, pause);
        let _ = reg.save();
        toast(shared, copy::toast_paused_deferred(pause), false);
    }
}

/// Rebuild the overview + the selected session's detail and publish the snapshot.
async fn refresh(
    shared: &Arc<Mutex<Shared>>,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    selected: &Option<PathBuf>,
) {
    let hosted: std::collections::HashSet<String> = started.lock().await.keys().cloned().collect();
    let overview = fetch_overview(&hosted).await;
    // Reuse the previous ticket for the same running folder: every v2 mint
    // carries a fresh invite id, so re-minting per poll would make the visible
    // ticket (and its QR) churn every 1.5s. NB: this lock lives in a match
    // scrutinee, so the guard drops at this statement's semicolon — keep the
    // `.await` below in its own statement or the guard would span an await.
    let prior_invite = match (selected, shared.lock()) {
        (Some(dir), Ok(s)) => s
            .detail
            .as_ref()
            .filter(|d| d.running && d.dir == dir.to_string_lossy())
            .and_then(|d| d.invite.clone()),
        _ => None,
    };
    let detail = match selected {
        Some(dir) => Some(fetch_detail(dir, prior_invite).await),
        None => None,
    };
    if let Ok(mut s) = shared.lock() {
        s.overview = Some(overview);
        s.detail = detail;
        s.tick = s.tick.wrapping_add(1);
    }
}

async fn fetch_overview(hosted: &std::collections::HashSet<String>) -> Overview {
    // NB: do NOT prune+save the registry here. This runs every ~1.5s, so a
    // transiently unavailable volume (a network share or USB hiccup) would make
    // `AppState::exists` momentarily false and permanently unregister the session.
    // Unreadable sessions are shown with `readable: false` and simply reappear
    // when the volume returns; the CLI's explicit commands do the real pruning.
    let reg = Registry::load();
    let supervisor = crate::supervisor::control_alive().await;
    let mut sessions = Vec::with_capacity(reg.sessions.len());
    for s in &reg.sessions {
        let dir = PathBuf::from(&s.path);
        let st = AppState::load(&dir).ok();
        let readable = st.is_some();
        let (files, total_bytes, role, strict, id_short) = match &st {
            Some(st) => (
                st.files.values().filter(|f| !f.deleted).count(),
                st.files
                    .values()
                    .filter(|f| !f.deleted)
                    .map(|f| f.size)
                    .sum::<u64>(),
                st.config.role.as_str().to_string(),
                st.config.strict,
                st.node_id_short().unwrap_or_else(|| "?".into()),
            ),
            None => (0, 0, "?".into(), true, "-".into()),
        };
        let conflicts = crate::conflicts::list(&dir).len();
        let running = ipc::daemon_alive(&dir).await;
        let (peers_online, peers_total) = if running {
            match ipc::request(&dir, &IpcRequest::Status).await {
                Ok(r) if r.ok => {
                    let d = r.data.unwrap_or_default();
                    let members = d.get("members").and_then(|v| v.as_array());
                    let total = members.map(|m| m.len()).unwrap_or(0);
                    let online = members
                        .map(|m| {
                            m.iter()
                                .filter(|x| {
                                    x.get("online").and_then(|b| b.as_bool()).unwrap_or(false)
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    (online, total)
                }
                _ => (0, 0),
            }
        } else {
            (0, 0)
        };
        sessions.push(SessionRow {
            name: base_name(&s.path),
            path: s.path.clone(),
            running,
            paused: s.paused,
            hosted_by_gui: hosted.contains(&s.path),
            readable,
            role,
            strict,
            files,
            total_bytes,
            conflicts,
            peers_online,
            peers_total,
            id_short,
        });
    }
    Overview {
        version: env!("TAZAMUN_VERSION").to_string(),
        supervisor,
        sessions,
    }
}

async fn fetch_detail(dir: &Path, prior_invite: Option<String>) -> Detail {
    let conflicts: Vec<ConflictRow> = crate::conflicts::list(dir)
        .into_iter()
        .map(|c| ConflictRow {
            name: c.name,
            path: c.path.unwrap_or_default(),
            reason: c.reason.unwrap_or_else(|| "conflicting copy".into()),
            ts_ms: c.ts_ms,
            size: c.size,
        })
        .collect();
    let audit: Vec<AuditRow> = crate::audit::read(dir, &crate::audit::Filter::default())
        .into_iter()
        .rev()
        .take(200)
        .map(|e| AuditRow {
            ts_ms: e.ts_ms,
            kind: e.kind,
            path: e.path,
            peer: e.peer,
            detail: e.detail,
        })
        .rev()
        .collect();
    let invite = crate::home::offline_invite(dir);

    if ipc::daemon_alive(dir).await {
        // A running daemon can mint a live ticket (carries current addresses —
        // faster first connection than the offline fallback). Minted once per
        // selection: the caller passes the prior ticket back in on later polls.
        let live_invite = match prior_invite {
            Some(t) => Some(t),
            None => match ipc::request(
                dir,
                &IpcRequest::Invite {
                    role: None,
                    ttl_ms: None,
                },
            )
            .await
            {
                Ok(r) if r.ok => r
                    .data
                    .and_then(|d| d.get("ticket").and_then(|t| t.as_str().map(String::from))),
                _ => None,
            },
        };
        let invite = live_invite.or(invite);
        match ipc::request(dir, &IpcRequest::DashboardState).await {
            Ok(r) if r.ok => {
                let d = r.data.unwrap_or_default();
                return parse_running_detail(dir, &d, conflicts, audit, invite);
            }
            Ok(r) => {
                return Detail {
                    dir: dir.to_string_lossy().into(),
                    error: r.error.map(|e| e.message),
                    conflicts,
                    audit,
                    invite,
                    ..Default::default()
                };
            }
            Err(e) => {
                return Detail {
                    dir: dir.to_string_lossy().into(),
                    error: Some(e.to_string()),
                    conflicts,
                    audit,
                    invite,
                    ..Default::default()
                };
            }
        }
    }

    // Offline: read what we can directly.
    match AppState::load(dir) {
        Ok(st) => {
            let files = st
                .files
                .iter()
                .filter(|(_, r)| !r.deleted)
                .map(|(p, r)| FileRow {
                    path: p.as_str().to_string(),
                    size: r.size,
                    locked_by: None,
                    mine_lock: false,
                })
                .collect();
            Detail {
                dir: dir.to_string_lossy().into(),
                running: false,
                role: st.config.role.as_str().to_string(),
                strict: st.config.strict,
                invite,
                files,
                conflicts,
                audit,
                ..Default::default()
            }
        }
        Err(e) => Detail {
            dir: dir.to_string_lossy().into(),
            error: Some(e.to_string()),
            conflicts,
            audit,
            invite,
            ..Default::default()
        },
    }
}

fn parse_running_detail(
    dir: &Path,
    d: &serde_json::Value,
    conflicts: Vec<ConflictRow>,
    audit: Vec<AuditRow>,
    invite: Option<String>,
) -> Detail {
    let leases: Vec<LeaseRow> = d
        .get("leases")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|l| LeaseRow {
                    path: jstr(l, "path"),
                    holder: jstr(l, "holder"),
                    mine: l.get("mine").and_then(|b| b.as_bool()).unwrap_or(false),
                    expires_in_ms: l.get("expires_in_ms").and_then(|n| n.as_u64()).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    let members: Vec<Member> = d
        .get("members")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|m| Member {
                    id_short: short(&jstr(m, "id")),
                    name: m.get("name").and_then(|n| n.as_str()).map(str::to_string),
                    online: m.get("online").and_then(|b| b.as_bool()).unwrap_or(false),
                    grade: jstr(m, "grade"),
                    conn: jstr(m, "conn"),
                    rtt_ms: m.get("rtt_ms").and_then(|n| n.as_u64()),
                    via_lan: m.get("via_lan").and_then(|b| b.as_bool()).unwrap_or(false),
                    jitter_ms: m
                        .get("rtt_jitter_ms")
                        .and_then(|n| n.as_f64())
                        .unwrap_or(0.0),
                    rate_tx: m.get("rate_tx_bps").and_then(|n| n.as_f64()).unwrap_or(0.0) as u64,
                    rate_rx: m.get("rate_rx_bps").and_then(|n| n.as_f64()).unwrap_or(0.0) as u64,
                    bytes_tx: m.get("bytes_tx").and_then(|n| n.as_u64()).unwrap_or(0),
                    bytes_rx: m.get("bytes_rx").and_then(|n| n.as_u64()).unwrap_or(0),
                    relay_url: m
                        .get("relay_url")
                        .and_then(|r| r.as_str())
                        .map(str::to_string),
                    ttd_ms: m.get("time_to_direct_ms").and_then(|n| n.as_u64()),
                    flaps: m.get("flaps_per_min").and_then(|n| n.as_u64()).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    let files: Vec<FileRow> = d
        .get("files")
        .and_then(|v| v.as_object())
        .map(|obj| {
            let mut v: Vec<FileRow> = obj
                .iter()
                .map(|(path, meta)| {
                    let lease = leases.iter().find(|l| &l.path == path);
                    FileRow {
                        path: path.clone(),
                        size: meta.get("size").and_then(|n| n.as_u64()).unwrap_or(0),
                        locked_by: lease.map(|l| l.holder.clone()),
                        mine_lock: lease.map(|l| l.mine).unwrap_or(false),
                    }
                })
                .collect();
            v.sort_by(|a, b| a.path.cmp(&b.path));
            v
        })
        .unwrap_or_default();
    // The config summary rides in the payload (P10+): typed view for Settings.
    let config = d
        .get("config")
        .and_then(|c| c.as_object())
        .map(|c| ConfigView {
            autolock: c.get("autolock").and_then(|b| b.as_bool()).unwrap_or(false),
            strict: c.get("strict").and_then(|b| b.as_bool()).unwrap_or(true),
            role: c
                .get("role")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            update_channel: c
                .get("update_channel")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            lease_ttl_ms: c.get("lease_ttl_ms").and_then(|n| n.as_u64()).unwrap_or(0),
            acquire_timeout_ms: c
                .get("acquire_timeout_ms")
                .and_then(|n| n.as_u64())
                .unwrap_or(0),
            wait_timeout_ms: c
                .get("wait_timeout_ms")
                .and_then(|n| n.as_u64())
                .unwrap_or(0),
            dashboard_port: c
                .get("dashboard_port")
                .and_then(|n| n.as_u64())
                .unwrap_or(0) as u16,
            relay: c.get("relay").and_then(|s| s.as_str()).map(str::to_string),
            lan: c.get("lan").and_then(|b| b.as_bool()).unwrap_or(true),
            max_down: c.get("max_down").and_then(|n| n.as_u64()).unwrap_or(0),
        });
    let (role, strict) = config
        .as_ref()
        .map(|c| (c.role.clone(), c.strict))
        .unwrap_or_else(|| (String::new(), true));
    // Per-path version history (P14: tags + pins ride along).
    let versions: BTreeMap<String, Vec<VersionRow>> = d
        .get("versions")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(path, list)| {
                    let rows = list
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|e| VersionRow {
                                    n: e.get("n").and_then(|n| n.as_u64()).unwrap_or(0),
                                    ts_ms: e.get("ts_ms").and_then(|n| n.as_u64()).unwrap_or(0),
                                    size: e.get("size").and_then(|n| n.as_u64()).unwrap_or(0),
                                    tag: e.get("tag").and_then(|t| t.as_str()).map(str::to_string),
                                    pinned: e
                                        .get("pinned")
                                        .and_then(|b| b.as_bool())
                                        .unwrap_or(false),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (path.clone(), rows)
                })
                .collect()
        })
        .unwrap_or_default();
    let pulls: Vec<PullRow> = d
        .get("pending_pulls")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|p| PullRow {
                    path: jstr(p, "path"),
                    percent: p.get("percent").and_then(|n| n.as_u64()).unwrap_or(0),
                    bytes_done: p.get("bytes_done").and_then(|n| n.as_u64()).unwrap_or(0),
                    bytes_total: p.get("bytes_total").and_then(|n| n.as_u64()).unwrap_or(0),
                    rate: p
                        .get("rate_bytes_per_sec")
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    let events: Vec<EventRow> = d
        .get("events")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|e| EventRow {
                    text: jstr(e, "text"),
                })
                .collect()
        })
        .unwrap_or_default();
    let transfer = d.get("transfer");
    Detail {
        dir: dir.to_string_lossy().into(),
        running: true,
        role,
        strict,
        invite,
        members,
        files,
        files_total: d.get("files_total").and_then(|n| n.as_u64()).unwrap_or(0) as usize,
        files_truncated: d
            .get("files_truncated")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        conflicts,
        leases,
        audit,
        config,
        versions,
        pulls,
        backlog: transfer
            .and_then(|t| t.get("backlog"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize,
        resuming: transfer
            .and_then(|t| t.get("resuming"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize,
        download_limit_bps: transfer
            .and_then(|t| t.get("download_limit_bps"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0),
        events,
        error: None,
    }
}

// ─── the eframe app ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Peers,
    Files,
    Conflicts,
    History,
    Audit,
    Settings,
}

impl Tab {
    /// Display order, shared by the tab bar, the running head, and the folio.
    const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Peers,
        Tab::Files,
        Tab::Conflicts,
        Tab::History,
        Tab::Audit,
        Tab::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Peers => "Peers",
            Tab::Files => "Files",
            Tab::Conflicts => "Conflicts",
            Tab::History => "History",
            Tab::Audit => "Audit",
            Tab::Settings => "Settings",
        }
    }

    /// The persisted form of a tab. Derived from `label` so the two cannot
    /// drift; an unknown key from another build degrades to Overview.
    fn from_key(key: &str) -> Tab {
        Tab::ALL
            .into_iter()
            .find(|t| t.label().eq_ignore_ascii_case(key))
            .unwrap_or(Tab::Overview)
    }

    /// One-based position, so the folio can read "3/7".
    fn folio(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) + 1
    }
}

/// A pending confirmation modal: the command fires only on explicit confirm.
struct Confirm {
    title: String,
    body: String,
    verb: String,
    danger: bool,
    action: Option<Cmd>,
}

struct App {
    tx: mpsc::UnboundedSender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    selected: Option<String>,
    tab: Tab,
    file_filter: String,
    init_path: String,
    join_path: String,
    join_ticket: String,
    peer_id: String,
    peer_name: String,
    cfg_edits: BTreeMap<String, (String, String)>,
    open_versions: std::collections::HashSet<String>,
    tag_edit: Option<(String, usize, String)>,
    confirm: Option<Confirm>,
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    qr: Option<(String, egui::TextureHandle)>,
    colophon_open: bool,
    files_sort: grouping::SortMode,
    telemetry: telemetry::TelemetryStore,
    seen_tick: u64,
    toasts: toasts::Queue,
    shortcuts_open: bool,
    text_scale: f32,
    /// Set to the scale that still needs pushing into the context; taken on the
    /// next frame so `theme::install` (which also writes `text_styles`) can
    /// never race ahead of it.
    scale_dirty: Option<f32>,
    /// One-shot: focus the file filter on the next frame that draws it.
    focus_filter: bool,
    /// Id of the selected tab, republished every frame so the skip link has a
    /// live target to hand keyboard focus to.
    skip_target: Option<egui::Id>,
    multi: selection::Selection,
    /// Last state written to disk. Comparing against it each frame catches
    /// every change — including a window drag — without a mark-dirty call at
    /// each of the dozen sites that can alter a preference.
    prefs_last: prefs::Prefs,
    prefs_saved_at: f64,
    /// Session the peer sky is currently revealing, and when that began.
    /// `animate_bool_with_time` cannot express this: it returns the target
    /// immediately for an id it has not seen, so a constant `true` yields 1.0
    /// on the first frame and the draw-on never plays.
    sky_key: Option<String>,
    sky_start: f64,
    shot_sent: bool,
}

impl App {
    fn new(
        tx: mpsc::UnboundedSender<Cmd>,
        shared: Arc<Mutex<Shared>>,
        saved: prefs::Prefs,
    ) -> Self {
        // The screenshot hook wins over a restored session so a capture is
        // reproducible whatever the last run left behind.
        let preselect = std::env::var("TAZAMUN_GUI_SHOT_SELECT")
            .ok()
            .or_else(|| saved.last_session.clone());
        if let Some(p) = &preselect {
            let _ = tx.send(Cmd::Select(Some(PathBuf::from(p))));
        }
        // Both the screenshot hook and the persisted preference name tabs the
        // same way, so both go through `from_key`. Spelling the arms out here
        // meant "overview" had no arm and fell through to the saved tab — so
        // asking for the overview silently produced whichever tab was open
        // last, which is exactly the sort of thing a screenshot hook must not do.
        let pretab = match std::env::var("TAZAMUN_GUI_SHOT_TAB") {
            Ok(v) if !v.trim().is_empty() => Tab::from_key(v.trim()),
            _ => Tab::from_key(&saved.last_tab),
        };
        Self {
            tx,
            shared,
            selected: preselect,
            tab: pretab,
            file_filter: String::new(),
            init_path: String::new(),
            join_path: String::new(),
            join_ticket: String::new(),
            peer_id: String::new(),
            peer_name: String::new(),
            cfg_edits: BTreeMap::new(),
            open_versions: std::collections::HashSet::new(),
            tag_edit: None,
            confirm: None,
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            qr: None,
            colophon_open: false,
            files_sort: if saved.sort_by_size {
                grouping::SortMode::Size
            } else {
                grouping::SortMode::Name
            },
            telemetry: telemetry::TelemetryStore::default(),
            seen_tick: 0,
            toasts: toasts::Queue::default(),
            shortcuts_open: false,
            text_scale: saved.text_scale,
            // Queued rather than applied: `theme::install` also writes
            // `text_styles`, so the scale has to land after it.
            scale_dirty: Some(saved.text_scale),
            focus_filter: false,
            skip_target: None,
            multi: selection::Selection::default(),
            prefs_last: saved,
            prefs_saved_at: 0.0,
            sky_key: None,
            sky_start: 0.0,
            shot_sent: false,
        }
    }

    /// The preferences as they stand this frame.
    fn prefs_now(&self, ctx: &egui::Context) -> prefs::Prefs {
        let (rect, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect, v.maximized.unwrap_or(false))
        });
        prefs::Prefs {
            text_scale: self.text_scale,
            sort_by_size: self.files_sort == grouping::SortMode::Size,
            last_tab: self.tab.label().to_ascii_lowercase(),
            last_session: self.selected.clone(),
            // A maximized window reports its expanded size; storing that would
            // restore a maximized-looking window that is not maximized.
            window: if maximized {
                self.prefs_last.window
            } else {
                rect.map(|r| [r.width(), r.height()])
            },
            maximized,
        }
    }

    /// Persists at most once every [`PREFS_DEBOUNCE`] seconds, and only when
    /// something actually differs — a window drag must not become one file
    /// write per frame.
    fn flush_prefs(&mut self, ctx: &egui::Context, now: f64, force: bool) {
        let next = self.prefs_now(ctx);
        if next == self.prefs_last && !force {
            return;
        }
        if !force && now - self.prefs_saved_at < PREFS_DEBOUNCE {
            return;
        }
        prefs::save(&next);
        self.prefs_last = next;
        self.prefs_saved_at = now;
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    fn select(&mut self, path: Option<String>) {
        self.selected = path.clone();
        self.tab = Tab::Overview;
        self.confirm = None;
        self.open_versions.clear();
        self.tag_edit = None;
        self.cfg_edits.clear();
        self.send(Cmd::Select(path.map(PathBuf::from)));
    }

    fn ask(&mut self, title: &str, body: String, verb: &str, danger: bool, action: Cmd) {
        self.confirm = Some(Confirm {
            title: title.to_string(),
            body,
            verb: verb.to_string(),
            danger,
            action: Some(action),
        });
    }
}

impl eframe::App for App {
    // The window is transparent; `chrome::paint_root` draws the rounded body,
    // so everything outside its corners stays see-through.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Keep timers/ages fresh even without a worker push.
        ui.ctx().request_repaint_after(REFRESH);
        let now = ui.input(|i| i.time);
        // Applied here, never at construction: `theme::install` also writes
        // `text_styles`, so the scale has to land after it, once per change.
        if let Some(scale) = self.scale_dirty.take() {
            a11y::apply_text_scale(ui.ctx(), scale);
        }
        self.flush_prefs(ui.ctx(), now, false);

        let snapshot = {
            // Poison-tolerant: a panicked worker holding this lock must not turn
            // every subsequent UI frame into a second panic.
            let mut s = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for t in std::mem::take(&mut s.toasts) {
                let kind = if t.error {
                    toasts::Kind::Bad
                } else {
                    toasts::Kind::Good
                };
                self.toasts.push(t.text, kind, now);
            }
            if let Some((target, path)) = s.picked.take() {
                match target {
                    PickTarget::Init => self.init_path = path,
                    PickTarget::Join => self.join_path = path,
                }
            }
            (s.overview.clone(), s.detail.clone(), s.tick, s.busy)
        };
        let (overview, detail, tick, busy) = snapshot;
        if tick != self.seen_tick {
            self.seen_tick = tick;
            if let Some(d) = &detail
                && d.running
            {
                for m in &d.members {
                    self.telemetry.push(&m.id_short, m.rtt_ms);
                }
                let live: Vec<String> = d.members.iter().map(|m| m.id_short.clone()).collect();
                self.telemetry.prune(&live);
            }
        }

        let maximized = chrome::is_maximized(ui);
        chrome::paint_root(ui, maximized);

        Panel::top("titlebar")
            .frame(egui::Frame::NONE)
            .default_size(chrome::TITLEBAR_H)
            .show(ui, |ui| self.title_bar(ui, &overview, busy, maximized));
        Panel::bottom("status")
            .frame(egui::Frame::NONE)
            .default_size(statusbar::STRIP_H)
            .show(ui, |ui| {
                let ov = overview.as_ref();
                let note = self.selected.as_deref().map(base_name);
                statusbar::status_strip(
                    ui,
                    statusbar::Status {
                        sessions: ov.map(|o| o.sessions.len()).unwrap_or(0),
                        running: ov
                            .map(|o| o.sessions.iter().filter(|s| s.running).count())
                            .unwrap_or(0),
                        peers_online: ov
                            .map(|o| o.sessions.iter().map(|s| s.peers_online).sum())
                            .unwrap_or(0),
                        conflicts: ov
                            .map(|o| o.sessions.iter().map(|s| s.conflicts).sum())
                            .unwrap_or(0),
                        busy,
                        note: note.as_deref(),
                    },
                    maximized,
                );
            });
        Panel::left("sessions")
            .frame(egui::Frame::NONE)
            .resizable(true)
            .default_size(294.0)
            .show(ui, |ui| {
                // The status strip owns the window's bottom edge now, so the
                // sidebar must not round a corner in the middle of the frame.
                chrome::paint_sidebar_bg(ui, true);
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 10,
                        top: 10,
                        bottom: 8,
                    })
                    .show(ui, |ui| self.sidebar(ui, &overview));
            });
        CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 18,
                right: 18,
                top: 12,
                bottom: 12,
            }))
            .show(ui, |ui| match &self.selected {
                None => self.home(ui, &overview),
                Some(_) => self.session_view(ui, detail.as_ref()),
            });

        // Drag-a-folder-onto-the-window: overlay while hovering, route on drop.
        dropzone::overlay_if_hovering(ui);
        let registered: Vec<String> = overview
            .as_ref()
            .map(|ov| ov.sessions.iter().map(|s| s.path.clone()).collect())
            .unwrap_or_default();
        if let Some(act) = dropzone::take_drop(ui, &registered) {
            match act {
                dropzone::DropAction::OpenSession(p) => self.select(Some(p)),
                dropzone::DropAction::PrefillInit(p) => {
                    self.select(None);
                    self.init_path = p;
                    self.toasts
                        .push(copy::DROP_TOAST_PREFILLED.into(), toasts::Kind::Good, now);
                }
                dropzone::DropAction::Rejected(why) => {
                    self.toasts.push(why.to_string(), toasts::Kind::Warn, now);
                }
            }
        }

        self.keyboard(ui, &overview);
        self.debug_screenshot(ui);
        self.palette_overlay(ui, &overview);
        self.shortcuts_overlay(ui);
        self.confirm_overlay(ui);
        self.colophon_overlay(ui);
        self.toast_overlay(ui);
        chrome::resize_zones(ui);
    }
}

// ─── views ───────────────────────────────────────────────────────────────────

impl App {
    fn title_bar(
        &mut self,
        ui: &mut egui::Ui,
        overview: &Option<Overview>,
        busy: bool,
        maximized: bool,
    ) {
        chrome::paint_titlebar_bg(ui, maximized);
        let bar = ui.max_rect();
        chrome::titlebar_interactions(ui, bar);
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 16,
                right: 8,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.set_min_height(bar.height());
                ui.style_mut().interaction.selectable_labels = false;
                ui.horizontal_centered(|ui| {
                    chrome::wordmark(ui, 19.0);
                    if let Some(ov) = overview {
                        ui.add_space(8.0);
                        let running = ov.sessions.iter().filter(|s| s.running).count();
                        let conflicts: usize = ov.sessions.iter().map(|s| s.conflicts).sum();
                        let n = ov.sessions.len();
                        theme::pill(
                            ui,
                            &format!("{n} session{}", if n == 1 { "" } else { "s" }),
                            DIM,
                        );
                        if running > 0 {
                            theme::pill(ui, &format!("{running} running"), GOOD);
                        }
                        if conflicts > 0 {
                            theme::pill(ui, &figures::count(conflicts, "conflict"), WARN);
                        }
                        if ov.supervisor {
                            theme::pill(ui, "supervisor", theme::LAPIS);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        // Painter-drawn glyphs describe nothing on their own, so
                        // each one is named for assistive technology here.
                        let close = chrome::window_button(ui, chrome::WinButton::Close, maximized);
                        a11y::label_button(&close, copy::WIN_CLOSE);
                        if close.clicked() {
                            let now = ui.input(|i| i.time);
                            self.flush_prefs(ui.ctx(), now, true);
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let max_restore = chrome::window_button(
                            ui,
                            chrome::WinButton::MaximizeRestore,
                            maximized,
                        );
                        a11y::label_button(
                            &max_restore,
                            if maximized {
                                copy::WIN_RESTORE
                            } else {
                                copy::WIN_MAXIMIZE
                            },
                        );
                        if max_restore.clicked() {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        let minimize =
                            chrome::window_button(ui, chrome::WinButton::Minimize, maximized);
                        a11y::label_button(&minimize, copy::WIN_MINIMIZE);
                        if minimize.clicked() {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(8.0);
                        let r = ui
                            .add(egui::Button::new(
                                egui::RichText::new("⌘K").size(11.0).color(DIM),
                            ))
                            .on_hover_text("command palette (Ctrl+K)");
                        if r.clicked() {
                            self.palette_open = true;
                            self.palette_query.clear();
                            self.palette_sel = 0;
                        }
                        if busy {
                            ceremony::loading_mark(ui, 16.0);
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, overview: &Option<Overview>) {
        // Invisible until tabbed to, then the first stop past the window
        // chrome: it hands focus straight to the open tab.
        if focusnav::skip_link(ui, copy::SKIP_TO_CONTENT)
            && let Some(id) = self.skip_target
        {
            ui.ctx().memory_mut(|m| m.request_focus(id));
        }
        let home_selected = self.selected.is_none();
        let (home_hit, _) = self.nav_card(ui, copy::SIDEBAR_HOME_TITLE, home_selected, |ui| {
            ui.label(
                egui::RichText::new(copy::SIDEBAR_HOME_TITLE)
                    .family(theme::fam_semibold())
                    .size(13.5)
                    .color(if home_selected { ACCENT } else { INK }),
            );
            ui.label(
                egui::RichText::new(copy::SIDEBAR_HOME_SUB)
                    .size(11.0)
                    .color(DIM),
            );
        });
        if home_hit {
            self.multi.clear();
            self.select(None);
        }
        ui.add_space(4.0);
        theme::section(ui, copy::SESSIONS_SECTION);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(ov) = overview else {
                    for w in [180.0, 140.0, 160.0] {
                        theme::skeleton(ui, w);
                    }
                    return;
                };
                let all_keys: Vec<String> = ov.sessions.iter().map(|r| r.path.clone()).collect();
                // Unconditional: if every session is unregistered while some
                // were marked, the empty-state branch renders and the bulk bar
                // below would otherwise still claim a count.
                self.multi.retain_existing(&all_keys);
                if ov.sessions.is_empty() {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(copy::NO_SESSIONS_TITLE).color(DIM));
                    ui.label(
                        egui::RichText::new(copy::NO_SESSIONS_HINT)
                            .color(theme::FAINT)
                            .size(11.5),
                    );
                } else {
                    let rows: Vec<SessionRow> = ov.sessions.clone();
                    let keys = all_keys.clone();
                    let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));

                    let mut rects: Vec<egui::Rect> = Vec::with_capacity(rows.len());
                    for (i, s) in rows.iter().enumerate() {
                        let open = self.selected.as_deref() == Some(s.path.as_str());
                        let (hit, rect) = self.session_card(ui, s, open);
                        rects.push(rect);
                        if hit {
                            if ctrl || shift {
                                self.multi.click(&keys, i, ctrl, shift);
                            } else {
                                // An unmodified click is the old gesture: open
                                // this one and abandon any marked set.
                                self.multi.clear();
                                self.select(Some(s.path.clone()));
                            }
                        }
                        ui.add_space(6.0);
                    }

                    // One brace per contiguous run, set in the cards' own left
                    // padding — drawing left of the content rect would fall
                    // outside the ScrollArea's clip and never appear.
                    let t = ui.ctx().animate_bool_with_time(
                        egui::Id::new("multi-brace"),
                        !self.multi.is_empty(),
                        0.18,
                    );
                    if t > 0.0 {
                        let runs: Vec<marginalia::Run> = self
                            .multi
                            .runs(&keys)
                            .into_iter()
                            .filter_map(|(a, b)| {
                                Some(marginalia::Run {
                                    top: rhythm::snap(rects.get(a)?.top()),
                                    bottom: rhythm::snap(rects.get(b)?.bottom()),
                                })
                            })
                            .collect();
                        let x = ui.min_rect().left() + 4.0;
                        marginalia::brace(ui.painter(), x, &runs, t);
                    }
                }
                // The bulk bar sits between the list and the footer, and only
                // while a set is marked.
                if !self.multi.is_empty() {
                    let marked = self.multi.ordered(
                        &ov.sessions
                            .iter()
                            .map(|r| r.path.clone())
                            .collect::<Vec<String>>(),
                    );
                    let any_running = ov
                        .sessions
                        .iter()
                        .any(|r| r.running && self.multi.is_selected(&r.path));
                    let any_stopped = ov
                        .sessions
                        .iter()
                        .any(|r| !r.running && self.multi.is_selected(&r.path));
                    ui.add_space(6.0);
                    let out = marginalia::bulk_bar(ui, self.multi.len(), any_running, any_stopped);
                    if out.start {
                        for path in &marked {
                            self.send(Cmd::Start(PathBuf::from(path)));
                        }
                    }
                    if out.stop {
                        for path in &marked {
                            self.send(Cmd::Stop(PathBuf::from(path)));
                        }
                    }
                    if out.clear {
                        self.multi.clear();
                    }
                }
                // Footer pinned under the list: version + supervisor state.
                ui.add_space(8.0);
                ornament::rule_with_diamond(ui, ACCENT);
                ui.horizontal(|ui| {
                    let (mark, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    ornament::khatam(
                        ui.painter(),
                        mark.center(),
                        5.5,
                        theme::GOLD.linear_multiply(0.8),
                        true,
                    );
                    ui.label(
                        egui::RichText::new(format!("v{}", ov.version))
                            .size(10.5)
                            .color(theme::FAINT),
                    );
                    if ov.supervisor {
                        ui.label(
                            egui::RichText::new("· supervisor on")
                                .size(10.5)
                                .color(GOOD),
                        );
                    }
                });
            });
    }

    /// A clickable rounded nav card; returns true on click or on keyboard
    /// activation. Contents are laid out inside; hover/selection animate the
    /// fill. `label` is what a screen reader announces, since everything drawn
    /// inside is painter output the row itself cannot describe.
    fn nav_card(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        selected: bool,
        add_contents: impl FnOnce(&mut egui::Ui),
    ) -> (bool, egui::Rect) {
        let width = ui.available_width();
        let resp = ui
            .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                ui.set_width(width);
                let hovered = ui.response().hovered();
                let t =
                    ui.ctx()
                        .animate_bool_with_time(ui.response().id, hovered || selected, 0.12);
                let fill = theme::lerp_color(egui::Color32::TRANSPARENT, theme::BG3, t * 0.9);
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(theme::R_CARD)
                    .stroke(if selected {
                        egui::Stroke::new(1.0, ACCENT.linear_multiply(0.35))
                    } else {
                        egui::Stroke::NONE
                    })
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.vertical(add_contents);
                    });
            })
            .response;
        if selected {
            let r = resp.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    r.left_top() + egui::vec2(0.0, 8.0),
                    egui::vec2(3.0, r.height() - 16.0),
                ),
                2.0,
                ACCENT,
            );
        }
        a11y::label_selectable(&resp, label, selected);
        // Ring first, then activation: an unfocused row consumes no keys.
        let by_key = focusnav::activate(ui, &resp);
        (resp.clicked() || by_key, resp.rect)
    }

    /// One session row in the sidebar.
    fn session_card(
        &mut self,
        ui: &mut egui::Ui,
        s: &SessionRow,
        selected: bool,
    ) -> (bool, egui::Rect) {
        let (_, dot_color) = status_dot(s);
        // Spoken form of everything the row paints: name, state, conflict count.
        let mut label = format!("{}, {}", s.name, status_text(s));
        match s.conflicts {
            0 => {}
            1 => label.push_str(", 1 conflict"),
            n => label.push_str(&format!(", {n} conflicts")),
        }
        self.nav_card(ui, &label, selected, |ui| {
            ui.horizontal(|ui| {
                theme::glow_dot(ui, dot_color);
                ui.label(
                    egui::RichText::new(&s.name)
                        .family(theme::fam_medium())
                        .size(13.5)
                        .color(if selected { ACCENT } else { INK }),
                );
                if s.conflicts > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme::pill(ui, &format!("{}", s.conflicts), WARN);
                    });
                }
            });
            ui.label(egui::RichText::new(status_text(s)).size(11.0).color(DIM));
            let mut meta = format!("{} · {}", s.id_short, human_bytes(s.total_bytes));
            if s.hosted_by_gui {
                meta.push_str(" · hosted here");
            }
            ui.label(egui::RichText::new(meta).size(10.0).color(theme::FAINT));
        })
    }

    fn home(&mut self, ui: &mut egui::Ui, overview: &Option<Overview>) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let hero = ui.available_rect_before_wrap();
                ornament::corner_flourish(
                    ui.painter(),
                    hero.right_top(),
                    egui::vec2(-1.0, 1.0),
                    110.0,
                    theme::GOLD.linear_multiply(0.5),
                );
                ui.add_space(4.0);
                match overview {
                    Some(ov) if ov.sessions.is_empty() => self.first_light(ui),
                    Some(ov) => {
                        ui.label(
                            egui::RichText::new(copy::HOME_TITLE)
                                .family(theme::fam_semibold())
                                .size(18.0)
                                .color(INK),
                        );
                        ui.label(egui::RichText::new(copy::HOME_SUB).size(12.0).color(DIM));
                        ui.add_space(8.0);
                        let running = ov.sessions.iter().filter(|s| s.running).count();
                        let conflicts: usize = ov.sessions.iter().map(|s| s.conflicts).sum();
                        let bytes: u64 = ov.sessions.iter().map(|s| s.total_bytes).sum();
                        components::ledger_stats(
                            ui,
                            &[
                                (format!("{}", ov.sessions.len()), "sessions"),
                                (format!("{running}"), "running"),
                                (format!("{conflicts}"), "conflicts"),
                                (human_bytes(bytes), "synced data"),
                            ],
                        );
                        if conflicts > 0 {
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(copy::HOME_CONFLICTS_NOTE)
                                    .size(11.5)
                                    .color(WARN),
                            );
                        }
                        ui.add_space(12.0);
                        theme::section(ui, copy::CREATE_TITLE);
                        components::notched_card(ui, Some(ACCENT), |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(egui::RichText::new(copy::CREATE_HINT).size(11.5).color(DIM));
                            self.create_form_row(ui);
                        });
                        ui.add_space(6.0);
                        theme::section(ui, copy::JOIN_TITLE);
                        components::notched_card(ui, None, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(egui::RichText::new(copy::JOIN_HINT).size(11.5).color(DIM));
                            self.join_form_row(ui);
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            for w in [96.0, 96.0, 96.0] {
                                theme::skeleton(ui, w);
                            }
                        });
                    }
                }
                // Outside the match on purpose: text size is a property of the
                // window, and the reader who most needs it is the first-timer
                // on the zero-session page.
                ui.add_space(10.0);
                self.display_section(ui);
                rhythm::space(ui, 3);
                rhythm::foot_rule(ui);
            });
    }

    /// App-wide display preferences. Text size lives here rather than in a
    /// session's Settings tab: it is a property of the window, and Settings
    /// needs a running daemon it has no business requiring for this.
    fn display_section(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, copy::DISPLAY_TITLE);
        components::notched_card(ui, None, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(copy::A11Y_TEXT_SIZE)
                        .size(12.5)
                        .family(theme::fam_medium())
                        .color(INK),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Ends disable rather than vanish, so the row never reflows.
                    let at_max = self.text_scale >= a11y::SCALE_MAX;
                    let at_min = self.text_scale <= a11y::SCALE_MIN;
                    let bigger = ui.add_enabled_ui(!at_max, |ui| controls::ghost_small(ui, "+"));
                    a11y::label_button(&bigger.inner, copy::A11Y_BIGGER);
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(a11y::scale_label(self.text_scale))
                            .size(11.5)
                            .family(theme::fam_medium())
                            .color(DIM),
                    );
                    ui.add_space(2.0);
                    let smaller = ui.add_enabled_ui(!at_min, |ui| controls::ghost_small(ui, "-"));
                    a11y::label_button(&smaller.inner, copy::A11Y_SMALLER);
                    let step = if bigger.inner.clicked() {
                        Some(a11y::step_scale(self.text_scale, true))
                    } else if smaller.inner.clicked() {
                        Some(a11y::step_scale(self.text_scale, false))
                    } else {
                        None
                    };
                    if let Some(next) = step
                        && next != self.text_scale
                    {
                        self.text_scale = next;
                        self.scale_dirty = Some(next);
                    }
                });
            });
            ui.label(
                egui::RichText::new(copy::A11Y_TEXT_SIZE_HINT)
                    .size(11.0)
                    .color(theme::FAINT),
            );
        });
    }

    /// The zero-session opening page: three medallioned steps joined by a
    /// strapwork thread, the real create/join forms living inside step one.
    fn first_light(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(copy::FL_TITLE)
                .family(theme::fam_semibold())
                .size(20.0)
                .color(INK),
        );
        ui.label(egui::RichText::new(copy::FL_SUB).size(12.0).color(DIM));
        ui.add_space(10.0);
        onboarding::first_light_frame(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    onboarding::medallion(ui, 1, onboarding::StepState::Active);
                    onboarding::connector(ui, 118.0);
                    onboarding::medallion(ui, 2, onboarding::StepState::Future);
                    onboarding::connector(ui, 34.0);
                    onboarding::medallion(ui, 3, onboarding::StepState::Future);
                });
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(copy::FL_STEP1_TITLE)
                            .family(theme::fam_semibold())
                            .size(14.5)
                            .color(INK),
                    );
                    ui.label(
                        egui::RichText::new(copy::FL_STEP1_HINT)
                            .size(11.5)
                            .color(DIM),
                    );
                    ui.add_space(4.0);
                    self.create_form_row(ui);
                    self.join_form_row(ui);
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(copy::FL_STEP2_TITLE)
                            .family(theme::fam_semibold())
                            .size(13.0)
                            .color(theme::FAINT),
                    );
                    ui.label(
                        egui::RichText::new(copy::FL_STEP2_HINT)
                            .size(11.0)
                            .color(theme::FAINT),
                    );
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new(copy::FL_STEP3_TITLE)
                            .family(theme::fam_semibold())
                            .size(13.0)
                            .color(theme::FAINT),
                    );
                    ui.label(
                        egui::RichText::new(copy::FL_STEP3_HINT)
                            .size(11.0)
                            .color(theme::FAINT),
                    );
                });
            });
        });
    }

    /// The create-session input row (shared by Home and first light).
    fn create_form_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            fields::text_field(
                ui,
                &mut self.init_path,
                copy::HINT_FOLDER,
                300.0,
                fields::FieldState::Neutral,
            );
            if controls::ghost_button(ui, "Browse…")
                .on_hover_text(copy::BROWSE_HOVER)
                .clicked()
            {
                self.send(Cmd::PickFolder(PickTarget::Init));
            }
            if components::bevel_primary(ui, "Create").clicked()
                && !self.init_path.trim().is_empty()
            {
                self.send(Cmd::Init(PathBuf::from(self.init_path.trim())));
            }
        });
    }

    /// The join-with-ticket input row (shared by Home and first light).
    fn join_form_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            fields::text_field(
                ui,
                &mut self.join_path,
                copy::HINT_EMPTY_FOLDER,
                240.0,
                fields::FieldState::Neutral,
            );
            if controls::ghost_button(ui, "Browse…")
                .on_hover_text(copy::BROWSE_HOVER)
                .clicked()
            {
                self.send(Cmd::PickFolder(PickTarget::Join));
            }
            // A ticket is self-describing, so the field can say whether what
            // was pasted even looks like one before Join is pressed.
            let t = self.join_ticket.trim();
            let ticket_state = if t.is_empty() {
                fields::FieldState::Neutral
            } else if t.starts_with("tzm1") {
                fields::FieldState::Valid
            } else {
                fields::FieldState::Invalid
            };
            fields::text_field_mono_state(
                ui,
                &mut self.join_ticket,
                copy::HINT_TICKET,
                200.0,
                ticket_state,
            );
            if components::bevel_primary(ui, "Join").clicked()
                && !self.join_path.trim().is_empty()
                && !self.join_ticket.trim().is_empty()
            {
                self.send(Cmd::Join(
                    PathBuf::from(self.join_path.trim()),
                    self.join_ticket.trim().to_string(),
                ));
            }
        });
    }

    fn session_view(&mut self, ui: &mut egui::Ui, detail: Option<&Detail>) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        let dir = PathBuf::from(&sel);
        // Use the detail only once it belongs to the selected folder — otherwise
        // the lifecycle buttons and the Conflicts badge would show the previously
        // selected session's state for the ~1.5s before the worker refreshes.
        let detail = detail.filter(|d| d.dir == sel);
        let running = detail.map(|d| d.running).unwrap_or(false);

        // Page furniture before the page: where you are, and which leaf.
        let name = base_name(&sel);
        let folio = format!("{}/{}", self.tab.folio(), Tab::ALL.len());
        rhythm::running_head(ui, &[name.as_str(), self.tab.label()], Some(&folio));

        // Header: name, path, lifecycle.
        ui.horizontal(|ui| {
            theme::glow_dot(ui, if running { GOOD } else { DIM });
            ui.label(
                egui::RichText::new(base_name(&sel))
                    .family(theme::fam_semibold())
                    .size(18.0)
                    .color(INK),
            );
            ui.label(egui::RichText::new(&sel).color(theme::FAINT).size(11.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if controls::ghost_button(ui, "Stop").clicked() {
                        self.send(Cmd::Stop(dir.clone()));
                    }
                } else if components::bevel_primary(ui, "Start").clicked() {
                    self.send(Cmd::Start(dir.clone()));
                }
                if controls::ghost_small(ui, "Pause").clicked() {
                    self.send(Cmd::Pause(dir.clone()));
                }
                if controls::ghost_small(ui, "Resume").clicked() {
                    self.send(Cmd::Resume(dir.clone()));
                }
                ui.add_space(6.0);
                if controls::ghost_small(ui, "Open folder")
                    .on_hover_text(copy::OPEN_FOLDER_HOVER)
                    .clicked()
                    && let Err(e) = sysopen::open_folder(Path::new(&sel))
                {
                    self.toasts.push(
                        format!("could not open the file manager: {e}"),
                        toasts::Kind::Bad,
                        ui.input(|i| i.time),
                    );
                }
                if controls::ghost_small(ui, "Copy path")
                    .on_hover_text(copy::COPY_PATH_HOVER)
                    .clicked()
                {
                    ui.ctx().copy_text(sel.clone());
                    self.toasts.push(
                        copy::TOAST_PATH_COPIED.into(),
                        toasts::Kind::Info,
                        ui.input(|i| i.time),
                    );
                }
            });
        });
        rhythm::space(ui, 2);

        // Tab bar with the animated gold underline.
        let ctotal = detail.map(|d| d.conflicts.len()).unwrap_or(0);
        let mut active_rect: Option<egui::Rect> = None;
        let bar_bottom = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                for tab in Tab::ALL {
                    let label = tab.label();
                    let selected = self.tab == tab;
                    let r = ui.add(
                        egui::Label::new(
                            egui::RichText::new(label)
                                .family(theme::fam_medium())
                                .size(13.0)
                                .color(if selected { INK } else { DIM }),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    a11y::label_selectable(&r, label, selected);
                    if r.clicked() || focusnav::activate(ui, &r) {
                        self.tab = tab;
                    }
                    if tab == Tab::Conflicts {
                        controls::count_chip(ui, ctotal);
                    }
                    if selected {
                        active_rect = Some(r.rect);
                        self.skip_target = Some(r.id);
                    }
                }
                ui.min_rect().bottom()
            })
            .inner;
        if let Some(r) = active_rect {
            let x =
                ui.ctx()
                    .animate_value_with_time(egui::Id::new("tab-underline-x"), r.left(), 0.14);
            let w =
                ui.ctx()
                    .animate_value_with_time(egui::Id::new("tab-underline-w"), r.width(), 0.14);
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, bar_bottom + 4.0), egui::vec2(w, 2.0)),
                1.0,
                ACCENT,
            );
        }
        ui.add_space(8.0);

        let Some(d) = detail else {
            for w in [260.0, 200.0, 230.0] {
                theme::skeleton(ui, w);
            }
            return;
        };
        if let Some(err) = &d.error {
            ui.colored_label(BAD, format!("could not load this session: {err}"));
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Overview => self.tab_overview(ui, d),
                Tab::Peers => self.tab_peers(ui, d),
                Tab::Files => self.tab_files(ui, &dir, d),
                Tab::Conflicts => self.tab_conflicts(ui, &dir, d),
                Tab::History => self.tab_history(ui, &dir, d),
                Tab::Audit => self.tab_audit(ui, d),
                Tab::Settings => self.tab_settings(ui, &dir, d),
            });
    }

    fn tab_overview(&mut self, ui: &mut egui::Ui, d: &Detail) {
        components::ledger_stats(
            ui,
            &[
                (format!("{}", d.files_total.max(d.files.len())), "files"),
                (if d.strict { "strict" } else { "easy" }.to_string(), "mode"),
                (
                    if d.role.is_empty() { "?" } else { &d.role }.to_string(),
                    "role",
                ),
                (
                    format!("{}", d.members.iter().filter(|m| m.online).count()),
                    "peers online",
                ),
                (format!("{}", d.conflicts.len()), "conflicts"),
            ],
        );
        ui.add_space(6.0);

        theme::section(ui, "Members");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if d.members.is_empty() {
                ui.label(
                    egui::RichText::new(if d.running {
                        copy::MEMBERS_EMPTY_RUNNING
                    } else {
                        copy::MEMBERS_EMPTY_STOPPED
                    })
                    .color(DIM),
                );
            } else {
                for m in &d.members {
                    ui.horizontal(|ui| {
                        health::signal_arcs(
                            ui,
                            telemetry::grade_lit(&m.grade),
                            if m.online { GOOD } else { theme::FAINT },
                        );
                        let name = m.name.clone().unwrap_or_else(|| m.id_short.clone());
                        ui.label(
                            egui::RichText::new(name)
                                .family(theme::fam_medium())
                                .color(INK),
                        );
                        if m.via_lan {
                            theme::pill(ui, "LAN", theme::LAPIS);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(match m.rtt_ms {
                                    Some(r) => format!("{r} ms"),
                                    None => "—".into(),
                                })
                                .size(11.5)
                                .color(DIM),
                            );
                            ui.label(
                                egui::RichText::new(format!("{} · {}", m.grade, m.conn))
                                    .size(11.5)
                                    .color(theme::FAINT),
                            );
                        });
                    });
                }
            }
        });

        if !d.pulls.is_empty() || d.backlog > 0 || d.resuming > 0 {
            ui.add_space(4.0);
            theme::section(ui, "Transfers");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for p in &d.pulls {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&p.path).size(12.0).color(INK));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} / {} · {}% · {}/s",
                                    human_bytes(p.bytes_done),
                                    human_bytes(p.bytes_total),
                                    p.percent,
                                    human_bytes(p.rate)
                                ))
                                .size(11.0)
                                .color(DIM),
                            );
                        });
                    });
                    components::progress_gold(ui, p.percent as f32 / 100.0);
                    ui.add_space(2.0);
                }
                let mut meta = format!("backlog {} · resuming {}", d.backlog, d.resuming);
                if d.download_limit_bps > 0 {
                    meta.push_str(&format!(
                        " · capped {}/s",
                        human_bytes(d.download_limit_bps)
                    ));
                }
                ui.label(egui::RichText::new(meta).size(10.5).color(theme::FAINT));
            });
        }

        if !d.events.is_empty() {
            ui.add_space(4.0);
            theme::section(ui, "Activity");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for e in d.events.iter().rev().take(8) {
                    ui.horizontal(|ui| {
                        controls::diamond_bullet(ui);
                        ui.label(
                            egui::RichText::new(&e.text)
                                .size(11.5)
                                .color(DIM)
                                .family(egui::FontFamily::Monospace),
                        );
                    });
                }
            });
        }

        ui.add_space(4.0);
        theme::section(ui, "Invite");
        let qr = d.invite.as_ref().and_then(|t| self.qr_texture(ui.ctx(), t));
        components::notched_card(ui, Some(ACCENT), |ui| {
            ui.set_width(ui.available_width());
            match &d.invite {
                Some(t) => {
                    ui.label(
                        egui::RichText::new(copy::INVITE_CAUTION)
                            .size(11.5)
                            .color(DIM),
                    );
                    ceremony::ticket_card(ui, t, qr.as_ref());
                    if components::bevel_primary(ui, "Copy ticket").clicked() {
                        ui.ctx().copy_text(t.clone());
                        self.toasts.push(
                            copy::TOAST_TICKET_COPIED.into(),
                            toasts::Kind::Info,
                            ui.input(|i| i.time),
                        );
                    }
                }
                None => {
                    ui.label(egui::RichText::new(copy::INVITE_EMPTY).color(DIM));
                }
            }
        });
    }

    fn tab_peers(&mut self, ui: &mut egui::Ui, d: &Detail) {
        if d.members.is_empty() {
            components::empty_state(
                ui,
                copy::PEERS_EMPTY_TITLE,
                if d.running {
                    copy::PEERS_EMPTY_RUNNING
                } else {
                    copy::PEERS_EMPTY_STOPPED
                },
            );
            return;
        }
        ui.label(egui::RichText::new(copy::PEERS_INTRO).size(11.5).color(DIM));
        rhythm::space(ui, 2);

        // The mesh as a shape before the mesh as a list.
        let stars: Vec<constellation::Star<'_>> = d
            .members
            .iter()
            .map(|m| constellation::Star {
                id: m.id_short.as_str(),
                name: m.name.as_deref().unwrap_or(m.id_short.as_str()),
                // Gated on an actual path, not on `online`: the daemon calls a
                // peer online when it has merely been seen in presence gossip,
                // which can be via a third node. The telemetry ring never ages
                // out, so keying on `online` would show that peer its last
                // round-trip forever — and draw it a direct thread it does not
                // have.
                rtt_ms: if has_path(m) {
                    self.telemetry
                        .last_ms(&m.id_short)
                        .or(m.rtt_ms)
                        .map(|ms| u32::try_from(ms).unwrap_or(u32::MAX))
                } else {
                    None
                },
                relayed: m.conn.eq_ignore_ascii_case("relayed"),
                online: m.online && has_path(m),
                grade: Some(m.grade.as_str()),
            })
            .collect();
        let width = ui.available_width();
        let height = constellation::desired_height(width, stars.len());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let now = ui.input(|i| i.time);
        if self.sky_key.as_deref() != Some(d.dir.as_str()) {
            self.sky_key = Some(d.dir.clone());
            self.sky_start = now;
        }
        let sky_t = (((now - self.sky_start) / SKY_REVEAL) as f32).clamp(0.0, 1.0);
        if sky_t < 1.0 {
            // Nothing else drives frames this fast; REFRESH is 1.5s.
            ui.ctx().request_repaint();
        }
        let hovered = constellation::sky(ui, rect, &stars, sky_t);
        rhythm::space(ui, 2);

        for (i, m) in d.members.iter().enumerate() {
            let grade_color = match m.grade.as_str() {
                "Good" => GOOD,
                "Fair" => WARN,
                "Poor" => BAD,
                _ => theme::FAINT,
            };
            // Hovering a star in the sky lights its row, so the shape and the
            // list are legibly the same peers.
            let lit = hovered == Some(i);
            let card = if lit {
                theme::card().stroke(egui::Stroke::new(1.0, ACCENT.linear_multiply(0.55)))
            } else {
                theme::card()
            };
            card.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    health::signal_arcs(ui, telemetry::grade_lit(&m.grade), grade_color);
                    let name = m.name.clone().unwrap_or_else(|| m.id_short.clone());
                    ui.label(
                        egui::RichText::new(name)
                            .family(theme::fam_semibold())
                            .size(13.5)
                            .color(INK),
                    );
                    ui.label(
                        egui::RichText::new(&m.id_short)
                            .size(10.5)
                            .family(egui::FontFamily::Monospace)
                            .color(theme::FAINT),
                    );
                    theme::pill(ui, &m.conn, if m.online { GOOD } else { theme::FAINT });
                    if m.via_lan {
                        theme::pill(ui, "LAN", theme::LAPIS);
                    } else if m.relay_url.is_some() {
                        theme::pill(ui, "relay", DIM);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // No path, no round-trip. The ring's last sample is
                        // history, and `online` alone can mean "seen in gossip
                        // via someone else" — showing that as a live figure is
                        // the same lie as showing an offline peer's last RTT.
                        let live_rtt = if has_path(m) {
                            self.telemetry.last_ms(&m.id_short).or(m.rtt_ms)
                        } else {
                            None
                        };
                        ui.label(
                            egui::RichText::new(match live_rtt {
                                Some(r) if m.jitter_ms > 0.05 => {
                                    format!("{r} ms ±{:.1}", m.jitter_ms)
                                }
                                Some(r) => format!("{r} ms"),
                                None => "—".into(),
                            })
                            .size(11.0)
                            .family(egui::FontFamily::Monospace)
                            .color(DIM),
                        );
                        health::sparkline(
                            ui,
                            &self.telemetry.series(&m.id_short),
                            egui::vec2(120.0, 24.0),
                            if m.online { theme::LAPIS } else { theme::FAINT },
                        );
                    });
                });
                ui.horizontal(|ui| {
                    health::rate_arrows(
                        ui,
                        &telemetry::fmt_rate(m.rate_tx),
                        &telemetry::fmt_rate(m.rate_rx),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "lifetime · up {} · down {}",
                            telemetry::fmt_total(m.bytes_tx),
                            telemetry::fmt_total(m.bytes_rx)
                        ))
                        .size(10.5)
                        .color(theme::FAINT),
                    );
                    if let Some(ms) = m.ttd_ms {
                        theme::pill(ui, &format!("direct in {:.1}s", ms as f64 / 1000.0), GOOD);
                    }
                    if m.flaps > 0 {
                        theme::pill(ui, &format!("{} flaps/min", m.flaps), WARN);
                    }
                });
            });
            ui.add_space(4.0);
        }
    }

    fn tab_files(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        ui.horizontal(|ui| {
            let out = fields::search_field(ui, &mut self.file_filter, copy::HINT_FILTER, 280.0);
            if out.cleared {
                self.file_filter.clear();
            }
            // Ctrl+F asked for this field; the tab switch only reaches it now.
            if std::mem::take(&mut self.focus_filter) {
                out.response.request_focus();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let by_size = self.files_sort == grouping::SortMode::Size;
                if ui
                    .selectable_label(by_size, copy::FILES_SORT_SIZE)
                    .clicked()
                {
                    self.files_sort = grouping::SortMode::Size;
                }
                if ui
                    .selectable_label(!by_size, copy::FILES_SORT_NAME)
                    .clicked()
                {
                    self.files_sort = grouping::SortMode::Name;
                }
                ui.label(
                    egui::RichText::new(copy::FILES_SORT_LABEL)
                        .size(10.5)
                        .color(theme::FAINT),
                );
            });
        });
        if d.files_truncated {
            ui.label(
                egui::RichText::new(copy::files_truncated(d.files.len(), d.files_total))
                    .size(11.0)
                    .color(WARN),
            );
        }
        ui.add_space(4.0);
        let filter = self.file_filter.to_lowercase();
        let rows: Vec<FileRow> = d
            .files
            .iter()
            .filter(|f| filter.is_empty() || f.path.to_lowercase().contains(&filter))
            .cloned()
            .collect();
        if rows.is_empty() {
            if filter.is_empty() {
                components::empty_state(ui, copy::FILES_EMPTY_TITLE, copy::FILES_EMPTY_HINT);
            } else {
                ui.label(egui::RichText::new(copy::FILES_FILTER_EMPTY).color(DIM));
            }
            return;
        }
        // Grouped by top-level folder, each group carrying its share of the
        // session's bytes — a flat list hides where the weight actually sits.
        let keys: Vec<(String, u64)> = rows.iter().map(|f| (f.path.clone(), f.size)).collect();
        for g in grouping::group_files(&keys, self.files_sort) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if g.root {
                    ui.label(egui::RichText::new(&g.name).size(11.5).color(theme::FAINT));
                } else {
                    controls::diamond_bullet(ui);
                    ui.label(
                        egui::RichText::new(&g.name)
                            .family(theme::fam_semibold())
                            .size(13.0)
                            .color(INK),
                    );
                }
                ui.label(
                    egui::RichText::new(grouping::group_caption(
                        g.indices.len(),
                        &human_bytes(g.bytes),
                        g.share,
                    ))
                    .size(10.5)
                    .color(theme::FAINT),
                );
            });
            components::progress_gold(ui, g.share);
            ui.add_space(2.0);
            for i in g.indices {
                let Some(f) = rows.get(i) else { continue };
                self.file_card(ui, dir, d, f);
            }
        }
    }

    /// One file row: name, type chip, size, lease seal, actions, and its
    /// version history when expanded.
    fn file_card(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail, f: &FileRow) {
        let open = self.open_versions.contains(&f.path);
        let has_versions = d.versions.contains_key(&f.path);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if has_versions && controls::chevron(ui, open).clicked() {
                    if open {
                        self.open_versions.remove(&f.path);
                    } else {
                        self.open_versions.insert(f.path.clone());
                    }
                }
                ui.label(
                    egui::RichText::new(&f.path)
                        .family(theme::fam_medium())
                        .color(INK),
                );
                components::ext_chip(ui, &f.path);
                ui.label(
                    egui::RichText::new(human_bytes(f.size))
                        .size(11.5)
                        .color(theme::FAINT),
                );
                match &f.locked_by {
                    Some(h) if f.mine_lock => {
                        let _ = h;
                        components::seal_dot(ui, ACCENT, true);
                        theme::pill(ui, "locked · you", ACCENT);
                    }
                    Some(h) => {
                        components::seal_dot(ui, WARN, true);
                        theme::pill(ui, &format!("locked · {}", short(h)), WARN);
                    }
                    None => {}
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if d.running {
                        if f.locked_by.is_some() {
                            if f.mine_lock && controls::ghost_small(ui, "Unlock").clicked() {
                                self.send(Cmd::Unlock {
                                    dir: dir.to_path_buf(),
                                    path: f.path.clone(),
                                });
                            }
                        } else if controls::ghost_small(ui, "Lock").clicked() {
                            self.send(Cmd::Lock {
                                dir: dir.to_path_buf(),
                                path: f.path.clone(),
                            });
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(copy::FILES_STOPPED_ACTION)
                                .size(10.5)
                                .color(theme::FAINT),
                        );
                    }
                });
            });
            if open && let Some(versions) = d.versions.get(&f.path) {
                ui.add_space(2.0);
                let ctx = VersionCtx::new(d);
                for v in versions {
                    self.version_row(ui, dir, &f.path, v, d.running, ctx);
                }
            }
        });
        ui.add_space(4.0);
    }

    /// One version row (shared by Files expanders and the History tab).
    fn version_row(
        &mut self,
        ui: &mut egui::Ui,
        dir: &Path,
        path: &str,
        v: &VersionRow,
        running: bool,
        ctx: VersionCtx,
    ) {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(format!("v{}", v.n))
                    .size(11.5)
                    .family(egui::FontFamily::Monospace)
                    .color(theme::LAPIS),
            );
            ui.label(
                egui::RichText::new(fmt_ts_full(v.ts_ms))
                    .size(11.5)
                    .color(DIM),
            );
            // Monospace so the padding actually lines the column up.
            ui.label(
                egui::RichText::new(figures::align(
                    &figures::split(&human_bytes(v.size)),
                    ctx.width,
                ))
                .size(11.5)
                .family(egui::FontFamily::Monospace)
                .color(DIM),
            );
            ui.label(
                egui::RichText::new(figures::ago(v.ts_ms / 1000, ctx.now_s))
                    .size(11.0)
                    .color(theme::FAINT),
            );
            if v.pinned {
                theme::pill(ui, "pinned", ACCENT);
            }
            if let Some(tag) = &v.tag {
                theme::pill(ui, tag, theme::LAPIS);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !running {
                    ui.label(
                        egui::RichText::new(copy::VERSION_STOPPED_ACTION)
                            .size(10.5)
                            .color(theme::FAINT),
                    );
                    return;
                }
                if controls::ghost_small(ui, "Restore").clicked() {
                    self.ask(
                        copy::RESTORE_TITLE,
                        copy::restore_body(path, v.n, &human_bytes(v.size)),
                        "Restore",
                        false,
                        Cmd::Restore {
                            dir: dir.to_path_buf(),
                            path: path.to_string(),
                            n: v.n as usize,
                        },
                    );
                }
                if controls::ghost_small(ui, if v.pinned { "Unpin" } else { "Pin" }).clicked() {
                    self.send(Cmd::Pin {
                        dir: dir.to_path_buf(),
                        path: path.to_string(),
                        n: v.n as usize,
                        pinned: !v.pinned,
                    });
                }
                let editing = self
                    .tag_edit
                    .as_ref()
                    .is_some_and(|(p, n, _)| p == path && *n == v.n as usize);
                if editing {
                    let mut done: Option<Option<String>> = None;
                    if let Some((_, _, buf)) = self.tag_edit.as_mut() {
                        let r = fields::text_field(
                            ui,
                            buf,
                            copy::HINT_TAG,
                            110.0,
                            fields::FieldState::Neutral,
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let t = buf.trim().to_string();
                            done = Some(if t.is_empty() { None } else { Some(t) });
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.tag_edit = None;
                        }
                    }
                    if let Some(name) = done {
                        self.send(Cmd::Tag {
                            dir: dir.to_path_buf(),
                            path: path.to_string(),
                            n: v.n as usize,
                            name,
                        });
                        self.tag_edit = None;
                    }
                } else if controls::ghost_small(ui, "Tag").clicked() {
                    self.tag_edit = Some((
                        path.to_string(),
                        v.n as usize,
                        v.tag.clone().unwrap_or_default(),
                    ));
                }
            });
        });
    }

    fn tab_history(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        if d.versions.is_empty() {
            components::empty_state(
                ui,
                copy::HISTORY_EMPTY_TITLE,
                if d.running {
                    copy::HISTORY_EMPTY_HINT_RUNNING
                } else {
                    copy::HISTORY_EMPTY_HINT_STOPPED
                },
            );
            return;
        }
        // Flattened, newest first, across every path.
        let mut all: Vec<(String, VersionRow)> = d
            .versions
            .iter()
            .flat_map(|(p, vs)| vs.iter().map(|v| (p.clone(), v.clone())))
            .collect();
        all.sort_by_key(|e| std::cmp::Reverse(e.1.ts_ms));
        all.truncate(200);
        let vctx = VersionCtx::new(d);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut last_day = String::new();
            for (path, v) in &all {
                let day = fmt_day(v.ts_ms);
                if day != last_day {
                    components::day_rule(ui, &day);
                    last_day = day;
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(path)
                            .family(theme::fam_medium())
                            .size(12.5)
                            .color(INK),
                    );
                });
                self.version_row(ui, dir, path, v, d.running, vctx);
                ui.add_space(2.0);
            }
        });
    }

    fn tab_conflicts(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        if d.conflicts.is_empty() {
            components::empty_state(ui, copy::CONFLICTS_EMPTY_TITLE, copy::CONFLICTS_EMPTY_HINT);
            return;
        }
        ui.label(
            egui::RichText::new(copy::CONFLICTS_INTRO)
                .size(11.5)
                .color(DIM),
        );
        ui.add_space(4.0);
        for c in &d.conflicts {
            components::notched_card(ui, Some(WARN), |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(if c.path.is_empty() { &c.name } else { &c.path })
                            .family(theme::fam_medium())
                            .color(INK),
                    );
                    theme::pill(ui, &c.reason, WARN);
                });
                // Weigh the two copies against each other rather than merely
                // listing one of them: the choice ahead is which bytes live.
                let live = d.files.iter().find(|f| f.path == c.path);
                let kept_text = human_bytes(c.size);
                let kept_when = format!("kept {}", fmt_ts_full(c.ts_ms));
                let live_text = live.map(|f| human_bytes(f.size)).unwrap_or_default();
                balance::scales(
                    ui,
                    balance::Side {
                        title: copy::BAL_KEPT_TITLE,
                        size: c.size,
                        size_text: &kept_text,
                        when: &kept_when,
                        note: &c.reason,
                        accent: WARN,
                    },
                    balance::Side {
                        title: copy::BAL_LIVE_TITLE,
                        size: live.map(|f| f.size).unwrap_or(0),
                        size_text: &live_text,
                        when: copy::BAL_LIVE_WHEN,
                        note: if live.is_some() {
                            copy::BAL_LIVE_NOTE
                        } else {
                            copy::BAL_LIVE_MISSING
                        },
                        accent: theme::LAPIS,
                    },
                );
                ui.add_space(4.0);
                if d.running {
                    ui.horizontal(|ui| {
                        if !c.path.is_empty()
                            && components::bevel_primary(ui, "Keep mine (apply the copy)").clicked()
                        {
                            // Guided lock → apply → unlock → discard; the copy
                            // is only discarded once the publish succeeds.
                            self.send(Cmd::ResolveMine {
                                dir: dir.to_path_buf(),
                                id: c.name.clone(),
                                target: c.path.clone(),
                            });
                        }
                        if controls::bevel_danger(ui, "Keep theirs (discard copy)").clicked() {
                            self.ask(
                                copy::CONFLICT_DISCARD_TITLE,
                                copy::conflict_discard_body(&c.name),
                                "Discard the copy",
                                true,
                                Cmd::ConflictDiscard {
                                    dir: dir.to_path_buf(),
                                    id: c.name.clone(),
                                },
                            );
                        }
                    });
                } else {
                    ui.label(
                        egui::RichText::new(copy::CONFLICTS_STOPPED_NOTE)
                            .size(10.5)
                            .color(theme::FAINT),
                    );
                }
            });
            ui.add_space(4.0);
        }
    }

    fn tab_audit(&mut self, ui: &mut egui::Ui, d: &Detail) {
        if d.audit.is_empty() {
            components::empty_state(ui, copy::AUDIT_EMPTY_TITLE, copy::AUDIT_EMPTY_HINT);
            return;
        }
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            for e in &d.audit {
                let mut extra = String::new();
                if let Some(p) = &e.path {
                    extra.push_str(p);
                }
                if let Some(pe) = &e.peer {
                    extra.push_str(&format!("  ‹{}›", short(pe)));
                }
                if let Some(de) = &e.detail {
                    if !extra.is_empty() {
                        extra.push_str("  ");
                    }
                    extra.push_str(de);
                }
                let kind = e.kind.clone();
                let color = audit_color(&kind);
                components::timeline_row(ui, color, &fmt_ts(e.ts_ms), |ui| {
                    ui.horizontal(|ui| {
                        theme::pill(ui, &kind, color);
                        ui.label(egui::RichText::new(extra).size(11.5).color(DIM));
                    });
                });
            }
        });
    }

    fn tab_settings(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        let Some(cfg) = d.config.clone() else {
            components::empty_state(
                ui,
                copy::SETTINGS_NEED_DAEMON_TITLE,
                copy::SETTINGS_NEED_DAEMON_HINT,
            );
            return;
        };

        theme::section(ui, "Live settings");
        ui.label(
            egui::RichText::new(copy::LIVE_SECTION_NOTE)
                .size(11.0)
                .color(theme::FAINT),
        );
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.cfg_row(
                ui,
                dir,
                "lease-ttl",
                "lease duration",
                &human_dur(cfg.lease_ttl_ms),
            );
            self.cfg_row(
                ui,
                dir,
                "acquire-timeout",
                "lock acquire timeout",
                &fmt_ms_exact(cfg.acquire_timeout_ms),
            );
            self.cfg_row(
                ui,
                dir,
                "wait-timeout",
                "lock --wait timeout",
                &fmt_ms_exact(cfg.wait_timeout_ms),
            );
            self.cfg_row(
                ui,
                dir,
                "max-down",
                "download cap (0 = unlimited)",
                &if cfg.max_down == 0 {
                    "0".to_string()
                } else {
                    format!("{}", cfg.max_down)
                },
            );
            self.cfg_row(
                ui,
                dir,
                "dashboard-port",
                "web dashboard port",
                &format!("{}", cfg.dashboard_port),
            );
            self.cfg_row(
                ui,
                dir,
                "update-channel",
                "update channel",
                &cfg.update_channel,
            );
            self.cfg_toggle(
                ui,
                dir,
                "autolock",
                "auto-acquire on edit",
                Some(cfg.autolock),
            );
            self.cfg_toggle(ui, dir, "audit", "audit log", None);
            self.cfg_toggle(ui, dir, "hooks", "event hooks", None);
            self.cfg_toggle(ui, dir, "notify", "desktop notifications", None);
        });

        ui.add_space(6.0);
        theme::section(ui, "Fixed until restart");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (k, v) in [
                (
                    "mode",
                    if cfg.strict {
                        "strict".to_string()
                    } else {
                        "easy".to_string()
                    },
                ),
                ("role", cfg.role.clone()),
                (
                    "relay",
                    cfg.relay.clone().unwrap_or_else(|| "default (N0)".into()),
                ),
                ("lan", if cfg.lan { "on".into() } else { "off".into() }),
            ] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(k).size(12.0).color(DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(v)
                                .size(12.0)
                                .family(egui::FontFamily::Monospace)
                                .color(INK),
                        );
                    });
                });
            }
            ui.label(
                egui::RichText::new(copy::FIXED_SECTION_NOTE)
                    .size(10.5)
                    .color(theme::FAINT),
            );
        });

        ui.add_space(6.0);
        theme::section(ui, "Peers");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(copy::PEERS_NAME_HINT)
                    .size(11.5)
                    .color(DIM),
            );
            ui.horizontal(|ui| {
                fields::text_field_mono(ui, &mut self.peer_id, copy::HINT_PEER_ID, 180.0);
                fields::text_field(
                    ui,
                    &mut self.peer_name,
                    copy::HINT_PEER_NAME,
                    140.0,
                    fields::FieldState::Neutral,
                );
                if components::bevel_primary(ui, "Save").clicked()
                    && !self.peer_id.trim().is_empty()
                {
                    let name = self.peer_name.trim();
                    self.send(Cmd::PeerName {
                        dir: dir.to_path_buf(),
                        id: self.peer_id.trim().to_string(),
                        name: (!name.is_empty()).then(|| name.to_string()),
                    });
                }
            });
            for m in &d.members {
                ui.label(
                    egui::RichText::new(&m.id_short)
                        .size(11.0)
                        .family(egui::FontFamily::Monospace)
                        .color(theme::FAINT),
                );
            }
        });

        if !d.leases.is_empty() {
            ui.add_space(6.0);
            theme::section(ui, "Active leases");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for l in &d.leases {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&l.path).size(12.0).color(INK));
                        theme::pill(
                            ui,
                            &if l.mine {
                                "you".to_string()
                            } else {
                                short(&l.holder)
                            },
                            if l.mine { ACCENT } else { DIM },
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} left", human_dur(l.expires_in_ms)))
                                    .size(11.0)
                                    .color(theme::FAINT),
                            );
                        });
                    });
                }
            });
        }
    }

    /// One editable live-config row: label, current value, edit buffer, apply.
    fn cfg_row(&mut self, ui: &mut egui::Ui, dir: &Path, key: &str, label: &str, current: &str) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).size(12.0).color(DIM));
            ui.label(
                egui::RichText::new(format!("({key})"))
                    .size(10.0)
                    .color(theme::FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The entry is (baseline, buffer). Baseline = the daemon value
                // the buffer was seeded from, so an external change (peer/CLI)
                // is followed when the user has not typed, and a user edit is
                // never silently replaced OR left offering a stale revert.
                if self.cfg_edits.get(key).is_some_and(|(_, b)| b == current) {
                    self.cfg_edits.remove(key);
                } else if let Some((base, buf)) = self.cfg_edits.get_mut(key)
                    && *base != current
                    && buf == base
                {
                    *base = current.to_string();
                    *buf = current.to_string();
                }
                let dirty = self.cfg_edits.get(key).is_some_and(|(_, b)| b != current);
                // RTL: first added lands rightmost — Apply, reset, then the field
                // (which must stay visible while dirty, or typing hides it).
                if dirty {
                    if components::bevel_primary(ui, "Apply").clicked()
                        && let Some((_, buf)) = self.cfg_edits.get(key)
                    {
                        let value = buf.trim().to_string();
                        // The entry deliberately stays dirty: that *is* the
                        // pending state, and it is what the user gets back if
                        // the daemon refuses the value. Reseeding base and buf
                        // here would match the external-change branch above on
                        // the very next frame and reseed both to the *old*
                        // daemon value — reverting a good edit for a whole poll
                        // interval, and silently discarding a rejected one.
                        // It clears itself through the `buf == current` check
                        // once the daemon reports the new value back.
                        self.send(Cmd::ConfigSet {
                            dir: dir.to_path_buf(),
                            key: key.to_string(),
                            value,
                        });
                    }
                    if controls::ghost_small(ui, "reset").clicked() {
                        self.cfg_edits.remove(key);
                    }
                }
                let (_, buf) = self
                    .cfg_edits
                    .entry(key.to_string())
                    .or_insert_with(|| (current.to_string(), current.to_string()));
                fields::text_field(ui, buf, "", 120.0, fields::FieldState::Neutral);
            });
        });
    }

    /// An on/off live key. `current` is None when the payload does not report
    /// the value (audit/hooks/notify) — both buttons are offered blind.
    fn cfg_toggle(
        &mut self,
        ui: &mut egui::Ui,
        dir: &Path,
        key: &str,
        label: &str,
        current: Option<bool>,
    ) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).size(12.0).color(DIM));
            ui.label(
                egui::RichText::new(format!("({key})"))
                    .size(10.0)
                    .color(theme::FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let on_sel = current == Some(true);
                let off_sel = current == Some(false);
                if ui.selectable_label(off_sel, "off").clicked() {
                    self.send(Cmd::ConfigSet {
                        dir: dir.to_path_buf(),
                        key: key.to_string(),
                        value: "off".into(),
                    });
                }
                if ui.selectable_label(on_sel, "on").clicked() {
                    self.send(Cmd::ConfigSet {
                        dir: dir.to_path_buf(),
                        key: key.to_string(),
                        value: "on".into(),
                    });
                }
            });
        });
    }

    /// Builds (and caches) the QR texture for a ticket. Dark modules on a light
    /// panel so phone cameras read it off the dark UI.
    fn qr_texture(&mut self, ctx: &egui::Context, ticket: &str) -> Option<egui::TextureHandle> {
        if let Some((t, tex)) = &self.qr
            && t == ticket
        {
            return Some(tex.clone());
        }
        let code = qrcode::QrCode::new(ticket.as_bytes()).ok()?;
        let w = code.width();
        let colors = code.to_colors();
        let quiet = 4usize;
        let side = w + quiet * 2;
        let mut pixels = vec![egui::Color32::from_rgb(0xe9, 0xec, 0xf8); side * side];
        for y in 0..w {
            for x in 0..w {
                if colors[y * w + x] == qrcode::Color::Dark {
                    pixels[(y + quiet) * side + (x + quiet)] =
                        egui::Color32::from_rgb(0x0a, 0x0f, 0x1e);
                }
            }
        }
        let img = egui::ColorImage::new([side, side], pixels);
        let tex = ctx.load_texture("invite-qr", img, egui::TextureOptions::NEAREST);
        self.qr = Some((ticket.to_string(), tex.clone()));
        Some(tex)
    }

    /// Global shortcuts: Ctrl+K palette, Ctrl+R refresh.
    /// Every chord `shortcuts::sections()` advertises, wired here so the sheet
    /// and the window can never drift. Escape is deliberately absent: each
    /// overlay consumes its own, so one press closes exactly one thing.
    fn keyboard(&mut self, ui: &egui::Ui, overview: &Option<Overview>) {
        const TAB_KEYS: [(egui::Key, Tab); 7] = [
            (egui::Key::Num1, Tab::Overview),
            (egui::Key::Num2, Tab::Peers),
            (egui::Key::Num3, Tab::Files),
            (egui::Key::Num4, Tab::Conflicts),
            (egui::Key::Num5, Tab::History),
            (egui::Key::Num6, Tab::Audit),
            (egui::Key::Num7, Tab::Settings),
        ];
        // While a text field owns the keyboard a bare "?" is a character being
        // typed, not a request for the sheet.
        let typing = ui.ctx().memory(|m| m.focused()).is_some();
        let cmd = egui::Modifiers::COMMAND;

        let (palette, refresh, settings, filter, bigger, smaller, reset, sheet, all, jump) = ui
            .input_mut(|i| {
                (
                    i.consume_key(cmd, egui::Key::K),
                    i.consume_key(cmd, egui::Key::R),
                    i.consume_key(cmd, egui::Key::Comma),
                    i.consume_key(cmd, egui::Key::F),
                    {
                        // Consume both spellings: layouts disagree on whether
                        // Ctrl and the "+" key reports Plus or Equals, and a
                        // short-circuit would leave the other one pending.
                        let plus = i.consume_key(cmd, egui::Key::Plus);
                        let equals = i.consume_key(cmd, egui::Key::Equals);
                        plus || equals
                    },
                    i.consume_key(cmd, egui::Key::Minus),
                    i.consume_key(cmd, egui::Key::Num0),
                    !typing && i.consume_key(egui::Modifiers::NONE, egui::Key::Questionmark),
                    // While a field has the keyboard, Ctrl+A selects its text,
                    // not every session.
                    !typing && i.consume_key(cmd, egui::Key::A),
                    TAB_KEYS
                        .iter()
                        .find(|(k, _)| i.consume_key(cmd, *k))
                        .map(|(_, t)| *t),
                )
            });

        if palette {
            self.palette_open = !self.palette_open;
            self.palette_query.clear();
            self.palette_sel = 0;
        }
        if refresh {
            self.send(Cmd::Refresh);
        }
        if all && let Some(ov) = overview {
            let keys: Vec<String> = ov.sessions.iter().map(|s| s.path.clone()).collect();
            self.multi.select_all(&keys);
        }
        if sheet {
            self.shortcuts_open = !self.shortcuts_open;
            // Keep the two modals mutually exclusive so Escape is unambiguous.
            self.palette_open = false;
        }

        // Tab chords only mean something once a session is on screen.
        if self.selected.is_some() {
            if let Some(t) = jump {
                self.tab = t;
            }
            if settings {
                self.tab = Tab::Settings;
            }
            if filter {
                self.tab = Tab::Files;
                self.focus_filter = true;
            }
        }

        if bigger || smaller || reset {
            let next = if reset {
                a11y::SCALE_DEFAULT
            } else {
                a11y::step_scale(self.text_scale, bigger)
            };
            if next != self.text_scale {
                self.text_scale = next;
                self.scale_dirty = Some(next);
                self.toasts.push(
                    a11y::scale_label(next),
                    toasts::Kind::Info,
                    ui.input(|i| i.time),
                );
            }
        }
    }

    /// The Ctrl+K command palette: fuzzy filter over sessions and actions.
    fn palette_overlay(&mut self, ui: &mut egui::Ui, overview: &Option<Overview>) {
        if !self.palette_open {
            return;
        }
        if modal_backdrop(ui.ctx(), "palette-dim", 140) {
            self.palette_open = false;
            return;
        }

        // Build the action list.
        enum Act {
            Go(Option<String>),
            Start(String),
            Stop(String),
            OpenTab(Tab),
            Colophon,
            Refresh,
            Quit,
        }
        let mut acts: Vec<(String, Act)> = vec![("Home — all sessions".into(), Act::Go(None))];
        if let Some(ov) = overview {
            for s in &ov.sessions {
                acts.push((format!("Open  {}", s.name), Act::Go(Some(s.path.clone()))));
                if s.running {
                    acts.push((format!("Stop  {}", s.name), Act::Stop(s.path.clone())));
                } else {
                    acts.push((format!("Start  {}", s.name), Act::Start(s.path.clone())));
                }
            }
        }
        if self.selected.is_some() {
            for (t, label) in [
                (Tab::Peers, "Go to Peers"),
                (Tab::Files, "Go to Files"),
                (Tab::Conflicts, "Go to Conflicts"),
                (Tab::History, "Go to History"),
                (Tab::Audit, "Go to Audit"),
                (Tab::Settings, "Go to Settings"),
            ] {
                acts.push((label.into(), Act::OpenTab(t)));
            }
        }
        acts.push(("Colophon — about tazamun".into(), Act::Colophon));
        acts.push(("Refresh now".into(), Act::Refresh));
        acts.push(("Quit tazamun".into(), Act::Quit));

        let q = self.palette_query.to_lowercase();
        let filtered: Vec<(String, Act)> = acts
            .into_iter()
            .filter(|(label, _)| fuzzy_match(&label.to_lowercase(), &q))
            .collect();
        if self.palette_sel >= filtered.len() {
            self.palette_sel = filtered.len().saturating_sub(1);
        }

        let (esc, enter) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            )
        });
        if esc {
            self.palette_open = false;
            return;
        }
        // Wraps at both ends, answers Home/End, and clamps an index left stale
        // by a narrowing filter.
        focusnav::list_nav(ui, filtered.len(), &mut self.palette_sel);

        // A resting pointer must not own the selection. `hovered()` is true
        // for a stationary pointer, so taking it unconditionally means an arrow
        // key moves the highlight and the draw loop snaps it straight back —
        // and Enter then runs the *hovered* row, which can be Quit or Stop.
        // `is_moving` is smoothed, so hover resumes as soon as the mouse does.
        let pointer_moving = ui.input(|i| i.pointer.is_moving());
        let mut clicked_row = false;
        egui::Area::new(egui::Id::new("palette"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 90.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::BG1)
                    .stroke(egui::Stroke::new(1.0, ACCENT.linear_multiply(0.35)))
                    .corner_radius(theme::R_CARD + 2)
                    .inner_margin(egui::Margin::same(10))
                    .shadow(egui::Shadow {
                        offset: [0, 6],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(120),
                    })
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        let avail = ui.available_width();
                        let te = fields::text_field(
                            ui,
                            &mut self.palette_query,
                            copy::PALETTE_HINT,
                            avail,
                            fields::FieldState::Neutral,
                        );
                        if !te.has_focus() {
                            te.request_focus();
                        }
                        if te.changed() {
                            self.palette_sel = 0;
                        }
                        ornament::girih_band(
                            ui.painter(),
                            egui::Rect::from_min_max(
                                egui::pos2(te.rect.left() + 4.0, te.rect.bottom() + 2.0),
                                egui::pos2(te.rect.right() - 4.0, te.rect.bottom() + 7.0),
                            ),
                            theme::GOLD.linear_multiply(0.12),
                        );
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (i, (label, _)) in filtered.iter().enumerate() {
                                    let selected = i == self.palette_sel;
                                    let r = ui.add(
                                        egui::Button::new(
                                            egui::RichText::new(label)
                                                .size(13.0)
                                                .color(if selected { INK } else { DIM }),
                                        )
                                        .fill(if selected {
                                            theme::BG3
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        })
                                        .min_size(egui::vec2(ui.available_width(), 28.0)),
                                    );
                                    if selected {
                                        ornament::diamond(
                                            ui.painter(),
                                            egui::pos2(r.rect.left() + 9.0, r.rect.center().y),
                                            2.4,
                                            ACCENT,
                                        );
                                    }
                                    if r.hovered() && pointer_moving {
                                        self.palette_sel = i;
                                    }
                                    if r.clicked() {
                                        self.palette_sel = i;
                                        clicked_row = true;
                                    }
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ceremony::keycap(ui, "↑");
                            ceremony::keycap(ui, "↓");
                            ui.label(egui::RichText::new("move").size(10.0).color(theme::FAINT));
                            ui.add_space(8.0);
                            ceremony::keycap(ui, "Enter");
                            ui.label(egui::RichText::new("run").size(10.0).color(theme::FAINT));
                            ui.add_space(8.0);
                            ceremony::keycap(ui, "Esc");
                            ui.label(egui::RichText::new("close").size(10.0).color(theme::FAINT));
                        });
                    });
            });

        if (enter || clicked_row) && !filtered.is_empty() {
            let (_, act) = &filtered[self.palette_sel.min(filtered.len() - 1)];
            match act {
                Act::Go(p) => self.select(p.clone()),
                Act::Start(p) => self.send(Cmd::Start(PathBuf::from(p))),
                Act::Stop(p) => self.send(Cmd::Stop(PathBuf::from(p))),
                Act::OpenTab(t) => self.tab = *t,
                Act::Colophon => self.colophon_open = true,
                Act::Refresh => self.send(Cmd::Refresh),
                Act::Quit => {
                    // Quitting is the one moment the debounce would lose work.
                    let now = ui.input(|i| i.time);
                    self.flush_prefs(ui.ctx(), now, true);
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            self.palette_open = false;
        }
    }

    /// The colophon (About) overlay — the manuscript's closing page.
    fn colophon_overlay(&mut self, ui: &mut egui::Ui) {
        if !self.colophon_open {
            return;
        }
        if modal_backdrop(ui.ctx(), "colophon-dim", 140) {
            self.colophon_open = false;
            return;
        }
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.colophon_open = false;
            return;
        }
        egui::Area::new(egui::Id::new("colophon"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -20.0))
            .show(ui.ctx(), |ui| {
                let fr = egui::Frame::new()
                    .fill(theme::BG1)
                    .stroke(theme::stroke_faint())
                    .corner_radius(theme::R_CARD + 2)
                    .inner_margin(egui::Margin::same(20))
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(140),
                    })
                    .show(ui, |ui| {
                        ui.set_width(440.0);
                        colophon::colophon(ui, env!("TAZAMUN_VERSION"));
                    });
                ceremony::adorn_dialog(ui.painter(), fr.response.rect, false);
            });
    }

    /// The `?` sheet: every chord the window answers to, rendered straight from
    /// `shortcuts::sections()`. Tall enough to need scrolling at small window
    /// sizes, so the body is capped and scrolls rather than overflowing.
    fn shortcuts_overlay(&mut self, ui: &mut egui::Ui) {
        if !self.shortcuts_open {
            return;
        }
        if modal_backdrop(ui.ctx(), "shortcuts-dim", 140) {
            self.shortcuts_open = false;
            return;
        }
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.shortcuts_open = false;
            return;
        }
        let max_h = (ui.ctx().viewport_rect().height() - 120.0).clamp(200.0, 560.0);
        egui::Area::new(egui::Id::new("shortcuts"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -10.0))
            .show(ui.ctx(), |ui| {
                let fr = egui::Frame::new()
                    .fill(theme::BG1)
                    .stroke(theme::stroke_faint())
                    .corner_radius(theme::R_CARD + 2)
                    .inner_margin(egui::Margin::same(20))
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(140),
                    })
                    .show(ui, |ui| {
                        ui.set_width(400.0);
                        ui.label(
                            egui::RichText::new(copy::SHORTCUTS_TITLE)
                                .size(14.5)
                                .family(theme::fam_semibold())
                                .color(INK),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(copy::SHORTCUTS_SUB)
                                .size(11.5)
                                .color(theme::DIM),
                        );
                        ui.add_space(10.0);
                        egui::ScrollArea::vertical()
                            .max_height(max_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                shortcuts::sheet(ui);
                            });
                    });
                ceremony::adorn_dialog(ui.painter(), fr.response.rect, false);
            });
    }

    /// The confirm modal: nothing destructive fires without an explicit click.
    fn confirm_overlay(&mut self, ui: &mut egui::Ui) {
        let Some(confirm) = self.confirm.as_ref() else {
            return;
        };
        let (title, body, verb, danger) = (
            confirm.title.clone(),
            confirm.body.clone(),
            confirm.verb.clone(),
            confirm.danger,
        );
        if modal_backdrop(ui.ctx(), "confirm-dim", 150) {
            self.confirm = None;
            return;
        }

        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.confirm = None;
            return;
        }
        let mut decided: Option<bool> = None;
        egui::Area::new(egui::Id::new("confirm"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .show(ui.ctx(), |ui| {
                let fr = egui::Frame::new()
                    .fill(theme::BG1)
                    .stroke(if danger {
                        egui::Stroke::new(1.0, BAD.linear_multiply(0.5))
                    } else {
                        theme::stroke_faint()
                    })
                    .corner_radius(theme::R_CARD + 2)
                    .inner_margin(egui::Margin::same(16))
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(140),
                    })
                    .show(ui, |ui| {
                        ui.set_width(420.0);
                        ui.label(
                            egui::RichText::new(&title)
                                .family(theme::fam_semibold())
                                .size(15.0)
                                .color(INK),
                        );
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(&body).size(12.5).color(DIM));
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let r = if danger {
                                        controls::bevel_danger(ui, &verb)
                                    } else {
                                        components::bevel_primary(ui, &verb)
                                    };
                                    if r.clicked() {
                                        decided = Some(true);
                                    }
                                    if controls::ghost_button(ui, "Cancel").clicked() {
                                        decided = Some(false);
                                    }
                                },
                            );
                        });
                    });
                // Adorn over the finished card: the alphas are ghost-level, so
                // the flourishes/seal never fight the text.
                ceremony::adorn_dialog(ui.painter(), fr.response.rect, danger);
            });
        match decided {
            Some(true) => {
                if let Some(mut c) = self.confirm.take()
                    && let Some(action) = c.action.take()
                {
                    self.send(action);
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn toast_overlay(&mut self, ui: &mut egui::Ui) {
        let now = ui.input(|i| i.time);
        self.toasts.expire(now);
        if self.toasts.is_empty() {
            return;
        }
        toasts::draw(ui, &self.toasts, now);
        // Animate the stack's slide/fade while anything is still on screen.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

// ─── small helpers ───────────────────────────────────────────────────────────

/// Full-screen input-blocking dim on the foreground layer. The dialog `Area`
/// registered after this sits on top and receives its own clicks; everything
/// else lands here, so nothing behind a modal is clickable. Returns true when
/// the backdrop itself was clicked (dismiss).
fn modal_backdrop(ctx: &egui::Context, id: &str, alpha: u8) -> bool {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, r) = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(alpha));
            r
        })
        .inner
        .clicked()
}

/// Color-classify an audit event kind for its pill.
fn audit_color(kind: &str) -> egui::Color32 {
    if kind.contains("quarantine") || kind.contains("conflict") {
        WARN
    } else if kind.contains("error") || kind.contains("refus") {
        BAD
    } else if kind.contains("lock") || kind.contains("publish") || kind.contains("restore") {
        GOOD
    } else {
        theme::LAPIS
    }
}

/// Subsequence fuzzy match ("szn" matches "session").
fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut it = haystack.chars();
    'outer: for nc in needle.chars() {
        for hc in it.by_ref() {
            if hc == nc {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

fn status_dot(s: &SessionRow) -> (&'static str, egui::Color32) {
    if !s.readable {
        ("○", BAD)
    } else if s.paused {
        ("⏸", WARN)
    } else if s.running && s.peers_online > 0 {
        ("●", GOOD)
    } else if s.running {
        ("●", WARN)
    } else {
        ("○", DIM)
    }
}

fn status_text(s: &SessionRow) -> String {
    if !s.readable {
        return "unreadable".into();
    }
    let mut t = if s.paused {
        "paused".to_string()
    } else if s.running {
        format!("running · {}/{} peers", s.peers_online, s.peers_total)
    } else {
        "stopped".to_string()
    };
    t.push_str(&format!(
        " · {} · {}",
        s.role,
        if s.strict { "strict" } else { "easy" }
    ));
    if s.files > 0 {
        t.push_str(&format!(" · {} files", s.files));
    }
    t
}

fn base_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn jstr(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn short(id: &str) -> String {
    id.chars().take(10).collect()
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < U.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{f:.1} {}", U[i])
    }
}

fn human_dur(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// Exact humantime rendering ("1m 30s") — round-trips through config parsing,
/// unlike the lossy approximate `human_dur`.
fn fmt_ms_exact(ms: u64) -> String {
    humantime::format_duration(std::time::Duration::from_millis(ms)).to_string()
}

fn fmt_ts(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => "?".into(),
    }
}

fn fmt_day(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(dt) => dt.format("%d %b %Y").to_string(),
        None => "?".into(),
    }
}

/// Everything a version row needs that is the same for every row in a frame:
/// the shared column width and one wall-clock reading. A 200-row history must
/// not call the clock 200 times per frame.
#[derive(Clone, Copy)]
struct VersionCtx {
    width: usize,
    now_s: u64,
}

impl VersionCtx {
    fn new(d: &Detail) -> Self {
        Self {
            width: version_value_width(d),
            now_s: crate::now_ms() / 1000,
        }
    }
}

/// True when this node currently has a live path to the peer. `Member::online`
/// is not that: the daemon reports a peer online when it has been seen in
/// presence gossip inside `ONLINE_WINDOW`, which it may have reached through a
/// third node. `conn` is the authoritative answer — `Direct`, `Relayed`, or
/// `None`.
fn has_path(m: &Member) -> bool {
    !m.conn.eq_ignore_ascii_case("none")
}

fn version_value_width(d: &Detail) -> usize {
    let figs: Vec<figures::Figure> = d
        .versions
        .values()
        .flatten()
        .map(|v| figures::split(&human_bytes(v.size)))
        .collect();
    figures::value_width(&figs)
}

fn fmt_ts_full(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => "?".into(),
    }
}

impl App {
    /// Hidden capture hook for docs and bug reports: run with
    /// `TAZAMUN_GUI_SHOT=/path/prefix` and the app writes one composited frame
    /// (`<prefix>.raw` RGBA + `<prefix>.dim`) about 3s after launch, then exits.
    fn debug_screenshot(&mut self, ui: &egui::Ui) {
        let Ok(path) = std::env::var("TAZAMUN_GUI_SHOT") else {
            return;
        };
        let t = ui.input(|i| i.time);
        if !self.shot_sent && t > 3.0 {
            self.shot_sent = true;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let img = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            let mut bytes = Vec::with_capacity(img.pixels.len() * 4);
            for p in &img.pixels {
                bytes.extend_from_slice(&p.to_array());
            }
            let _ = std::fs::write(format!("{path}.raw"), &bytes);
            let _ = std::fs::write(
                format!("{path}.dim"),
                format!("{} {}", img.size[0], img.size[1]),
            );
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
