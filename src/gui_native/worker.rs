//! The async side of the GUI: every command the view can send, and the polling
//! that keeps the snapshot fresh.
//!
//! Nothing here paints. The view owns the UI thread and reads
//! [`Shared`](super::model::Shared); this module owns all I/O — the iroh
//! daemons, the IPC sockets, the device registry — and pushes results back
//! through that snapshot.
//!
//! # Shape
//!
//! The receiver loop holds nothing slow. It takes a command, stamps an
//! [`InFlight`] ticket so the click has an answer in the same frame, and spawns
//! the work. Only `Quit`, `Select`, `Cancel`, `Refresh` and `PickFolder` are
//! answered inline — they touch loop state or a detached dialog thread and never
//! a socket. Polling is a second task that runs for the life of the window, so a
//! command never queues behind a poll and a poll never queues behind a command.
//!
//! Two rules keep that safe:
//!
//! * **A gate per directory.** Two commands against one session take the same
//!   [`tokio::sync::Mutex`] and run in order — an unlock must not overtake the
//!   lock it belongs to. Commands against different sessions never share a gate,
//!   so one wedged daemon cannot hold up a healthy folder.
//! * **A ticket that clears itself.** [`Ticket`] removes its entry from
//!   `Shared.inflight` in `Drop`. Success, refusal, transport error, early
//!   return, unwind — there is no exit that can leave a spinner running.
//!
//! # Where a cancel lands
//!
//! [`Cmd::Cancel`] sets `cancelling` on a ticket; the work stops at its next
//! **step boundary** and never between an act and the publish that makes it
//! real. It takes effect:
//!
//! * in `LockWait`, between attempts — nothing has been leased, so stopping
//!   costs nothing;
//! * in `Move`, after the lease and *before* the rename — the lease is released
//!   and the tree is untouched;
//! * in the conflict resolutions, before `ConflictApply`, and again before the
//!   final discard — not discarding a preserved copy loses nothing;
//! * in `Restore`, before the restore request;
//! * in `Dashboard`, between starting the server and resolving its port.
//!
//! It does **not** take effect inside a single round trip (Lock, Unlock, Tag,
//! Pin, ConfigSet, PeerName, Diff, Doctor, Gc, Invite, ConflictDiscard): once a
//! request is on the wire the daemon acts on it and this side will not pretend
//! otherwise. It also does not interrupt an applied-but-unpublished edit, an
//! on-disk rename waiting for its publish, a running `conflicts::prune`, or
//! `Start`/`Stop`/`Init`/`Join`/`Rekey`/`Supervisor`/`Update`, none of which
//! have a midpoint at which stopping would leave less damage than finishing —
//! least of all an update, whose midpoint is a half-replaced binary.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eframe::egui;
use n0_future::join_all;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;

use crate::daemon::DaemonHandle;
use crate::ipc::{self, IpcError, IpcRequest, IpcResponse};
use crate::registry::{Registry, SessionKind};
use crate::state::AppState;

use super::copy;
use super::folderpick;
use super::model::*;
use super::{GUI_SHUTDOWN, REFRESH, absolute, base_name, jstr, short};

/// Rows a single file-query page asks for. Large enough that most sessions need
/// one round trip, small enough that a keystroke is cheap.
const FILE_PAGE: usize = 200;
/// A file query runs while the user types, so it gets a short leash.
const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

// ─── tuning ──────────────────────────────────────────────────────────────────

/// A poll is a background read nobody asked for, so it does not get the IPC
/// default of 30 s. A loopback round trip is sub-millisecond; three seconds is
/// already an unwell daemon, and it caps a stalled session at two refresh beats
/// instead of twenty.
const POLL_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a poll may be outstanding before the session is drawn as slow.
const POLL_SLOW: Duration = Duration::from_millis(750);

/// Between waitlist attempts, matching the CLI's `lock --wait` cadence. The
/// daemon fast-wakes a registered waiter on release, so this is only a ceiling —
/// and it is also how long a cancel can take to land.
const WAIT_RETRY: Duration = Duration::from_secs(2);

/// The dashboard binds within milliseconds; 40 × 50 ms is the same ~2 s budget
/// the CLI gives it before falling back to the configured port.
const DASHBOARD_TRIES: usize = 40;
const DASHBOARD_POLL: Duration = Duration::from_millis(50);

/// Grace for commands still running when the window closes — a Start fired a
/// moment before the close should finish rather than be abandoned. Well inside
/// the window's own `GUI_SHUTDOWN` budget so teardown stays bounded.
const QUIT_DRAIN: Duration = Duration::from_secs(3);

/// Gate key for work that belongs to the device rather than to one session. A
/// NUL byte cannot occur in a path, so this can never collide with a folder.
const DEVICE_GATE: &str = "\0device";

/// Bound on the worker-side backlog. Reached only if the UI stops draining
/// (a frozen or minimised window); dropping the oldest keeps the newest, which
/// is what a user coming back to the window wants to see.
const TOAST_BACKLOG: usize = 32;

// ─── async worker ────────────────────────────────────────────────────────────

pub(super) async fn worker(
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    shared: Arc<Mutex<Shared>>,
    started: Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    ctx: egui::Context,
) {
    let clerk = Clerk {
        shared,
        ctx,
        polling: Arc::new(AtomicBool::new(false)),
        next_id: Arc::new(AtomicU64::new(1)),
    };
    record_running_version(&clerk);
    let gates = Gates::default();
    let selected: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let poke = Arc::new(Notify::new());
    let picking = Arc::new(AtomicBool::new(false));

    let poller = tokio::spawn(refresh_loop(
        clerk.clone(),
        started.clone(),
        selected.clone(),
        poke.clone(),
    ));

    let mut tasks = JoinSet::new();
    while let Some(cmd) = rx.recv().await {
        // Reap settled commands so the set cannot grow across a long session.
        while tasks.try_join_next().is_some() {}
        match cmd {
            Cmd::Quit => break,
            Cmd::Refresh => poke.notify_one(),
            Cmd::Select(dir) => {
                *selected.lock().unwrap_or_else(PoisonError::into_inner) = dir;
                poke.notify_one();
            }
            // A cancel only marks a ticket, so it must not queue behind the
            // command it is trying to stop.
            Cmd::Cancel(id) => {
                clerk.write(|s| mark_cancelling(s, id));
            }
            Cmd::PickFolder(target) => pick_folder(&clerk, &picking, target),
            // A read, driven by typing: it must not queue behind a write on the
            // same folder, and it must not raise a ticket that would flicker on
            // every keystroke.
            Cmd::SearchFiles { .. } => {
                tasks.spawn(search_files(cmd, clerk.clone()));
            }
            cmd => {
                tasks.spawn(run_cmd(
                    cmd,
                    clerk.clone(),
                    gates.clone(),
                    started.clone(),
                    selected.clone(),
                    poke.clone(),
                ));
            }
        }
    }

    poller.abort();
    let _ = poller.await;
    let _ = tokio::time::timeout(QUIT_DRAIN, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
}

/// The portal/native dialog is blocking — run it on a DETACHED std thread, not
/// the runtime's blocking pool: dropping a tokio runtime waits indefinitely for
/// `spawn_blocking` tasks, so a dialog left open would hang process exit. A
/// plain thread dies with the process. One dialog at a time (double-click guard).
fn pick_folder(clerk: &Clerk, picking: &Arc<AtomicBool>, target: PickTarget) {
    if picking
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let clerk = clerk.clone();
    let picking = picking.clone();
    std::thread::spawn(move || {
        match folderpick::pick("Choose a folder to sync") {
            Ok(p) => {
                let picked = p.to_string_lossy().into_owned();
                clerk.write(|s| s.picked = Some((target, picked)));
            }
            // Closing the dialog is not an error.
            Err(folderpick::PickError::Cancelled) => {}
            // Anything else must be said out loud: a dialog that cannot open
            // looks exactly like a broken button.
            Err(folderpick::PickError::NoBackend(why)) => clerk.toast(why, true),
        }
        picking.store(false, Ordering::SeqCst);
    });
}

// ─── writing into the register ───────────────────────────────────────────────

/// The handle every spawned command writes the register with: the snapshot the
/// window reads, the repaint signal, the poll flag `busy` folds in, and the
/// counter that numbers tickets.
#[derive(Clone)]
struct Clerk {
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    polling: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
}

impl Clerk {
    /// Seconds on egui's own clock — the one `ui.input(|i| i.time)` reads — so
    /// the view can age a ticket without knowing anything about this module.
    ///
    /// Never call it while holding `shared`: the UI thread takes the egui lock
    /// first and the snapshot second, and taking them the other way round here
    /// is the deadlock.
    fn now(&self) -> f64 {
        self.ctx.input(|i| i.time)
    }

    /// One short critical section, then a repaint. A poisoned lock is recovered
    /// rather than skipped: `Shared` is plain data with no invariant a panic
    /// could break, and silently dropping the write would lose the toast that
    /// explains what went wrong.
    fn write<R>(&self, f: impl FnOnce(&mut Shared) -> R) -> R {
        let out = {
            let mut s = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
            let r = f(&mut s);
            let polling = self.polling.load(Ordering::Relaxed);
            recompute_busy(&mut s, polling);
            r
        };
        self.ctx.request_repaint();
        out
    }

    fn read<R>(&self, f: impl FnOnce(&Shared) -> R) -> R {
        let s = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        f(&s)
    }

    fn toast(&self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.write(|s| {
            s.toasts.push(Toast { text, error });
            let excess = s.toasts.len().saturating_sub(TOAST_BACKLOG);
            s.toasts.drain(..excess);
        });
    }

    /// An answer too long, and too worth reading, to be a toast.
    fn report(&self, title: impl Into<String>, body: impl Into<String>, failed: bool) {
        let r = Report {
            title: title.into(),
            body: body.into(),
            link: None,
            failed,
        };
        self.write(|s| s.report = Some(r));
    }

    fn report_link(&self, title: impl Into<String>, body: impl Into<String>, link: String) {
        let r = Report {
            title: title.into(),
            body: body.into(),
            link: Some(link),
            failed: false,
        };
        self.write(|s| s.report = Some(r));
    }

    fn refusal(&self, refusal: Option<Refusal>) {
        self.write(|s| s.refusal = refusal);
    }

    fn set_polling(&self, on: bool) {
        self.polling.store(on, Ordering::Relaxed);
        self.write(|_: &mut Shared| ());
    }

    /// Accept a command: give it a number, put it on screen, and hand back the
    /// ticket that will take it off again.
    fn open(&self, dir: &str, what: &'static str, subject: impl Into<String>) -> Ticket {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let started = self.now();
        let dir = dir.to_string();
        let subject = subject.into();
        self.write(|s| open_inflight(s, id, dir, what, subject, started));
        Ticket {
            clerk: self.clone(),
            id,
        }
    }
}

/// An accepted command's entry in `Shared.inflight`, removed on drop.
struct Ticket {
    clerk: Clerk,
    id: u64,
}

impl Ticket {
    /// Which round trip of a guided sequence is running, so a failure at step
    /// three is reported as step three rather than as the whole thing.
    fn step(&self, n: u8, of: u8) {
        self.clerk.write(|s| set_step(s, self.id, (n, of)));
    }

    /// True once the user has asked to abandon this command. Read only at a
    /// step boundary — see the module docs for where those are.
    fn cancelled(&self) -> bool {
        self.clerk.read(|s| is_cancelling(s, self.id))
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.clerk.write(|s| close_inflight(s, self.id));
    }
}

// ─── in-flight bookkeeping (pure over the snapshot) ──────────────────────────

/// `busy` means the window has something outstanding: a command the user
/// started, or the background poll.
fn recompute_busy(s: &mut Shared, polling: bool) {
    s.busy = polling || !s.inflight.is_empty();
}

fn open_inflight(
    s: &mut Shared,
    id: u64,
    dir: String,
    what: &'static str,
    subject: String,
    started: f64,
) {
    s.inflight.push(InFlight {
        id,
        dir,
        what,
        subject,
        step: None,
        started,
        cancelling: false,
    });
}

/// Whether a ticket was actually removed. `Drop` calls this exactly once per
/// id, so a `false` would mean the ticket had already been cleared elsewhere.
fn close_inflight(s: &mut Shared, id: u64) -> bool {
    let before = s.inflight.len();
    s.inflight.retain(|f| f.id != id);
    s.inflight.len() != before
}

fn set_step(s: &mut Shared, id: u64, step: (u8, u8)) -> bool {
    match s.inflight.iter_mut().find(|f| f.id == id) {
        Some(f) => {
            f.step = Some(step);
            true
        }
        None => false,
    }
}

/// `false` when the id names nothing — a command that settled before the user's
/// click landed, which needs no answer.
fn mark_cancelling(s: &mut Shared, id: u64) -> bool {
    match s.inflight.iter_mut().find(|f| f.id == id) {
        Some(f) => {
            f.cancelling = true;
            true
        }
        None => false,
    }
}

fn is_cancelling(s: &Shared, id: u64) -> bool {
    s.inflight.iter().any(|f| f.id == id && f.cancelling)
}

// ─── per-session serialisation ───────────────────────────────────────────────

/// One mutex per session folder. Two commands against the same folder take the
/// same gate and run in order; commands against different folders never meet.
#[derive(Clone, Default)]
struct Gates(Arc<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>);

impl Gates {
    fn get(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        // A gate no task owns is reachable only through this map, so a strong
        // count of one proves it is idle and safe to forget. Anything queued on
        // or held has a second owner and is kept — which is what stops two
        // commands on one folder from ever ending up on different mutexes.
        map.retain(|_, gate| Arc::strong_count(gate) > 1);
        map.entry(key.to_string()).or_default().clone()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

// ─── dispatch ────────────────────────────────────────────────────────────────

/// What a dispatched command is worth to the register: the gate that serialises
/// it, the session it belongs to, and the words the window shows while it runs.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    gate: String,
    dir: String,
    what: &'static str,
    subject: String,
}

impl Entry {
    fn session(dir: String, what: &'static str, subject: impl Into<String>) -> Self {
        Self {
            gate: dir.clone(),
            dir,
            what,
            subject: subject.into(),
        }
    }

    /// Device-wide work: no session to file it under, and its own gate so it
    /// neither blocks nor is blocked by any folder.
    fn device(what: &'static str, subject: impl Into<String>) -> Self {
        Self {
            gate: DEVICE_GATE.to_string(),
            dir: String::new(),
            what,
            subject: subject.into(),
        }
    }
}

/// Pure routing: which gate, which session, which verb. `None` for the commands
/// the loop answers inline, which never reach a task.
fn entry_for(cmd: &Cmd) -> Option<Entry> {
    let key = |dir: &PathBuf| dir.to_string_lossy().to_string();
    let folder = |dir: &PathBuf| base_name(&dir.to_string_lossy());
    Some(match cmd {
        Cmd::Quit
        | Cmd::Refresh
        | Cmd::Select(_)
        | Cmd::Cancel(_)
        | Cmd::PickFolder(_)
        | Cmd::SearchFiles { .. } => {
            return None;
        }
        Cmd::Lock { dir, path } => Entry::session(key(dir), "locking", path),
        Cmd::Unlock { dir, path } => Entry::session(key(dir), "publishing", path),
        Cmd::LockWait { dir, path } => Entry::session(key(dir), "awaiting", path),
        Cmd::ConfigSet { dir, key: k, .. } => Entry::session(key(dir), "saving", k),
        Cmd::ResolveMine { dir, target, .. } | Cmd::ResolveBoth { dir, target, .. } => {
            Entry::session(key(dir), "resolving", target)
        }
        Cmd::Restore { dir, path, n } => {
            Entry::session(key(dir), "restoring", format!("{path} · version {n}"))
        }
        Cmd::Tag { dir, path, n, .. } => {
            Entry::session(key(dir), "tagging", format!("{path} · version {n}"))
        }
        Cmd::Pin {
            dir,
            path,
            n,
            pinned,
        } => Entry::session(
            key(dir),
            if *pinned { "pinning" } else { "unpinning" },
            format!("{path} · version {n}"),
        ),
        Cmd::ConflictDiscard { dir, id } => Entry::session(key(dir), "discarding", id),
        Cmd::PeerName { dir, id, .. } => Entry::session(key(dir), "naming", short(id)),
        Cmd::Start(dir) => Entry::session(key(dir), "starting", folder(dir)),
        Cmd::Stop(dir) => Entry::session(key(dir), "stopping", folder(dir)),
        Cmd::Pause(dir) => Entry::session(key(dir), "pausing", folder(dir)),
        Cmd::Resume(dir) => Entry::session(key(dir), "resuming", folder(dir)),
        Cmd::Init(dir) => Entry::session(key(dir), "creating", folder(dir)),
        Cmd::Join(dir, _) => Entry::session(key(dir), "joining", folder(dir)),
        Cmd::Move { dir, from, to } => {
            Entry::session(key(dir), "renaming", format!("{from} → {to}"))
        }
        Cmd::Diff { dir, path, n } => {
            Entry::session(key(dir), "comparing", format!("{path} · version {n}"))
        }
        Cmd::Doctor { dir } => Entry::session(key(dir), "checking", folder(dir)),
        Cmd::Dashboard { dir } => Entry::session(key(dir), "opening", folder(dir)),
        Cmd::Gc { dir } => Entry::session(key(dir), "collecting", folder(dir)),
        Cmd::PruneConflicts { dir, names, .. } => Entry::session(
            key(dir),
            "pruning",
            format!(
                "{} preserved cop{}",
                names.len(),
                if names.len() == 1 { "y" } else { "ies" }
            ),
        ),
        Cmd::Invite { dir, role, .. } => Entry::session(
            key(dir),
            "minting",
            format!("{} invite", role.as_deref().unwrap_or("editor")),
        ),
        Cmd::Rekey { dir } => Entry::session(key(dir), "rekeying", folder(dir)),
        Cmd::Supervisor { install } => Entry::device(
            if *install { "installing" } else { "removing" },
            "the supervisor",
        ),
        // The binary belongs to the device, not to a folder: an update must not
        // wait behind a wedged session, and the two halves share the device gate
        // so a check can never run alongside an install of the same binary.
        Cmd::Update { apply } => Entry::device(
            if *apply { "installing" } else { "checking" },
            if *apply {
                "the new version"
            } else {
                "for a newer release"
            },
        ),
    })
}

async fn run_cmd(
    cmd: Cmd,
    clerk: Clerk,
    gates: Gates,
    started: Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    selected: Arc<Mutex<Option<PathBuf>>>,
    poke: Arc<Notify>,
) {
    // Quit, Refresh, Select, Cancel and PickFolder are answered in the loop and
    // are never spawned, so there is no work here for a command without an entry.
    let Some(entry) = entry_for(&cmd) else {
        return;
    };
    // The ticket is taken before the gate, so a command queued behind another is
    // visible as queued rather than as a click that did nothing.
    let ticket = clerk.open(&entry.dir, entry.what, entry.subject);
    let gate = gates.get(&entry.gate);

    match cmd {
        // The waitlist is the one command that may run for ten minutes, so it
        // takes the gate per attempt instead of holding it throughout —
        // otherwise waiting on one file would freeze its whole session.
        Cmd::LockWait { dir, path } => lock_wait(&clerk, &ticket, &gate, &dir, &path).await,
        cmd => {
            let _permit = gate.lock().await;
            perform(cmd, &clerk, &ticket, &started, &selected).await;
        }
    }

    drop(ticket);
    poke.notify_one();
}

async fn perform(
    cmd: Cmd,
    clerk: &Clerk,
    ticket: &Ticket,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    selected: &Arc<Mutex<Option<PathBuf>>>,
) {
    match cmd {
        // Already answered before this function is reached: the six handled in
        // the receiver loop, and `LockWait` in `run_cmd` (it takes its folder's
        // gate per attempt instead of holding it for the whole wait). They are
        // spelled out rather than caught by a wildcard so a new command is a
        // compile error here instead of a button that silently does nothing.
        Cmd::Quit
        | Cmd::Refresh
        | Cmd::Select(_)
        | Cmd::Cancel(_)
        | Cmd::PickFolder(_)
        | Cmd::SearchFiles { .. }
        | Cmd::LockWait { .. } => {}
        Cmd::Lock { dir, path } => {
            let p = path.clone();
            edit_action(
                clerk,
                &dir,
                IpcRequest::Lock { path },
                &p,
                copy::TOAST_LOCKED,
                "lock refused",
            )
            .await
        }
        Cmd::Unlock { dir, path } => {
            let p = path.clone();
            edit_action(
                clerk,
                &dir,
                IpcRequest::Unlock { path },
                &p,
                copy::TOAST_UNLOCKED,
                "unlock failed",
            )
            .await
        }
        Cmd::ConfigSet { dir, key, value } => {
            ipc_action(
                clerk,
                &dir,
                IpcRequest::ConfigSet { key, value },
                "setting saved",
                "could not set",
            )
            .await
        }
        Cmd::ResolveMine { dir, id, target } => {
            resolve_conflict(clerk, ticket, &dir, &id, &target, "keep-mine", true).await
        }
        Cmd::ResolveBoth { dir, id, target } => {
            resolve_conflict(clerk, ticket, &dir, &id, &target, "keep-both", false).await
        }
        Cmd::Restore { dir, path, n } => restore_guided(clerk, ticket, &dir, &path, n).await,
        Cmd::Tag { dir, path, n, name } => {
            ipc_action(
                clerk,
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
                clerk,
                &dir,
                IpcRequest::Pin { path, n, pinned },
                if pinned { "pinned" } else { "unpinned" },
                "pin failed",
            )
            .await
        }
        Cmd::ConflictDiscard { dir, id } => {
            ipc_action(
                clerk,
                &dir,
                IpcRequest::ConflictDiscard { id },
                "discarded",
                "discard failed",
            )
            .await
        }
        Cmd::PeerName { dir, id, name } => {
            ipc_action(
                clerk,
                &dir,
                IpcRequest::PeerName { id, name },
                "peer name saved",
                "could not name peer",
            )
            .await
        }
        Cmd::Start(dir) => start_session(clerk, started, &dir).await,
        Cmd::Stop(dir) => stop_session(clerk, started, &dir).await,
        Cmd::Pause(dir) => set_paused(clerk, &dir, true).await,
        Cmd::Resume(dir) => set_paused(clerk, &dir, false).await,
        Cmd::Init(dir) => {
            // A directory scan plus key generation: blocking work, and it does
            // not belong on the reactor thread.
            let d = dir.clone();
            match blocking(move || crate::cli::init(&d)).await {
                Ok(Ok(())) => {
                    select(selected, absolute(&dir));
                    clerk.toast(format!("created {}", dir.display()), false);
                }
                Ok(Err(e)) => clerk.toast(format!("init failed: {e}"), true),
                Err(e) => clerk.toast(format!("init failed: {e}"), true),
            }
        }
        Cmd::Join(dir, ticket_str) => {
            let d = dir.clone();
            let t = ticket_str.trim().to_string();
            match blocking(move || crate::cli::join(&d, &t)).await {
                Ok(Ok(())) => {
                    select(selected, absolute(&dir));
                    clerk.toast(format!("joined into {}", dir.display()), false);
                }
                Ok(Err(e)) => clerk.toast(format!("join failed: {e}"), true),
                Err(e) => clerk.toast(format!("join failed: {e}"), true),
            }
        }
        Cmd::Move { dir, from, to } => move_guided(clerk, ticket, &dir, &from, &to).await,
        Cmd::Diff { dir, path, n } => diff_action(clerk, &dir, &path, n).await,
        Cmd::Doctor { dir } => doctor_action(clerk, &dir).await,
        Cmd::Dashboard { dir } => dashboard_action(clerk, ticket, &dir).await,
        Cmd::Gc { dir } => gc_action(clerk, &dir).await,
        Cmd::PruneConflicts {
            dir,
            older_than_ms,
            names,
        } => prune_action(clerk, &dir, older_than_ms, names).await,
        Cmd::Invite { dir, role, ttl_ms } => invite_action(clerk, &dir, role, ttl_ms).await,
        Cmd::Rekey { dir } => rekey_action(clerk, &dir).await,
        Cmd::Supervisor { install } => supervisor_action(clerk, install).await,
        Cmd::Update { apply } => update_action(clerk, selected, apply).await,
    }
}

fn select(selected: &Arc<Mutex<Option<PathBuf>>>, dir: PathBuf) {
    *selected.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
}

/// Runs synchronous work off the reactor. A `JoinError` means the closure
/// panicked, which is a bug rather than a refusal — it is reported, not hidden
/// behind a result that looks like the operation simply did not happen.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("a background task failed: {e}"))
}

// ─── edits ───────────────────────────────────────────────────────────────────

/// The daemon diagnoses every refusal — which of the three preconditions
/// failed, what would clear it, who holds the lease, which peers it asked. Keep
/// all of it; a one-line toast is where that diagnosis used to go to die.
fn diagnose(path: &str, r: &IpcResponse, fallback: &str) -> Refusal {
    let diag = r.data.as_ref().map(|d| &d["diagnosis"]);
    Refusal {
        path: path.to_string(),
        precondition: diag
            .and_then(|d| d["precondition"].as_str())
            .unwrap_or_default()
            .to_string(),
        message: r
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| fallback.to_string()),
        hint: diag
            .and_then(|d| d["hint"].as_str())
            .unwrap_or_default()
            .to_string(),
        held_by: diag.and_then(|d| d["held_by"].as_str()).map(short),
        peers: diag
            .and_then(|d| d["peers"].as_array())
            .map(|ps| {
                ps.iter()
                    .map(|p| {
                        let id = short(&jstr(p, "id"));
                        let conn = jstr(p, "conn");
                        let grade = jstr(p, "grade");
                        match (conn.is_empty(), grade.is_empty()) {
                            (true, true) => id,
                            _ => format!("{id} · {grade} · {conn}"),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Sends an edit request and keeps the daemon's whole diagnosis when it is
/// refused, so the view can name the precondition and its remedy.
async fn edit_action(
    clerk: &Clerk,
    dir: &Path,
    req: IpcRequest,
    path: &str,
    ok_msg: &str,
    err_prefix: &str,
) {
    match ipc::request(dir, &req).await {
        Ok(r) if r.ok => {
            clerk.refusal(None);
            clerk.toast(ok_msg, false);
        }
        Ok(r) => {
            let refusal = diagnose(path, &r, err_prefix);
            let message = refusal.message.clone();
            clerk.refusal(Some(refusal));
            clerk.toast(format!("{err_prefix}: {message}"), true);
        }
        Err(e) => clerk.toast(format!("{err_prefix}: {e}"), true),
    }
}

async fn ipc_action(clerk: &Clerk, dir: &Path, req: IpcRequest, ok_msg: &str, err_prefix: &str) {
    match ipc::request(dir, &req).await {
        Ok(r) if r.ok => clerk.toast(ok_msg, false),
        Ok(r) => {
            let msg = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| err_prefix.to_string());
            clerk.toast(format!("{err_prefix}: {msg}"), true);
        }
        Err(e) => clerk.toast(format!("{err_prefix}: {e}"), true),
    }
}

/// The daemon's one remedy for a LEASE refusal: register interest so the holder
/// is told and this node is fast-woken on release, then re-attempt the whole
/// acquire (all three preconditions re-checked each round) until the path frees
/// or the session's wait-timeout runs out.
///
/// The gate is taken per attempt and released across the sleep. Holding it for
/// the whole wait would be correct and useless — it would freeze every other
/// command on that session for up to ten minutes, which is the freeze this
/// module exists to remove.
async fn lock_wait(
    clerk: &Clerk,
    ticket: &Ticket,
    gate: &Arc<tokio::sync::Mutex<()>>,
    dir: &Path,
    path: &str,
) {
    let d = dir.to_path_buf();
    let wait = match blocking(move || AppState::load(&d).map(|st| st.config.wait_timeout())).await {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => {
            clerk.toast(format!("cannot wait for {path}: {e}"), true);
            return;
        }
        Err(e) => {
            clerk.toast(format!("cannot wait for {path}: {e}"), true);
            return;
        }
    };
    let deadline = tokio::time::Instant::now() + wait;
    let mut announced = false;

    loop {
        // Step boundary: nothing is leased here, so abandoning costs nothing.
        if ticket.cancelled() {
            clerk.toast(
                format!("stopped waiting for {path} — no lease was taken"),
                false,
            );
            return;
        }
        let attempt = {
            let _permit = gate.lock().await;
            ipc::request(
                dir,
                &IpcRequest::Lock {
                    path: path.to_string(),
                },
            )
            .await
        };
        let r = match attempt {
            Ok(r) => r,
            Err(e) => {
                clerk.toast(format!("waiting for {path} failed: {e}"), true);
                return;
            }
        };
        if r.ok {
            clerk.refusal(None);
            clerk.toast(copy::TOAST_LOCKED, false);
            return;
        }
        let held = r.error.as_ref().is_some_and(|e| e.code == "lease_held");
        if !held || tokio::time::Instant::now() >= deadline {
            let refusal = diagnose(path, &r, "lock refused");
            let message = refusal.message.clone();
            let gave_up = held;
            clerk.refusal(Some(refusal));
            if gave_up {
                clerk.toast(
                    format!("gave up waiting for {path} after the wait-timeout: {message}"),
                    true,
                );
            } else {
                clerk.toast(format!("lock refused: {message}"), true);
            }
            return;
        }
        if !announced {
            let _permit = gate.lock().await;
            // Registering interest is advisory: the retry above re-checks every
            // precondition anyway, so a failure here only costs the fast wake.
            if let Err(e) = ipc::request(
                dir,
                &IpcRequest::LockWait {
                    path: path.to_string(),
                },
            )
            .await
            {
                clerk.toast(
                    format!("could not join the waitlist for {path} ({e}) — still retrying"),
                    true,
                );
            }
            drop(_permit);
            let by = r
                .data
                .as_ref()
                .and_then(|d| d["diagnosis"]["held_by"].as_str())
                .map(short)
                .unwrap_or_else(|| "another peer".into());
            clerk.toast(
                format!("{path} is held by {by} — waiting; it locks itself the moment it frees"),
                false,
            );
            announced = true;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(left.min(WAIT_RETRY)).await;
    }
}

/// keep-mine: the guided lock → apply → unlock → discard sequence. The
/// quarantined copy is discarded ONLY after the apply AND its publish succeed, so
/// a failure at any step leaves the preserved copy untouched (Golden Invariant).
/// The daemon's `ConflictApply` refuses without a self-held lease, hence the
/// explicit lock/unlock around it — exactly the CLI's `resolve --keep mine` path.
async fn resolve_conflict(
    clerk: &Clerk,
    ticket: &Ticket,
    dir: &Path,
    id: &str,
    target: &str,
    verb: &str,
    discard: bool,
) {
    ticket.step(1, 4);
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
            let refusal = diagnose(target, &r, "lock refused");
            let m = refusal.message.clone();
            clerk.refusal(Some(refusal));
            clerk.toast(
                format!("{verb}: lock refused ({m}). The copy is untouched."),
                true,
            );
            return;
        }
        Err(e) => {
            clerk.toast(
                format!("{verb}: lock failed ({e}). The copy is untouched."),
                true,
            );
            return;
        }
    }
    // Step boundary: the lease is held but nothing has been written, so the
    // whole sequence can be abandoned by releasing it.
    if ticket.cancelled() {
        let held = !unlock_ok(dir, target).await;
        clerk.toast(
            format!(
                "{verb}: stopped before anything was applied. The copy is untouched.{}",
                lease_note(held, target)
            ),
            false,
        );
        return;
    }
    ticket.step(2, 4);
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
            clerk.toast(
                format!(
                    "{verb}: apply failed ({m}). The copy is untouched.{}",
                    lease_note(held, target)
                ),
                true,
            );
            return;
        }
        Err(e) => {
            let held = !unlock_ok(dir, target).await;
            clerk.toast(
                format!(
                    "{verb}: apply failed ({e}). The copy is untouched.{}",
                    lease_note(held, target)
                ),
                true,
            );
            return;
        }
    }
    // No cancel between here and the publish: the bytes are already on disk and
    // stopping now would leave them unpublished under a held lease.
    ticket.step(3, 4);
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
            clerk.toast(
                format!(
                    "{verb}: publish failed ({m}). Bytes applied but not published and the lease is still held — retry unlock from Files. The copy is untouched."
                ),
                true,
            );
            return;
        }
        Err(e) => {
            clerk.toast(
                format!(
                    "{verb}: publish failed ({e}). The lease is still held — retry unlock from Files. The copy is untouched."
                ),
                true,
            );
            return;
        }
    }
    // 4/4 discard the (now-superseded) quarantined copy — only when the
    // resolution the user chose actually supersedes it. `keep both` restores
    // the copy under a new name and deletes nothing, which is the one
    // resolution that cannot lose bytes.
    if !discard {
        clerk.toast(format!("restored the preserved copy as {target}"), false);
        return;
    }
    // Step boundary: the resolution has landed. Stopping here keeps the copy in
    // quarantine, which loses nothing.
    if ticket.cancelled() {
        clerk.toast(
            format!("resolved into {target}; the preserved copy was kept in quarantine"),
            false,
        );
        return;
    }
    ticket.step(4, 4);
    match ipc::request(dir, &IpcRequest::ConflictDiscard { id: id.to_string() }).await {
        Ok(r) if r.ok => clerk.toast(format!("resolved into {target}"), false),
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "discard failed".into());
            clerk.toast(
                format!(
                    "{verb}: published, but the copy discard failed ({m}) — it is still in quarantine."
                ),
                true,
            );
        }
        Err(e) => clerk.toast(
            format!(
                "{verb}: published, but the copy discard failed ({e}) — it is still in quarantine."
            ),
            true,
        ),
    }
}

/// The sentence appended when a rollback could not get the lease back.
fn lease_note(still_held: bool, path: &str) -> String {
    if still_held {
        format!(" The lease on {path} may still be held — release it from Files.")
    } else {
        String::new()
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
async fn restore_guided(clerk: &Clerk, ticket: &Ticket, dir: &Path, path: &str, n: usize) {
    ticket.step(1, 3);
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
            let refusal = diagnose(path, &r, "lock refused");
            let m = refusal.message.clone();
            clerk.refusal(Some(refusal));
            clerk.toast(
                format!("restore: lock refused ({m}). Nothing changed."),
                true,
            );
            return;
        }
        Err(e) => {
            clerk.toast(
                format!("restore: lock failed ({e}). Nothing changed."),
                true,
            );
            return;
        }
    }
    // Step boundary: leased, nothing written.
    if ticket.cancelled() {
        let held = !unlock_ok(dir, path).await;
        clerk.toast(
            format!(
                "restore stopped before anything changed.{}",
                lease_note(held, path)
            ),
            false,
        );
        return;
    }
    ticket.step(2, 3);
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
            clerk.toast(
                format!(
                    "restore failed ({m}). Nothing changed.{}",
                    lease_note(held, path)
                ),
                true,
            );
            return;
        }
        Err(e) => {
            let held = !unlock_ok(dir, path).await;
            clerk.toast(
                format!(
                    "restore failed ({e}). Nothing changed.{}",
                    lease_note(held, path)
                ),
                true,
            );
            return;
        }
    }
    // No cancel from here: the bytes are restored and must be published.
    ticket.step(3, 3);
    match ipc::request(
        dir,
        &IpcRequest::Unlock {
            path: path.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => clerk.toast(
            format!("restored version {n} of {path} (previous content kept in history)"),
            false,
        ),
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "publish failed".into());
            clerk.toast(
                format!(
                    "restored, but publish failed ({m}) — the lease is still held; retry unlock from Files."
                ),
                true,
            );
        }
        Err(e) => clerk.toast(
            format!(
                "restored, but publish failed ({e}) — the lease is still held; retry unlock from Files."
            ),
            true,
        ),
    }
}

/// What (if anything) is wrong with renaming `from` to `to`, given the on-disk
/// facts. Pure, so the refusal rules are tested without a daemon or a folder.
fn check_move(
    from: &str,
    to: &str,
    from_exists: bool,
    from_is_file: bool,
    to_exists: bool,
) -> Result<(), String> {
    if to.trim().is_empty() {
        return Err("the destination name is empty".into());
    }
    if from == to {
        return Err("the source and destination names are the same".into());
    }
    if !from_exists {
        return Err(format!("{from} does not exist"));
    }
    if !from_is_file {
        return Err(format!("{from} is not a file — only files can be renamed"));
    }
    if to_exists {
        return Err(format!(
            "{to} already exists — pick a name that does not, or remove it first"
        ));
    }
    Ok(())
}

/// Guided rename: lease the old name, rename on disk, publish the new name,
/// then publish the removal of the old one. Doing it under a lease is what makes
/// the delete propagate — a bare rename's delete-half is reverted by design — so
/// this is the only rename the GUI can offer. Mirrors `tazamun mv`.
///
/// Golden Invariant: exactly one step touches the user's bytes, and it is a
/// rename inside the same folder — nothing is copied, nothing is deleted. Every
/// failure before it leaves the tree as it was; every failure after it names
/// what is on disk and what is not yet published.
async fn move_guided(clerk: &Clerk, ticket: &Ticket, dir: &Path, from: &str, to: &str) {
    ticket.step(1, 4);
    let from_abs = dir.join(from);
    let to_abs = dir.join(to);
    let facts = {
        let (f, t) = (from_abs.clone(), to_abs.clone());
        blocking(move || (f.exists(), f.is_file(), t.exists())).await
    };
    let (from_exists, from_is_file, to_exists) = match facts {
        Ok(v) => v,
        Err(e) => {
            clerk.toast(format!("cannot rename {from}: {e}"), true);
            return;
        }
    };
    if let Err(why) = check_move(from, to, from_exists, from_is_file, to_exists) {
        clerk.toast(format!("cannot rename {from}: {why}"), true);
        return;
    }
    // Lease the old name so its removal is published rather than reverted.
    match ipc::request(
        dir,
        &IpcRequest::Lock {
            path: from.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => {}
        Ok(r) => {
            let refusal = diagnose(from, &r, "lock refused");
            let m = refusal.message.clone();
            clerk.refusal(Some(refusal));
            clerk.toast(format!("rename refused ({m}). Nothing was renamed."), true);
            return;
        }
        Err(e) => {
            clerk.toast(format!("rename failed ({e}). Nothing was renamed."), true);
            return;
        }
    }
    // Step boundary, and the last one: after the rename below the peers must be
    // told, so there is no safe place to stop until step 4 has run.
    if ticket.cancelled() {
        let held = !unlock_ok(dir, from).await;
        clerk.toast(
            format!(
                "rename stopped before anything moved.{}",
                lease_note(held, from)
            ),
            false,
        );
        return;
    }
    ticket.step(2, 4);
    let renamed = {
        let (f, t) = (from_abs, to_abs);
        blocking(move || std::fs::rename(&f, &t).map_err(|e| e.to_string())).await
    };
    match renamed {
        Ok(Ok(())) => {}
        Ok(Err(e)) | Err(e) => {
            let held = !unlock_ok(dir, from).await;
            clerk.toast(
                format!(
                    "could not rename on disk ({e}). {from} is unchanged.{}",
                    lease_note(held, from)
                ),
                true,
            );
            return;
        }
    }
    // Publish the new name. Best-effort: even if it cannot be leased right now
    // the file exists locally and will publish later. The removal of the old
    // name is the part that must land, so step 4 is not best-effort.
    ticket.step(3, 4);
    let published_new = matches!(
        ipc::request(dir, &IpcRequest::Lock { path: to.to_string() }).await,
        Ok(r) if r.ok
    ) && unlock_ok(dir, to).await;
    ticket.step(4, 4);
    match ipc::request(
        dir,
        &IpcRequest::Unlock {
            path: from.to_string(),
        },
    )
    .await
    {
        Ok(r) if r.ok => {
            let note = if published_new {
                String::new()
            } else {
                format!(" {to} is on disk but not published yet — it will go out on the next sync.")
            };
            clerk.toast(format!("renamed {from} → {to}.{note}"), false);
        }
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "publish failed".into());
            clerk.toast(
                format!(
                    "renamed to {to} on disk, but publishing the removal of {from} failed ({m}) — the lease is still held and peers still see {from}; retry unlock from Files."
                ),
                true,
            );
        }
        Err(e) => clerk.toast(
            format!(
                "renamed to {to} on disk, but publishing the removal of {from} failed ({e}) — the lease is still held and peers still see {from}; retry unlock from Files."
            ),
            true,
        ),
    }
}

// ─── answers that are too long for a toast ───────────────────────────────────

async fn diff_action(clerk: &Clerk, dir: &Path, path: &str, n: usize) {
    match ipc::request(
        dir,
        &IpcRequest::Diff {
            path: path.to_string(),
            n,
        },
    )
    .await
    {
        Ok(r) if r.ok => {
            let data = r.data.unwrap_or_default();
            clerk.report(
                format!("{path} · current ⟵ version {n}"),
                diff_report(&data),
                false,
            );
        }
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "diff failed".into());
            clerk.report(format!("{path} · version {n}"), m, true);
        }
        Err(e) => clerk.report(format!("{path} · version {n}"), e.to_string(), true),
    }
}

/// The chunk-level answer the daemon computes, in the CLI's own words.
fn diff_report(d: &serde_json::Value) -> String {
    let tag = d["version_tag"]
        .as_str()
        .map(|t| format!("version tag     : «{t}»\n"))
        .unwrap_or_default();
    if d["identical_content"].as_bool() == Some(true) {
        return format!("{tag}identical content — nothing changed.");
    }
    let get = |k: &str| d[k].as_u64().unwrap_or(0);
    let pct = d["changed_pct"].as_f64().unwrap_or(0.0);
    format!(
        "{tag}content changed : {pct:.1}%  ({} would transfer to a peer holding the old version)\n\
         chunks          : {} → {}  (identical {}, added {}, removed {}, moved {})\n\
         size            : {} → {}",
        crate::state::fmt_size(get("transfer_bytes")),
        get("old_chunks"),
        get("new_chunks"),
        get("identical"),
        get("added"),
        get("removed"),
        get("moved"),
        crate::state::fmt_size(get("old_bytes")),
        crate::state::fmt_size(get("new_bytes")),
    )
}

async fn doctor_action(clerk: &Clerk, dir: &Path) {
    match ipc::request(dir, &IpcRequest::Doctor).await {
        Ok(r) if r.ok => {
            let data = r.data.unwrap_or_default();
            clerk.report("doctor", doctor_report(&data), false);
        }
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "the daemon refused the check".into());
            clerk.report("doctor", m, true);
        }
        Err(e) => clerk.report(
            "doctor",
            format!(
                "{e}\n\nThis check reads the running daemon's own view of the network, so it \
                 needs the session started."
            ),
            true,
        ),
    }
}

/// The daemon's half of `tazamun doctor`: identity, bound sockets, relay policy
/// and per-peer connectivity. The filesystem and quarantine sections of the CLI
/// report are local probes and are not part of this answer.
fn doctor_report(d: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "peer id            : {}", jstr(d, "id"));
    let _ = writeln!(
        out,
        "mode               : {}",
        match d["mode"].as_str() {
            Some("airgap") => "AIRGAP (no relays, no DNS discovery, LAN mDNS only)",
            _ => "normal",
        }
    );
    let _ = writeln!(out, "relay policy       : {}", jstr(d, "relay_policy"));
    let _ = writeln!(
        out,
        "home relay         : {}",
        d["home_relay"].as_str().unwrap_or("(none)")
    );
    for r in d["relay_status"].as_array().unwrap_or(&Vec::new()) {
        let _ = writeln!(
            out,
            "relay handshake    : {} — {}",
            jstr(r, "url"),
            if r["connected"].as_bool() == Some(true) {
                "connected"
            } else {
                "NOT connected"
            }
        );
    }
    let _ = writeln!(
        out,
        "LAN discovery      : {}",
        if d["lan_discovery"].as_bool() == Some(true) {
            "on"
        } else {
            "off"
        }
    );
    let socks = d["bound_sockets"].as_array().cloned().unwrap_or_default();
    if socks.is_empty() {
        let _ = writeln!(out, "bound sockets      : (none reported)");
    }
    for s in socks {
        let _ = writeln!(out, "bound socket       : {}", s.as_str().unwrap_or("-"));
    }
    let _ = writeln!(
        out,
        "members            : {} known, {} connected",
        d["known_members"].as_u64().unwrap_or(0),
        d["connected_peers"].as_u64().unwrap_or(0)
    );
    if let Some(n) = d["unapplied_count"].as_u64().filter(|n| *n > 0) {
        let _ = writeln!(
            out,
            "held back          : {n} remote file(s) whose names cannot exist on this filesystem"
        );
    }
    let peers = d["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        let _ = writeln!(out, "\nno connected peers — nothing to hole-punch yet");
    } else {
        let _ = writeln!(out, "\nconnectivity:");
    }
    let mut relayed = false;
    for p in &peers {
        let conn = p["conn"].as_str().unwrap_or("None");
        relayed |= conn == "Relayed";
        let ttd = p["time_to_direct_ms"]
            .as_u64()
            .map(|ms| format!(", direct in {ms}ms"))
            .unwrap_or_else(|| {
                if conn == "Relayed" {
                    ", still relayed".to_string()
                } else {
                    String::new()
                }
            });
        let lan = if p["via_lan"].as_bool() == Some(true) {
            " via LAN"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {}  {conn}{lan} ({}, {:.0}ms{ttd})",
            short(&jstr(p, "id")),
            p["grade"].as_str().unwrap_or("Offline"),
            p["rtt_ms"].as_f64().unwrap_or(0.0),
        );
    }
    if relayed {
        let _ = writeln!(
            out,
            "\na peer is reachable only via relay — direct hole-punching has not succeeded. \
             Run this check on both ends and look at NAT and firewall rules."
        );
    }
    out
}

/// Start the loopback dashboard on demand, then wait for the port it actually
/// bound. The token rides the URL fragment, so it never reaches the server in a
/// request — which is why the address is handed over as a report to read rather
/// than as a line in a log.
async fn dashboard_action(clerk: &Clerk, ticket: &Ticket, dir: &Path) {
    ticket.step(1, 2);
    let started = match ipc::request(dir, &IpcRequest::DashboardStart).await {
        Ok(r) if r.ok => r.data.unwrap_or_default(),
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "the daemon refused to start the dashboard".into());
            clerk.report("dashboard", m, true);
            return;
        }
        Err(e) => {
            clerk.report("dashboard", e.to_string(), true);
            return;
        }
    };
    let token = jstr(&started, "token");
    let mut port = started["port"]
        .as_u64()
        .unwrap_or(u64::from(crate::consts::DASHBOARD_PORT)) as u16;
    // Step boundary: the server is bound (idempotently) and stopping here leaves
    // nothing half-done — the next open reuses it.
    if ticket.cancelled() {
        clerk.toast("stopped waiting for the dashboard address", false);
        return;
    }
    ticket.step(2, 2);
    for _ in 0..DASHBOARD_TRIES {
        match ipc::request(dir, &IpcRequest::DashboardInfo).await {
            Ok(r) if r.ok => {
                let bound = r
                    .data
                    .as_ref()
                    .and_then(|d| d["port"].as_u64())
                    .unwrap_or(0) as u16;
                if bound != 0 {
                    port = bound;
                    break;
                }
            }
            Ok(_) => break,
            Err(e) => {
                clerk.report("dashboard", e.to_string(), true);
                return;
            }
        }
        tokio::time::sleep(DASHBOARD_POLL).await;
    }
    let url = format!("http://127.0.0.1:{port}/#{token}");
    clerk.report_link(
        "dashboard",
        "Loopback only. The token in the address authorises changes — do not share the URL."
            .to_string(),
        url,
    );
}

async fn gc_action(clerk: &Clerk, dir: &Path) {
    match ipc::request(dir, &IpcRequest::Gc).await {
        Ok(r) if r.ok => {
            // The daemon reports what it protected, not what it freed: the
            // sweep itself is the blob store's own scheduled job, so a
            // bytes-reclaimed figure here would be invented.
            let protected = r
                .data
                .as_ref()
                .and_then(|d| d["protected_blobs"].as_u64())
                .unwrap_or(0);
            clerk.toast(
                format!(
                    "collected — {protected} blob{} are still referenced and protected; \
                     the rest are swept by the store's scheduled gc",
                    if protected == 1 { "" } else { "s" }
                ),
                false,
            );
        }
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "collection failed".into());
            clerk.toast(format!("collection failed: {m}"), true);
        }
        Err(e) => clerk.toast(format!("collection failed: {e}"), true),
    }
}

/// Delete preserved copies the view has already confirmed by name. These bytes
/// exist nowhere else, so every failure is named individually — a partial
/// failure reported as a success would be the one lie this program cannot tell.
async fn prune_action(clerk: &Clerk, dir: &Path, older_than_ms: u64, names: Vec<String>) {
    if names.is_empty() {
        clerk.toast("nothing to prune — no preserved copies were selected", true);
        return;
    }
    let window = humantime::format_duration(Duration::from_millis(older_than_ms)).to_string();
    let count = names.len();
    let d = dir.to_path_buf();
    let outcome = blocking(move || crate::conflicts::prune(&d, &names)).await;
    let (removed, freed, errors) = match outcome {
        Ok(v) => v,
        Err(e) => {
            clerk.toast(format!("prune failed: {e}"), true);
            return;
        }
    };
    let head = format!(
        "pruned {} of {count} preserved cop{} older than {window} · {} freed",
        removed.len(),
        if count == 1 { "y" } else { "ies" },
        crate::state::fmt_size(freed)
    );
    if errors.is_empty() {
        clerk.toast(head, false);
        return;
    }
    let body = format!(
        "{head}\n\n{} cop{} could not be removed and are still in quarantine:\n{}",
        errors.len(),
        if errors.len() == 1 { "y" } else { "ies" },
        errors
            .iter()
            .map(|e| format!("  {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    clerk.toast(
        format!("{head} — {} could not be removed", errors.len()),
        true,
    );
    clerk.report("prune", body, true);
}

async fn invite_action(clerk: &Clerk, dir: &Path, role: Option<String>, ttl_ms: Option<u64>) {
    let scope = format!(
        "{} invite, {}",
        role.as_deref().unwrap_or("editor"),
        match ttl_ms {
            Some(ms) => format!(
                "expires in {}",
                humantime::format_duration(Duration::from_millis(ms))
            ),
            None => "never expires".to_string(),
        }
    );
    match ipc::request(dir, &IpcRequest::Invite { role, ttl_ms }).await {
        Ok(r) if r.ok => match r.data.as_ref().and_then(|d| d["ticket"].as_str()) {
            Some(ticket) => clerk.report("invite", format!("{scope}\n\n{ticket}"), false),
            None => clerk.report(
                "invite",
                "the daemon accepted the request but returned no ticket".to_string(),
                true,
            ),
        },
        Ok(r) => {
            let m = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "the daemon refused to mint an invite".into());
            clerk.report("invite", m, true);
        }
        Err(e) => clerk.report("invite", e.to_string(), true),
    }
}

/// Rotate the session key — the only honest revocation. It rewrites
/// `state.json`, which a running daemon owns and rewrites from its own memory,
/// so a rotation underneath one would be overwritten and the daemon would keep
/// speaking the old key. Refuse instead of racing it.
async fn rekey_action(clerk: &Clerk, dir: &Path) {
    if ipc::daemon_alive(dir).await {
        clerk.report(
            "rekey",
            "Stop this session first. Rekey replaces the session key in state.json, and a \
             running daemon holds that file open and rewrites it from its own memory — \
             rotating underneath it would be undone the moment it next saves."
                .to_string(),
            true,
        );
        return;
    }
    let d = dir.to_path_buf();
    let rotated = blocking(move || {
        let mut state = AppState::load(&d)?;
        let ticket = crate::cli::rekey_rotate(&mut state, crate::now_ms())?;
        state.save(&d)?;
        Ok::<String, crate::cli::CliError>(ticket.encode())
    })
    .await;
    match rotated {
        Ok(Ok(ticket)) => clerk.report(
            "rekey",
            format!(
                "The session key and the admin key are rotated. Your files, history and \
                 settings are untouched.\n\nHand this invite to every member you KEEP:\n\n{ticket}\n\n\
                 On each kept machine, with its session stopped:\n  tazamun rekey --accept <ticket>\n\n\
                 Anyone not given this invite can no longer connect."
            ),
            false,
        ),
        Ok(Err(e)) => clerk.report("rekey", e.to_string(), true),
        Err(e) => clerk.report("rekey", e, true),
    }
}

/// Install or remove the device-wide supervisor. The OS backends shell out to
/// systemd/launchd/schtasks, so this is blocking work.
async fn supervisor_action(clerk: &Clerk, install: bool) {
    let outcome = blocking(move || {
        if install {
            crate::service::install_supervisor()
        } else {
            crate::service::uninstall_supervisor()
        }
    })
    .await;
    match outcome {
        Ok(Ok(msg)) => clerk.toast(msg, false),
        Ok(Err(e)) => clerk.toast(e.to_string(), true),
        Err(e) => clerk.toast(e, true),
    }
}

// ─── updates ─────────────────────────────────────────────────────────────────

/// Where the release list is read from. The install never needs these — it goes
/// through [`crate::cli::run`], which owns the repository, the asset target and
/// the archive layout — but a check has to ask GitHub directly, because the
/// command line's own check prints its answer instead of returning it.
const RELEASE_OWNER: &str = "cc1a2b";
const RELEASE_REPO: &str = "tazamun";

/// The version the window is running, for the menu to name before anything has
/// been checked and with no network at all.
///
/// It is written here, at the top of the worker, rather than left to the poll:
/// the poll is a round trip per session and there may be no sessions, so a
/// window on a fresh machine would otherwise show a blank version forever. It is
/// `CARGO_PKG_VERSION`, not `TAZAMUN_VERSION`: the latter carries a build id
/// (`0.1.9 (9e03554b)`), which is not semver, would never compare equal to a
/// release tag, and would make [`UpdateState::available`] permanently true.
fn record_running_version(clerk: &Clerk) {
    clerk.write(|s| s.update.current = self_update::cargo_crate_version!().to_string());
}

/// Holds `UpdateState.busy` for as long as the work runs, and clears it in
/// `Drop`.
///
/// The same rule [`Ticket`] keeps for the in-flight list, applied to the flag
/// the menu reads to decide whether its own items are live: there is no exit —
/// refusal, transport error, early return, unwind — that can leave the menu
/// permanently mid-check.
struct UpdateBusy(Clerk);

impl UpdateBusy {
    /// Taking the flag also clears the previous failure: what is on screen from
    /// here on belongs to this attempt.
    fn open(clerk: &Clerk) -> Self {
        clerk.write(|s| {
            s.update.busy = true;
            s.update.error = None;
        });
        Self(clerk.clone())
    }
}

impl Drop for UpdateBusy {
    fn drop(&mut self) {
        self.0.write(|s| s.update.busy = false);
    }
}

/// How a check or an install ended, so `UpdateState` is written exactly once —
/// at the end, whichever way the work went.
enum Settled {
    /// The newest release this channel accepts, whether it beats what is
    /// running, and the package manager that owns this install if one does.
    /// `latest` is `None` when nothing has been published yet.
    Checked {
        latest: Option<String>,
        newer: bool,
        managed: Option<(&'static str, &'static str)>,
    },
    /// The binary was replaced; this version starts with the next launch.
    Installed { version: String },
    /// A package manager owns this install, so nothing was touched.
    Managed {
        manager: &'static str,
        command: &'static str,
    },
    /// Nothing was touched, and this is why.
    Failed(String),
}

/// `Cmd::Update`: `apply: false` reports, `apply: true` replaces the binary.
///
/// The download, the asset-target normalisation, the per-archive binary layout
/// and the atomic self-replace are **not** repeated here — the install runs the
/// same command a terminal runs, `tazamun update --tag <version>`, through
/// [`crate::cli::run`]. Two things a window needs and a terminal does not are
/// added on this side: the newest version as a value rather than a printed line,
/// and a refusal for installs a package manager owns.
async fn update_action(clerk: &Clerk, selected: &Arc<Mutex<Option<PathBuf>>>, apply: bool) {
    // The release channel is a per-folder preference, so this reads it from the
    // folder on screen — the folder `tazamun update` would be run in. With none
    // open it falls back to the working directory, exactly as the command line
    // does with no `--dir`.
    let dir = selected
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    let _busy = UpdateBusy::open(clerk);
    let settled = update_run(dir, apply).await;
    // egui's clock, the one the view ages `InFlight.started` against — and never
    // taken while the snapshot lock is held.
    let now = clerk.now();
    let current = self_update::cargo_crate_version!();

    match settled {
        Settled::Checked {
            latest,
            newer,
            managed,
        } => {
            let said = match (latest.as_deref(), newer) {
                (None, _) => format!("no releases have been published yet — running {current}"),
                (Some(v), true) => match managed {
                    Some((manager, command)) => format!(
                        "update available: {current} → {v}. This copy belongs to {manager}, so \
                         take it with `{command}` rather than from here."
                    ),
                    None => {
                        format!("update available: {current} → {v} — install it from this menu")
                    }
                },
                (Some(v), false) if apply => {
                    format!("nothing installed — running {current}, newest release {v}")
                }
                (Some(v), false) => format!("up to date — running {current}, newest release {v}"),
            };
            clerk.write(move |s| {
                s.update.latest = latest;
                s.update.checked_at = Some(now);
            });
            clerk.toast(said, false);
        }
        Settled::Installed { version } => {
            let said = format!(
                "installed {version} — it runs from the next launch. Close this window and \
                 reopen it, then restart any session still running the old binary."
            );
            clerk.write(move |s| {
                s.update.latest = Some(version);
                s.update.applied = true;
                s.update.checked_at = Some(now);
            });
            clerk.toast(said, false);
        }
        Settled::Managed { manager, command } => {
            let short = format!("this copy belongs to {manager} — update it with `{command}`");
            let full = format!(
                "This tazamun was installed by {manager}, which keeps its own record of which \
                 version is on this machine.\n\nReplacing the binary from here would leave that \
                 record naming the old version, and the next {manager} operation could quietly \
                 put the old binary back. Nothing was downloaded and nothing was \
                 changed.\n\nUpdate it with:\n\n  {command}"
            );
            clerk.write({
                let short = short.clone();
                move |s| s.update.error = Some(short)
            });
            clerk.toast(short, true);
            clerk.report("update", full, true);
        }
        // `checked_at` deliberately does not move: a check that failed is not a
        // check, and the view must go on ageing the last one that worked.
        Settled::Failed(why) => {
            clerk.write({
                let why = why.clone();
                move |s| s.update.error = Some(why)
            });
            clerk.toast(why, true);
        }
    }
}

/// Resolve the newest release, and install it when asked to. Every exit is a
/// [`Settled`]; nothing here writes the snapshot.
async fn update_run(dir: PathBuf, apply: bool) -> Settled {
    // Judged before any network work: an install into a tree a package manager
    // owns is refused outright, and a check says so rather than offering a
    // button that would only refuse later.
    let managed = match blocking(current_install_manager).await {
        Ok(m) => m,
        Err(e) => return Settled::Failed(e),
    };
    if apply && let Some((manager, command)) = managed {
        return Settled::Managed { manager, command };
    }

    let token = github_token();
    let had_token = token.is_some();
    let folder = dir.clone();
    let resolved = blocking(move || {
        let channel = AppState::load(&folder)
            .map(|s| s.config.update_channel)
            .unwrap_or_else(|_| "stable".to_string());
        fetch_release_versions(token).map(|versions| (channel, versions))
    })
    .await;
    let (channel, versions) = match resolved {
        Ok(Ok(v)) => v,
        Ok(Err(why)) => return Settled::Failed(why),
        Err(e) => return Settled::Failed(e),
    };

    let current = self_update::cargo_crate_version!();
    let latest = newest_release(&channel, &versions);
    let newer = is_newer(current, latest.as_deref());
    match install_target(apply, current, latest.as_deref()) {
        Some(version) => install_release(dir, version, had_token).await,
        None => Settled::Checked {
            latest,
            newer,
            managed,
        },
    }
}

/// Install exactly `version` by running the command line's own `update`.
///
/// The tag is pinned to the release the resolution step just found rather than
/// left for the installer to pick again, for two reasons: the window can then
/// name the version it actually installed — `applied` is a fact, not a guess —
/// and a prerelease published between the resolution and the swap cannot land on
/// a machine that asked for the stable channel.
async fn install_release(dir: PathBuf, version: String, had_token: bool) -> Settled {
    let cli = crate::cli::Cli {
        // The same folder the channel was read from, so this is byte for byte
        // the command the user could have typed in it.
        dir,
        verbose: 0,
        net: crate::cli::NetFlags::default(),
        cmd: Some(crate::cli::Cmd::Update {
            check: false,
            tag: Some(version.clone()),
            yes: true,
            // Not passed through: the updater falls back to GITHUB_TOKEN /
            // GH_TOKEN itself, which is the same token the check used.
            token: None,
        }),
    };
    match crate::cli::run(cli, crate::ui::progress::Ui::disabled()).await {
        Ok(()) => Settled::Installed { version },
        Err(e) => Settled::Failed(update_failure(&e.to_string(), had_token)),
    }
}

/// The release list, newest first, as versions. Blocking: `self_update` drives
/// reqwest's blocking client, which panics if it is built on a reactor thread.
///
/// It downloads no asset and replaces nothing — this is the read half of
/// `update`, which is why a check cannot install even if it wanted to.
fn fetch_release_versions(token: Option<String>) -> Result<Vec<String>, String> {
    let had_token = token.is_some();
    let mut list = self_update::backends::github::ReleaseList::configure();
    list.repo_owner(RELEASE_OWNER).repo_name(RELEASE_REPO);
    if let Some(t) = &token {
        list.auth_token(t);
    }
    let releases = list
        .build()
        .map_err(|e| update_failure(&e.to_string(), had_token))?
        .fetch()
        .map_err(|e| update_failure(&e.to_string(), had_token))?;
    Ok(releases.into_iter().map(|r| r.version).collect())
}

/// The token that lifts GitHub's anonymous rate limit, from the same two
/// variables `tazamun update` reads.
fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|t| !t.is_empty())
}

/// Which package manager owns the running executable, if one does.
///
/// Canonicalised first, because the name on `PATH` is usually a shim: the npm
/// install is reached through `/usr/local/bin/tazamun`, and only the link target
/// says `node_modules`.
fn current_install_manager() -> Option<(&'static str, &'static str)> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    managed_by_path(&exe)
}

/// Whether an install can safely replace itself, judged from where it lives, and
/// if it cannot, the manager that owns it with the command that updates it.
/// Mirrors `cli::managed_by_path`, which is what the command line prints after a
/// self-replace; here the same judgement is made *before* one, because a window
/// has a button where the terminal has a warning.
///
/// Split on both separators rather than `components()`: `components()` cannot
/// see the segments of a Windows path on a Unix host, which would make the exact
/// layouts users report untestable. A Unix filename containing a literal
/// backslash could over-split, and would cost at worst one wrong refusal.
fn managed_by_path(exe: &Path) -> Option<(&'static str, &'static str)> {
    let s = exe.to_string_lossy();
    for part in s.split(['/', '\\']) {
        match part {
            "node_modules" => return Some(("npm", "npm update -g tazamun")),
            "Cellar" | "homebrew" | "Homebrew" => {
                return Some(("Homebrew", "brew upgrade tazamun"));
            }
            _ => {}
        }
    }
    None
}

/// The newest release this channel will accept, from the list GitHub returns
/// newest-first.
///
/// `stable` skips prereleases, which is what `/releases/latest` — the endpoint
/// the installer uses when it is handed no tag — already does; `beta` takes the
/// newest of everything, matching `update`'s beta branch. Without the filter the
/// window could offer a prerelease to a stable machine and then pin the install
/// to it, which is looser than the command line.
fn newest_release(channel: &str, versions: &[String]) -> Option<String> {
    versions
        .iter()
        .map(|v| v.trim_start_matches('v'))
        .find(|v| channel == "beta" || !is_prerelease(v))
        .map(str::to_string)
}

/// A semver prerelease (`0.2.0-beta.1`). Build metadata (`0.2.0+ci.7`) is not
/// one, and its `-` must not be mistaken for one.
fn is_prerelease(version: &str) -> bool {
    version
        .split('+')
        .next()
        .is_some_and(|core| core.contains('-'))
}

/// Whether `latest` beats what is running, by the same comparison the updater
/// itself uses. A version neither side can parse is not an update.
fn is_newer(current: &str, latest: Option<&str>) -> bool {
    latest.is_some_and(|v| self_update::version::bump_is_greater(current, v).unwrap_or(false))
}

/// The version an update command should install, if any.
///
/// `None` for a check — read-only by construction rather than by remembering to
/// branch — and `None` for an install with nothing newer to fetch, which keeps
/// the updater from being pointed at the version already running or at an older
/// one. Pure, so "a check never installs" is a property with a test rather than
/// a claim in a comment.
fn install_target(apply: bool, current: &str, latest: Option<&str>) -> Option<String> {
    if !apply || !is_newer(current, latest) {
        return None;
    }
    latest.map(str::to_string)
}

/// Turn an updater failure into a sentence with a next move in it.
///
/// Four of these actually happen, and as raw library text they all read as "the
/// update failed": a machine with no network, an anonymous API call over
/// GitHub's hourly cap, a release with no build for this platform, and a binary
/// this account may not overwrite. Each needs a different action, so each is
/// named, and the raw text is kept on the end for whoever has to diagnose it.
///
/// Phrased as a reason, not as a sentence: it lands in `UpdateState.error`,
/// which the view already introduces with "the last check could not finish".
fn update_failure(raw: &str, had_token: bool) -> String {
    let detail = raw.trim();
    let low = detail.to_ascii_lowercase();
    if low.contains("status: 403") || low.contains("status: 429") || low.contains("rate limit") {
        return format!(
            "GitHub is rate-limiting this machine. Anonymous release queries are capped per \
             hour — wait for the cap to reset, or set GITHUB_TOKEN or GH_TOKEN and check \
             again. ({detail})"
        );
    }
    if low.contains("404") && !had_token {
        return format!(
            "GitHub answered 404 — the repository is private, or it has published no releases \
             yet. Set GITHUB_TOKEN or GH_TOKEN, or run `tazamun update --token <TOKEN>` from a \
             terminal. ({detail})"
        );
    }
    if low.contains("permission denied")
        || low.contains("access is denied")
        || low.contains("os error 13")
        || low.contains("read-only file system")
    {
        return format!(
            "the download succeeded but the tazamun binary could not be replaced — this account \
             may not write over it. Run the update from an administrator or root shell, or \
             reinstall tazamun somewhere you own. ({detail})"
        );
    }
    if low.contains("no asset found")
        || low.contains("not found in archive")
        || low.contains("no releases found")
    {
        return format!(
            "that release carries no build for this platform, so there is nothing to install — \
             the detail below names the target it looked for. Update through whatever installed \
             this copy instead. ({detail})"
        );
    }
    if looks_offline(&low) {
        return format!(
            "could not reach github.com — this machine looks offline. Tazamun itself syncs \
             without it; only the update needs the network, and only to fetch the \
             release. ({detail})"
        );
    }
    detail.to_string()
}

/// Transport failures that mean "the network is not there", as reqwest and the
/// resolver word them.
fn looks_offline(low: &str) -> bool {
    const SIGNS: [&str; 8] = [
        "dns error",
        "failed to lookup address",
        "temporary failure in name resolution",
        "error sending request",
        "connection refused",
        "network is unreachable",
        "no route to host",
        "operation timed out",
    ];
    SIGNS.iter().any(|s| low.contains(s))
}

// ─── session lifecycle ───────────────────────────────────────────────────────

async fn start_session(
    clerk: &Clerk,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    dir: &Path,
) {
    let key = dir.to_string_lossy().to_string();
    // The folder's gate — not this map lock — is what makes a start idempotent:
    // only one command per folder runs at a time, so the map is only ever
    // touched in short critical sections and two folders can start at once.
    let already = { started.lock().await.contains_key(&key) };
    if already || ipc::daemon_alive(dir).await {
        clerk.toast(copy::TOAST_ALREADY_RUNNING, true);
        return;
    }
    let d = dir.to_path_buf();
    let saved = match blocking(move || AppState::load(&d).map(|st| st.config)).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            clerk.toast(format!("cannot start: {e}"), true);
            return;
        }
        Err(e) => {
            clerk.toast(format!("cannot start: {e}"), true);
            return;
        }
    };
    let net = match crate::cli::resolve_net_config(&saved, &crate::cli::NetFlags::default()) {
        Ok(n) => n,
        Err(e) => {
            clerk.toast(format!("network config error: {e}"), true);
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
            started.lock().await.insert(key, handle);
            clerk.toast(copy::TOAST_STARTED_HOSTED, false);
        }
        Err(e) => clerk.toast(format!("could not start: {e}"), true),
    }
}

async fn stop_session(
    clerk: &Clerk,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    dir: &Path,
) {
    let key = dir.to_string_lossy().to_string();
    // Take the handle out under the map lock, then release it: a wedged actor
    // must not hold every other session's start and stop behind it.
    let hosted = { started.lock().await.remove(&key) };
    if let Some(handle) = hosted {
        // Bounded so a wedged actor can't keep the ticket open forever.
        if tokio::time::timeout(GUI_SHUTDOWN, handle.shutdown())
            .await
            .is_err()
        {
            clerk.toast(copy::TOAST_STOP_TIMEOUT, true);
        } else {
            clerk.toast("stopped", false);
        }
        return;
    }
    if !ipc::daemon_alive(dir).await {
        clerk.toast(copy::TOAST_NOT_RUNNING, true);
        return;
    }
    match ipc::request(dir, &IpcRequest::Shutdown).await {
        Ok(r) if r.ok => clerk.toast(copy::TOAST_STOPPED, false),
        Ok(r) => clerk.toast(
            r.error
                .map(|e| e.message)
                .unwrap_or_else(|| "shutdown refused".into()),
            true,
        ),
        Err(e) => clerk.toast(format!("stop failed: {e}"), true),
    }
}

async fn set_paused(clerk: &Clerk, dir: &Path, pause: bool) {
    let abs = absolute(dir);
    let d = dir.to_path_buf();
    // Registry and state reads both hit the disk; do them together, off-thread.
    let prepared = blocking(move || {
        if AppState::load(&d).is_err() {
            return false;
        }
        let abs = absolute(&d).to_string_lossy().to_string();
        let mut reg = Registry::load();
        if !reg.sessions.iter().any(|s| s.path == abs) {
            reg.register(&d, SessionKind::Init, crate::now_ms());
            let _ = reg.save();
        }
        true
    })
    .await;
    match prepared {
        Ok(true) => {}
        Ok(false) => {
            clerk.toast(copy::TOAST_NOT_SESSION_FOLDER, true);
            return;
        }
        Err(e) => {
            clerk.toast(format!("could not reach the session: {e}"), true);
            return;
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
            Ok(r) if r.ok => clerk.toast(
                if pause {
                    copy::TOAST_PAUSED_LIVE
                } else {
                    copy::TOAST_RESUMED_LIVE
                },
                false,
            ),
            Ok(r) => clerk.toast(
                r.error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "supervisor refused".into()),
                true,
            ),
            Err(e) => clerk.toast(format!("failed: {e}"), true),
        }
        return;
    }
    let d = dir.to_path_buf();
    let deferred = blocking(move || {
        let mut reg = Registry::load();
        reg.set_paused(&d, pause);
        reg.save().map_err(|e| e.to_string())
    })
    .await;
    match deferred {
        Ok(Ok(())) => clerk.toast(copy::toast_paused_deferred(pause), false),
        Ok(Err(e)) => clerk.toast(format!("could not record the pause: {e}"), true),
        Err(e) => clerk.toast(format!("could not record the pause: {e}"), true),
    }
}

// ─── polling ─────────────────────────────────────────────────────────────────

/// One pass every [`REFRESH`], or immediately when a command settles. `Notify`
/// stores a permit when nobody is waiting, so a poke that arrives mid-pass is
/// not lost — it runs the next pass without delay instead.
async fn refresh_loop(
    clerk: Clerk,
    started: Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    selected: Arc<Mutex<Option<PathBuf>>>,
    poke: Arc<Notify>,
) {
    loop {
        run_refresh(&clerk, &started, &selected).await;
        tokio::select! {
            _ = poke.notified() => {}
            _ = tokio::time::sleep(REFRESH) => {}
        }
    }
}

/// What a poll of the selected session says the snapshot should do. `Keep` is
/// the whole point: a timed-out poll must never blank a good reading.
enum DetailUpdate {
    Set(Box<Detail>),
    Keep,
    Clear,
}

async fn run_refresh(
    clerk: &Clerk,
    started: &Arc<tokio::sync::Mutex<BTreeMap<String, DaemonHandle>>>,
    selected: &Arc<Mutex<Option<PathBuf>>>,
) {
    clerk.set_polling(true);
    let sel = selected
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let hosted: HashSet<String> = started.lock().await.keys().cloned().collect();
    let (prior_rows, prior_detail) = clerk.read(|s| {
        let rows: BTreeMap<String, SessionRow> = s
            .overview
            .as_ref()
            .map(|o| {
                o.sessions
                    .iter()
                    .map(|r| (r.path.clone(), r.clone()))
                    .collect()
            })
            .unwrap_or_default();
        (rows, s.detail.clone())
    });
    // Only the reading for the folder still on screen may be carried forward.
    let prior_detail =
        prior_detail.filter(|d| sel.as_ref().is_some_and(|p| d.dir == p.to_string_lossy()));

    let (overview, detail) = tokio::join!(fetch_overview(clerk, &hosted, &prior_rows), async {
        match &sel {
            Some(dir) => fetch_detail(clerk, dir, prior_detail.as_ref()).await,
            None => DetailUpdate::Clear,
        }
    });

    clerk.polling.store(false, Ordering::Relaxed);
    clerk.write(|s| {
        if let Some(o) = overview {
            s.overview = Some(o);
        }
        match detail {
            DetailUpdate::Set(d) => s.detail = Some(*d),
            DetailUpdate::Keep => {}
            DetailUpdate::Clear => s.detail = None,
        }
        s.tick = s.tick.wrapping_add(1);
    });
}

/// One IPC round trip with a poll's budget, marking the session slow while it is
/// still outstanding so the window can say so before the answer arrives.
async fn poll_request(
    clerk: &Clerk,
    dir: &Path,
    key: &str,
    req: &IpcRequest,
) -> Result<IpcResponse, IpcError> {
    let fut = ipc::request_with_timeout(dir, req, POLL_TIMEOUT);
    tokio::pin!(fut);
    tokio::select! {
        r = &mut fut => r,
        _ = tokio::time::sleep(POLL_SLOW) => {
            let key = key.to_string();
            clerk.write(move |s| { s.reach.insert(key, Reach::Slow); });
            fut.await
        }
    }
}

/// Records how a session's poll finished. `fetched_at` moves only on a definite
/// answer, so the view can age a stalled reading honestly.
fn settle_reach(clerk: &Clerk, key: &str, reach: Reach) {
    let now = clerk.now();
    let key = key.to_string();
    clerk.write(move |s| {
        if reach != Reach::Stalled {
            s.fetched_at.insert(key.clone(), now);
        }
        s.reach.insert(key, reach);
    });
}

/// `None` when the registry itself could not be read — the previous overview is
/// then kept rather than replaced by an empty list of sessions.
async fn fetch_overview(
    clerk: &Clerk,
    hosted: &HashSet<String>,
    prior: &BTreeMap<String, SessionRow>,
) -> Option<Overview> {
    // NB: do NOT prune+save the registry here. This runs every ~1.5s, so a
    // transiently unavailable volume (a network share or USB hiccup) would make
    // `AppState::exists` momentarily false and permanently unregister the session.
    // Unreadable sessions are shown with `readable: false` and simply reappear
    // when the volume returns; the CLI's explicit commands do the real pruning.
    let reg = match blocking(Registry::load).await {
        Ok(r) => r,
        Err(e) => {
            clerk.toast(format!("could not read the session list: {e}"), true);
            return None;
        }
    };
    // Every session is polled at once, and the supervisor probe rides alongside:
    // N sessions cost one round trip, not N.
    let (supervisor, sessions) = tokio::join!(
        crate::supervisor::control_alive(),
        join_all(
            reg.sessions
                .iter()
                .map(|s| session_row(clerk, s, hosted, prior.get(&s.path)))
        )
    );
    Some(Overview {
        version: env!("TAZAMUN_VERSION").to_string(),
        supervisor,
        sessions,
    })
}

/// The part of a session's row that comes off the disk rather than the socket.
struct DiskFacts {
    readable: bool,
    files: usize,
    total_bytes: u64,
    role: String,
    strict: bool,
    id_short: String,
    conflicts: usize,
}

fn read_disk_facts(dir: &Path) -> DiskFacts {
    let conflicts = crate::conflicts::list(dir).len();
    match AppState::load(dir) {
        Ok(st) => DiskFacts {
            readable: true,
            files: st.files.values().filter(|f| !f.deleted).count(),
            total_bytes: st
                .files
                .values()
                .filter(|f| !f.deleted)
                .map(|f| f.size)
                .sum(),
            role: st.config.role.as_str().to_string(),
            strict: st.config.strict,
            id_short: st.node_id_short().unwrap_or_else(|| "?".into()),
            conflicts,
        },
        Err(_) => DiskFacts {
            readable: false,
            files: 0,
            total_bytes: 0,
            role: "?".into(),
            strict: true,
            id_short: "-".into(),
            conflicts,
        },
    }
}

async fn session_row(
    clerk: &Clerk,
    entry: &crate::registry::SessionRef,
    hosted: &HashSet<String>,
    prior: Option<&SessionRow>,
) -> SessionRow {
    let dir = PathBuf::from(&entry.path);
    let facts = {
        let d = dir.clone();
        blocking(move || read_disk_facts(&d)).await
    };
    // One Status round trip doubles as the liveness probe: `daemon_alive` is
    // literally this request, so asking twice bought a second stall and nothing
    // else.
    let polled = poll_request(clerk, &dir, &entry.path, &IpcRequest::Status).await;
    let (running, peers_online, peers_total) = match polled {
        Ok(r) if r.ok => {
            settle_reach(clerk, &entry.path, Reach::Live);
            let d = r.data.unwrap_or_default();
            let members = d.get("members").and_then(|v| v.as_array());
            let total = members.map(|m| m.len()).unwrap_or(0);
            let online = members
                .map(|m| {
                    m.iter()
                        .filter(|x| x.get("online").and_then(|b| b.as_bool()).unwrap_or(false))
                        .count()
                })
                .unwrap_or(0);
            (true, online, total)
        }
        // A refusal is still an answer: something is listening and healthy
        // enough to say no.
        Ok(_) => {
            settle_reach(clerk, &entry.path, Reach::Live);
            (true, 0, 0)
        }
        // Nothing is listening. Also a definite answer, and the common one.
        Err(IpcError::NoDaemon) => {
            settle_reach(clerk, &entry.path, Reach::Live);
            (false, 0, 0)
        }
        // A socket that accepts and then says nothing. Keep the last good
        // reading on screen and mark it stale rather than zeroing the row.
        Err(_) => {
            settle_reach(clerk, &entry.path, Reach::Stalled);
            match prior {
                Some(p) => (p.running, p.peers_online, p.peers_total),
                None => (false, 0, 0),
            }
        }
    };
    let facts = match facts {
        Ok(f) => f,
        Err(e) => {
            clerk.toast(format!("could not read {}: {e}", entry.path), true);
            return carry_over(entry, hosted, prior);
        }
    };
    SessionRow {
        name: base_name(&entry.path),
        path: entry.path.clone(),
        running,
        paused: entry.paused,
        hosted_by_gui: hosted.contains(&entry.path),
        readable: facts.readable,
        role: facts.role,
        strict: facts.strict,
        files: facts.files,
        total_bytes: facts.total_bytes,
        conflicts: facts.conflicts,
        peers_online,
        peers_total,
        id_short: facts.id_short,
    }
}

/// The row to show when this pass learned nothing new about a session.
fn carry_over(
    entry: &crate::registry::SessionRef,
    hosted: &HashSet<String>,
    prior: Option<&SessionRow>,
) -> SessionRow {
    match prior {
        Some(p) => SessionRow {
            paused: entry.paused,
            hosted_by_gui: hosted.contains(&entry.path),
            ..p.clone()
        },
        None => SessionRow {
            name: base_name(&entry.path),
            path: entry.path.clone(),
            running: false,
            paused: entry.paused,
            hosted_by_gui: hosted.contains(&entry.path),
            readable: false,
            role: "?".into(),
            strict: true,
            files: 0,
            total_bytes: 0,
            conflicts: 0,
            peers_online: 0,
            peers_total: 0,
            id_short: "-".into(),
        },
    }
}

/// The parts of the open session's page that come off the disk.
struct LocalDetail {
    conflicts: Vec<ConflictRow>,
    audit: Vec<AuditRow>,
    offline_invite: Option<String>,
}

fn read_local_detail(dir: &Path) -> LocalDetail {
    LocalDetail {
        conflicts: crate::conflicts::list(dir)
            .into_iter()
            .map(|c| ConflictRow {
                name: c.name,
                path: c.path.unwrap_or_default(),
                reason: c.reason.unwrap_or_else(|| "conflicting copy".into()),
                ts_ms: c.ts_ms,
                size: c.size,
            })
            .collect(),
        audit: crate::audit::read(dir, &crate::audit::Filter::default())
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
            .collect(),
        offline_invite: crate::home::offline_invite(dir),
    }
}

async fn fetch_detail(clerk: &Clerk, dir: &Path, prior: Option<&Detail>) -> DetailUpdate {
    let key = dir.to_string_lossy().to_string();
    let local = {
        let d = dir.to_path_buf();
        match blocking(move || read_local_detail(&d)).await {
            Ok(v) => v,
            Err(e) => {
                clerk.toast(format!("could not read {key}: {e}"), true);
                return DetailUpdate::Keep;
            }
        }
    };
    let LocalDetail {
        conflicts,
        audit,
        offline_invite,
    } = local;

    // Reuse the previous ticket for the same running folder: every v2 mint
    // carries a fresh invite id, so re-minting per poll would make the visible
    // ticket (and its QR) churn every 1.5s.
    let prior_invite = prior.filter(|d| d.running).and_then(|d| d.invite.clone());
    let live_invite = match prior_invite {
        Some(t) => Some(t),
        None => match poll_request(
            clerk,
            dir,
            &key,
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
    let invite = live_invite.or(offline_invite);

    match poll_request(clerk, dir, &key, &IpcRequest::DashboardState).await {
        Ok(r) if r.ok => {
            let d = r.data.unwrap_or_default();
            DetailUpdate::Set(Box::new(parse_running_detail(
                dir, &d, conflicts, audit, invite,
            )))
        }
        // The daemon answered and refused: a real answer, worth showing.
        Ok(r) => DetailUpdate::Set(Box::new(Detail {
            dir: key,
            error: r.error.map(|e| e.message),
            conflicts,
            audit,
            invite,
            ..Default::default()
        })),
        // No daemon at all: read what the folder itself can tell us.
        Err(IpcError::NoDaemon) => {
            let d = dir.to_path_buf();
            match blocking(move || offline_detail(&d, conflicts, audit, invite)).await {
                Ok(detail) => DetailUpdate::Set(Box::new(detail)),
                Err(e) => {
                    clerk.toast(format!("could not read {key}: {e}"), true);
                    DetailUpdate::Keep
                }
            }
        }
        // A poll that timed out is not news. Keeping the last good reading is
        // the difference between "this is a moment stale" and "everything you
        // were looking at is gone".
        Err(e) => match prior {
            Some(_) => DetailUpdate::Keep,
            None => DetailUpdate::Set(Box::new(Detail {
                dir: key,
                error: Some(e.to_string()),
                conflicts,
                audit,
                invite,
                ..Default::default()
            })),
        },
    }
}

fn offline_detail(
    dir: &Path,
    conflicts: Vec<ConflictRow>,
    audit: Vec<AuditRow>,
    invite: Option<String>,
) -> Detail {
    match AppState::load(dir) {
        Ok(st) => Detail {
            dir: dir.to_string_lossy().into(),
            running: false,
            role: st.config.role.as_str().to_string(),
            strict: st.config.strict,
            invite,
            files: st
                .files
                .iter()
                .filter(|(_, r)| !r.deleted)
                .map(|(p, r)| FileRow {
                    path: p.as_str().to_string(),
                    size: r.size,
                    locked_by: None,
                    mine_lock: false,
                })
                .collect(),
            conflicts,
            audit,
            ..Default::default()
        },
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
            audit: c.get("audit").and_then(|b| b.as_bool()).unwrap_or(false),
            hooks: c.get("hooks").and_then(|b| b.as_bool()).unwrap_or(false),
            notify: c.get("notify").and_then(|b| b.as_bool()).unwrap_or(false),
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

/// Answers a file query against the daemon's whole index.
///
/// The snapshot's file list is capped, so filtering it client-side can only
/// find what the cap already let through. This asks the daemon instead, which
/// is the only way a path past that cap is reachable from the window.
async fn search_files(cmd: Cmd, clerk: Clerk) {
    let Cmd::SearchFiles {
        dir,
        pattern,
        by_size,
        desc,
        offset,
    } = cmd
    else {
        return;
    };
    let q = ipc::FilesQuery {
        pattern: (!pattern.trim().is_empty()).then(|| pattern.clone()),
        sort: Some(if by_size { "size" } else { "path" }.to_string()),
        desc,
        offset,
        limit: Some(FILE_PAGE),
        include_deleted: false,
    };
    // A short timeout: this runs while the user types, and a query that has to
    // wait on a wedged daemon is better abandoned than left to stack up.
    let reply = ipc::request_with_timeout(&dir, &IpcRequest::Files(q), SEARCH_TIMEOUT).await;
    let key = dir.to_string_lossy().to_string();
    match reply {
        Ok(r) if r.ok => {
            let data = r.data.unwrap_or(serde_json::Value::Null);
            let rows = data["files"]
                .as_array()
                .map(|a| a.iter().map(file_row_from).collect())
                .unwrap_or_default();
            let page = FilePage {
                dir: key,
                pattern,
                rows,
                matched: data["matched"].as_u64().unwrap_or(0) as usize,
                offset: data["offset"].as_u64().unwrap_or(0) as usize,
                next_offset: data["next_offset"].as_u64().map(|v| v as usize),
            };
            clerk.write(|s| s.file_page = Some(page));
        }
        // A failed query must not leave the previous answer on screen looking
        // like the answer to what was just typed.
        Ok(r) => {
            let why = r
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "the file query was refused".into());
            clerk.write(|s| s.file_page = None);
            clerk.toast(why, true);
        }
        Err(e) => {
            clerk.write(|s| s.file_page = None);
            clerk.toast(format!("could not search this folder: {e}"), true);
        }
    }
}

/// One row of a file query answer.
fn file_row_from(v: &serde_json::Value) -> FileRow {
    FileRow {
        path: jstr(v, "path"),
        size: v["size"].as_u64().unwrap_or(0),
        locked_by: v["locked_by"].as_str().map(str::to_string),
        mine_lock: v["mine_lock"].as_bool().unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clerk() -> Clerk {
        Clerk {
            shared: Arc::new(Mutex::new(Shared::default())),
            ctx: egui::Context::default(),
            polling: Arc::new(AtomicBool::new(false)),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn ids(c: &Clerk) -> Vec<u64> {
        c.read(|s| s.inflight.iter().map(|f| f.id).collect())
    }

    /// The one that matters: a leaked ticket is a spinner that never stops, so
    /// EVERY way out of a command must take its entry off the screen — the happy
    /// path, an early return, and an unwind through the middle of the work.
    #[test]
    fn every_exit_path_clears_its_ticket() {
        let c = clerk();

        // Ran to the end.
        {
            let _t = c.open("/s", "locking", "a.txt");
            assert_eq!(ids(&c).len(), 1);
        }
        assert!(ids(&c).is_empty());

        // Returned early, the way a refusal branch does.
        fn publish(c: &Clerk, accepted: bool) -> &'static str {
            let _t = c.open("/s", "publishing", "a.txt");
            if !accepted {
                return "refused";
            }
            "published"
        }
        assert_eq!(publish(&c, false), "refused");
        assert!(ids(&c).is_empty());
        assert_eq!(publish(&c, true), "published");
        assert!(ids(&c).is_empty());

        // Unwound. `?`-style exits are the same shape; a panic is the one that
        // no amount of care at the call sites would have covered.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let c2 = c.clone();
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _t = c2.open("/s", "restoring", "a.txt");
            panic!("the daemon did something unspeakable");
        }));
        std::panic::set_hook(prev);
        assert!(out.is_err());
        assert!(ids(&c).is_empty());

        // And nothing is left claiming the window is busy.
        assert!(!c.read(|s| s.busy));
    }

    #[test]
    fn tickets_are_numbered_and_independent() {
        let c = clerk();
        let a = c.open("/one", "locking", "a.txt");
        let b = c.open("/two", "locking", "b.txt");
        assert_eq!(ids(&c), vec![a.id, b.id]);
        assert!(a.id < b.id);
        drop(a);
        assert_eq!(ids(&c), vec![b.id]);
        assert!(c.read(|s| s.busy));
        drop(b);
        assert!(!c.read(|s| s.busy));
    }

    #[test]
    fn busy_covers_the_poll_as_well_as_commands() {
        let c = clerk();
        c.set_polling(true);
        assert!(c.read(|s| s.busy));
        c.set_polling(false);
        assert!(!c.read(|s| s.busy));
        let t = c.open("/s", "locking", "a.txt");
        assert!(c.read(|s| s.busy));
        drop(t);
        assert!(!c.read(|s| s.busy));
    }

    #[test]
    fn a_cancel_marks_only_its_own_ticket() {
        let c = clerk();
        let a = c.open("/s", "awaiting", "a.txt");
        let b = c.open("/s", "awaiting", "b.txt");
        assert!(c.write(|s| mark_cancelling(s, a.id)));
        assert!(a.cancelled());
        assert!(!b.cancelled());
        // A cancel for a command that already settled is not an error; there is
        // simply nothing left to stop.
        let gone = a.id + b.id + 1;
        assert!(!c.write(|s| mark_cancelling(s, gone)));
    }

    #[test]
    fn steps_are_visible_while_a_sequence_runs() {
        let c = clerk();
        let t = c.open("/s", "renaming", "a.txt → b.txt");
        assert_eq!(c.read(|s| s.inflight[0].step), None);
        t.step(2, 4);
        assert_eq!(c.read(|s| s.inflight[0].step), Some((2, 4)));
        t.step(4, 4);
        assert_eq!(c.read(|s| s.inflight[0].step), Some((4, 4)));
    }

    #[test]
    fn closing_a_ticket_twice_is_caught() {
        let mut s = Shared::default();
        open_inflight(&mut s, 7, "/s".into(), "locking", "a.txt".into(), 0.0);
        assert!(close_inflight(&mut s, 7));
        assert!(!close_inflight(&mut s, 7));
    }

    #[tokio::test]
    async fn one_folder_serialises_and_two_folders_do_not() {
        let gates = Gates::default();
        let a1 = gates.get("/one");
        let a2 = gates.get("/one");
        let b = gates.get("/two");
        assert!(Arc::ptr_eq(&a1, &a2), "one folder must share one gate");
        assert!(!Arc::ptr_eq(&a1, &b), "two folders must not share a gate");

        // A second command on the folder waits; a command on another does not.
        let held = a1.lock().await;
        assert!(a2.try_lock().is_err());
        assert!(b.try_lock().is_ok());
        drop(held);
        assert!(a2.try_lock().is_ok());
    }

    #[tokio::test]
    async fn idle_gates_are_reclaimed_but_busy_ones_are_not() {
        let gates = Gates::default();
        {
            let _idle = gates.get("/one");
            assert_eq!(gates.len(), 1);
        }
        // Nothing owns /one now, so asking for another folder forgets it.
        let busy = gates.get("/two");
        assert_eq!(gates.len(), 1);
        // /two is still owned here, so it survives the next sweep.
        let _other = gates.get("/three");
        assert_eq!(gates.len(), 2);
        drop(busy);
    }

    #[test]
    fn the_loop_answers_these_itself() {
        for cmd in [
            Cmd::Quit,
            Cmd::Refresh,
            Cmd::Select(None),
            Cmd::Cancel(1),
            Cmd::PickFolder(PickTarget::Init),
        ] {
            assert!(entry_for(&cmd).is_none());
        }
    }

    #[test]
    fn every_dispatched_command_is_filed_under_its_folder() {
        let dir = PathBuf::from("/tmp/register");
        let key = dir.to_string_lossy().to_string();
        let dispatched = [
            Cmd::Lock {
                dir: dir.clone(),
                path: "a.txt".into(),
            },
            Cmd::Unlock {
                dir: dir.clone(),
                path: "a.txt".into(),
            },
            Cmd::LockWait {
                dir: dir.clone(),
                path: "a.txt".into(),
            },
            Cmd::ConfigSet {
                dir: dir.clone(),
                key: "autolock".into(),
                value: "true".into(),
            },
            Cmd::ResolveMine {
                dir: dir.clone(),
                id: "c1".into(),
                target: "a.txt".into(),
            },
            Cmd::ResolveBoth {
                dir: dir.clone(),
                id: "c1".into(),
                target: "a.txt".into(),
            },
            Cmd::Restore {
                dir: dir.clone(),
                path: "a.txt".into(),
                n: 2,
            },
            Cmd::Tag {
                dir: dir.clone(),
                path: "a.txt".into(),
                n: 2,
                name: None,
            },
            Cmd::Pin {
                dir: dir.clone(),
                path: "a.txt".into(),
                n: 2,
                pinned: true,
            },
            Cmd::ConflictDiscard {
                dir: dir.clone(),
                id: "c1".into(),
            },
            Cmd::PeerName {
                dir: dir.clone(),
                id: "abcdef".into(),
                name: None,
            },
            Cmd::Start(dir.clone()),
            Cmd::Stop(dir.clone()),
            Cmd::Pause(dir.clone()),
            Cmd::Resume(dir.clone()),
            Cmd::Init(dir.clone()),
            Cmd::Join(dir.clone(), "tzm1...".into()),
            Cmd::Move {
                dir: dir.clone(),
                from: "a.txt".into(),
                to: "b.txt".into(),
            },
            Cmd::Diff {
                dir: dir.clone(),
                path: "a.txt".into(),
                n: 2,
            },
            Cmd::Doctor { dir: dir.clone() },
            Cmd::Dashboard { dir: dir.clone() },
            Cmd::Gc { dir: dir.clone() },
            Cmd::PruneConflicts {
                dir: dir.clone(),
                older_than_ms: 86_400_000,
                names: vec!["c1".into()],
            },
            Cmd::Invite {
                dir: dir.clone(),
                role: Some("viewer".into()),
                ttl_ms: Some(3_600_000),
            },
            Cmd::Rekey { dir: dir.clone() },
        ];
        // `Update` is deliberately absent: it belongs to the device, and its own
        // test below proves it never takes a folder's gate.
        for cmd in &dispatched {
            let e = entry_for(cmd).expect("a dispatched command must earn an entry");
            assert_eq!(
                e.gate, key,
                "every session command shares its folder's gate"
            );
            assert_eq!(e.dir, key);
            assert!(!e.what.is_empty());
            assert!(!e.subject.is_empty(), "{} had no subject", e.what);
        }
    }

    #[test]
    fn device_work_gets_its_own_gate_and_no_session() {
        let e = entry_for(&Cmd::Supervisor { install: true }).expect("entry");
        assert_eq!(e.gate, DEVICE_GATE);
        assert!(e.dir.is_empty());
        assert_eq!(e.what, "installing");
        let e = entry_for(&Cmd::Supervisor { install: false }).expect("entry");
        assert_eq!(e.what, "removing");
        // A NUL cannot appear in a path, so the device gate cannot collide.
        assert!(!DEVICE_GATE.chars().all(|c| c != '\0'));
    }

    #[test]
    fn two_folders_never_share_a_gate_key() {
        let a = entry_for(&Cmd::Gc {
            dir: PathBuf::from("/one"),
        })
        .expect("entry");
        let b = entry_for(&Cmd::Gc {
            dir: PathBuf::from("/two"),
        })
        .expect("entry");
        assert_ne!(a.gate, b.gate);
    }

    #[test]
    fn a_refusal_keeps_the_whole_diagnosis() {
        let r = IpcResponse {
            ok: false,
            data: Some(serde_json::json!({
                "diagnosis": {
                    "precondition": "LEASE",
                    "hint": "join the waitlist, or ask the holder to unlock",
                    "held_by": "abcdefghijklmnop",
                    "peers": [
                        {"id": "0123456789abcdef", "conn": "Direct", "grade": "Good"},
                        {"id": "fedcba9876543210"}
                    ]
                }
            })),
            error: Some(crate::ipc::IpcErrorBody {
                code: "lease_held".into(),
                message: "a.txt is held by abcdefghij".into(),
            }),
        };
        let d = diagnose("a.txt", &r, "lock refused");
        assert_eq!(d.path, "a.txt");
        assert_eq!(d.precondition, "LEASE");
        assert_eq!(d.message, "a.txt is held by abcdefghij");
        assert!(d.hint.contains("waitlist"));
        assert_eq!(d.held_by.as_deref(), Some("abcdefghij"));
        assert_eq!(d.peers, ["0123456789 · Good · Direct", "fedcba9876"]);
    }

    #[test]
    fn a_refusal_without_a_diagnosis_still_says_something() {
        let r = IpcResponse {
            ok: false,
            data: None,
            error: None,
        };
        let d = diagnose("a.txt", &r, "lock refused");
        assert_eq!(d.message, "lock refused");
        assert!(d.precondition.is_empty());
        assert!(d.peers.is_empty());
        assert!(d.held_by.is_none());
    }

    #[test]
    fn a_rename_is_refused_before_it_can_lose_anything() {
        assert!(check_move("a.txt", "b.txt", true, true, false).is_ok());
        // An empty or unchanged destination is a no-op the user did not mean.
        assert!(check_move("a.txt", "  ", true, true, false).is_err());
        assert!(check_move("a.txt", "a.txt", true, true, false).is_err());
        // Nothing to rename.
        assert!(check_move("a.txt", "b.txt", false, false, false).is_err());
        // Only files: a folder rename is many publishes, not one.
        assert!(check_move("dir", "other", true, false, false).is_err());
        // The destination exists — renaming onto it would delete its bytes.
        let why = check_move("a.txt", "b.txt", true, true, true).expect_err("must refuse");
        assert!(why.contains("already exists"), "{why}");
    }

    #[test]
    fn a_diff_reads_as_an_answer() {
        let body = diff_report(&serde_json::json!({
            "version_tag": "before the edit",
            "changed_pct": 12.5,
            "transfer_bytes": 2048,
            "old_chunks": 8, "new_chunks": 9,
            "identical": 7, "added": 2, "removed": 1, "moved": 0,
            "old_bytes": 4096, "new_bytes": 5120,
        }));
        assert!(body.contains("before the edit"));
        assert!(body.contains("12.5%"));
        assert!(body.contains("8 → 9"));
        let same = diff_report(&serde_json::json!({"identical_content": true}));
        assert!(same.contains("identical content"));
    }

    #[test]
    fn a_doctor_report_names_the_relay_problem() {
        let body = doctor_report(&serde_json::json!({
            "id": "abcdefghijklmnop",
            "mode": "normal",
            "relay_policy": "default",
            "home_relay": "https://relay.example",
            "relay_status": [{"url": "https://relay.example", "connected": false}],
            "lan_discovery": true,
            "bound_sockets": ["0.0.0.0:41234"],
            "known_members": 2,
            "connected_peers": 1,
            "unapplied_count": 3,
            "peers": [{"id": "0123456789abcdef", "conn": "Relayed", "grade": "Poor", "rtt_ms": 180.0}],
        }));
        assert!(body.contains("NOT connected"));
        assert!(body.contains("held back"));
        assert!(body.contains("only via relay"));
        assert!(body.contains("0123456789"));
    }

    #[test]
    fn a_stalled_session_keeps_its_last_good_row() {
        let entry = crate::registry::SessionRef {
            path: "/one".into(),
            kind: SessionKind::Init,
            added_ms: 0,
            paused: true,
        };
        let good = SessionRow {
            path: "/one".into(),
            name: "one".into(),
            running: true,
            paused: false,
            hosted_by_gui: false,
            readable: true,
            role: "editor".into(),
            strict: true,
            files: 12,
            total_bytes: 999,
            conflicts: 1,
            peers_online: 2,
            peers_total: 3,
            id_short: "abcdef".into(),
        };
        let hosted = HashSet::from(["/one".to_string()]);
        let kept = carry_over(&entry, &hosted, Some(&good));
        assert_eq!(kept.files, 12, "a timed-out poll must not blank the row");
        assert_eq!(kept.peers_online, 2);
        assert!(kept.running);
        // The two facts this side owns are still refreshed.
        assert!(kept.paused);
        assert!(kept.hosted_by_gui);

        // With nothing to carry, the row is honest rather than invented.
        let empty = carry_over(&entry, &HashSet::new(), None);
        assert!(!empty.readable);
        assert_eq!(empty.files, 0);
        assert_eq!(empty.name, "one");
    }

    #[test]
    fn the_menu_can_name_the_running_version_before_any_check() {
        let c = clerk();
        assert!(c.read(|s| s.update.current.is_empty()));
        record_running_version(&c);
        assert_eq!(
            c.read(|s| s.update.current.clone()),
            self_update::cargo_crate_version!()
        );
        // Naming the version is not the same as offering one: nothing has been
        // checked, so nothing is on offer and nothing has been applied.
        assert!(!c.read(|s| s.update.available()));
        assert!(c.read(|s| s.update.latest.is_none()));
        assert!(!c.read(|s| s.update.applied));
        assert!(c.read(|s| s.update.checked_at.is_none()));
    }

    /// The build id in `TAZAMUN_VERSION` would break both the comparison and
    /// `available()`, so the running version has to be the bare crate version.
    #[test]
    fn the_running_version_is_the_one_the_updater_compares() {
        let current = self_update::cargo_crate_version!();
        assert!(
            self_update::version::bump_is_greater(current, "99.0.0").unwrap_or(false),
            "the running version must parse as semver"
        );
        assert!(env!("TAZAMUN_VERSION").starts_with(current));
    }

    /// A stuck `busy` is a menu whose items never come back, so it has to clear
    /// on every exit — including the one no call site would have covered.
    #[test]
    fn an_update_never_leaves_the_menu_mid_check() {
        let c = clerk();
        c.write(|s| s.update.error = Some("the last attempt failed".into()));
        {
            let _busy = UpdateBusy::open(&c);
            assert!(c.read(|s| s.update.busy));
            // The stale failure belongs to the previous attempt, not this one.
            assert!(c.read(|s| s.update.error.is_none()));
        }
        assert!(!c.read(|s| s.update.busy));

        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let c2 = c.clone();
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _busy = UpdateBusy::open(&c2);
            panic!("github did something unspeakable");
        }));
        std::panic::set_hook(prev);
        assert!(out.is_err());
        assert!(!c.read(|s| s.update.busy));
    }

    #[test]
    fn an_update_is_device_work_and_never_waits_on_a_folder() {
        let check = entry_for(&Cmd::Update { apply: false }).expect("entry");
        let apply = entry_for(&Cmd::Update { apply: true }).expect("entry");
        for e in [&check, &apply] {
            assert_eq!(e.gate, DEVICE_GATE);
            assert!(e.dir.is_empty(), "an update belongs to no session");
            assert!(!e.what.is_empty());
            assert!(!e.subject.is_empty());
        }
        // One gate for both halves, so a check and an install of the same binary
        // cannot overlap...
        assert_eq!(check.gate, apply.gate);
        assert_ne!(check.what, apply.what);
        // ...and never a folder's, so a wedged session cannot hold one up.
        let folder = entry_for(&Cmd::Gc {
            dir: PathBuf::from("/one"),
        })
        .expect("entry");
        assert_ne!(apply.gate, folder.gate);
    }

    #[test]
    fn a_check_never_installs() {
        // Whatever is on offer, `apply: false` resolves to nothing to install.
        assert_eq!(install_target(false, "0.1.9", Some("0.2.0")), None);
        assert_eq!(install_target(false, "0.1.9", Some("1.0.0")), None);
        assert_eq!(install_target(false, "0.1.9", None), None);
        // An install takes only a strictly newer release: never the one already
        // running, never an older one, never nothing.
        assert_eq!(
            install_target(true, "0.1.9", Some("0.2.0")).as_deref(),
            Some("0.2.0")
        );
        assert_eq!(install_target(true, "0.1.9", Some("0.1.9")), None);
        assert_eq!(install_target(true, "0.1.9", Some("0.1.8")), None);
        assert_eq!(install_target(true, "0.1.9", None), None);
        // A tag neither side can parse is not an update.
        assert_eq!(install_target(true, "0.1.9", Some("nightly")), None);
    }

    #[test]
    fn a_stable_machine_is_never_offered_a_prerelease() {
        let list = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let mixed = list(&["0.2.0-beta.2", "0.2.0-beta.1", "0.1.9", "0.1.8"]);
        assert_eq!(newest_release("stable", &mixed).as_deref(), Some("0.1.9"));
        assert_eq!(
            newest_release("beta", &mixed).as_deref(),
            Some("0.2.0-beta.2")
        );
        // The tag may still carry its `v`, and build metadata is not a
        // prerelease — its hyphen must not hide a stable release.
        let tagged = list(&["v0.2.0+ci-7", "v0.1.9"]);
        assert_eq!(
            newest_release("stable", &tagged).as_deref(),
            Some("0.2.0+ci-7")
        );
        // A repository with nothing to offer is an answer, not a failure.
        assert_eq!(newest_release("stable", &[]), None);
        assert_eq!(newest_release("stable", &list(&["0.2.0-rc.1"])), None);
    }

    #[test]
    fn a_package_manager_install_is_not_self_replaceable() {
        // The exact layout a real npm-on-Windows install reports.
        let npm_win = Path::new(
            r"C:\Users\cc1a2b\AppData\Roaming\npm\node_modules\tazamun\node_modules\.bin_real\tazamun.exe",
        );
        assert_eq!(
            managed_by_path(npm_win),
            Some(("npm", "npm update -g tazamun"))
        );
        assert_eq!(
            managed_by_path(Path::new("/usr/local/lib/node_modules/tazamun/bin/tazamun")),
            Some(("npm", "npm update -g tazamun"))
        );
        for brew in [
            "/opt/homebrew/Cellar/tazamun/0.1.2/bin/tazamun",
            "/home/linuxbrew/.linuxbrew/Cellar/tazamun/0.1.2/bin/tazamun",
        ] {
            assert_eq!(
                managed_by_path(Path::new(brew)),
                Some(("Homebrew", "brew upgrade tazamun"))
            );
        }
        // A plain install is the self-updater's home turf and must not be
        // refused — a false positive here leaves a user with no way to update
        // at all.
        for plain in [
            "/usr/local/bin/tazamun",
            "/home/cc1a2b/.cargo/bin/tazamun",
            r"C:\Program Files\tazamun\tazamun.exe",
        ] {
            assert_eq!(managed_by_path(Path::new(plain)), None, "{plain}");
        }
    }

    #[test]
    fn each_update_failure_tells_the_user_something_different() {
        let offline = update_failure(
            "ReqwestError: error sending request for url \
             (https://api.github.com/repos/cc1a2b/tazamun/releases): dns error: failed to lookup \
             address information",
            false,
        );
        let limited = update_failure(
            "NetworkError: api request failed with status: 403 - for: \
             \"https://api.github.com/repos/cc1a2b/tazamun/releases\"",
            false,
        );
        let private = update_failure(
            "NetworkError: api request failed with status: 404 - for: \
             \"https://api.github.com/repos/cc1a2b/tazamun/releases/latest\"",
            false,
        );
        let no_asset = update_failure(
            "ReleaseError: No asset found for target: `aarch64-unknown-linux-musl`",
            false,
        );
        let denied = update_failure("IoError: Permission denied (os error 13)", false);
        let unknown = update_failure("ZipError: invalid Zip archive", false);

        assert!(offline.contains("offline"), "{offline}");
        assert!(limited.contains("rate-limiting"), "{limited}");
        assert!(limited.contains("GITHUB_TOKEN"), "{limited}");
        assert!(private.contains("private"), "{private}");
        assert!(
            no_asset.contains("no build for this platform"),
            "{no_asset}"
        );
        assert!(denied.contains("could not be replaced"), "{denied}");
        assert!(denied.contains("administrator or root"), "{denied}");
        assert!(unknown.contains("invalid Zip archive"), "{unknown}");

        // Six different next moves, none of them mistakable for another at a
        // glance.
        let all = [&offline, &limited, &private, &no_asset, &denied, &unknown];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
            // None of them may open with a second "the update failed": the view
            // already introduces `UpdateState.error` with one.
            assert!(!a.starts_with("the update failed"), "{a}");
        }
        // Every one keeps the raw text a maintainer would need.
        assert!(offline.contains("dns error"), "{offline}");
        assert!(limited.contains("status: 403"), "{limited}");
        assert!(private.contains("status: 404"), "{private}");
        assert!(
            no_asset.contains("aarch64-unknown-linux-musl"),
            "{no_asset}"
        );
        assert!(denied.contains("os error 13"), "{denied}");

        // With a token in hand a 404 is not "the repository is private"; that
        // advice would send the user to fetch the token they already have.
        let with_token = update_failure(
            "NetworkError: api request failed with status: 404 - for: \"https://api.github.com/x\"",
            true,
        );
        assert!(!with_token.contains("private"), "{with_token}");
    }

    #[test]
    fn a_poll_is_far_shorter_than_an_edit() {
        // A background read must not be able to outlast a refresh beat by much;
        // the 30 s IPC default is what froze the window for half a minute.
        assert!(POLL_TIMEOUT < Duration::from_secs(30));
        assert!(POLL_SLOW < POLL_TIMEOUT);
        assert!(REFRESH < POLL_TIMEOUT);
        // Teardown has to finish inside the window's own budget.
        assert!(QUIT_DRAIN < GUI_SHUTDOWN);
    }
}
