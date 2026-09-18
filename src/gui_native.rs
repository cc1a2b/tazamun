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
mod menubar;
mod model;
mod onboarding;
mod ornament;
mod prefs;
mod register;
mod rhythm;
mod selection;
mod shortcuts;
mod statusbar;
mod sysopen;
mod telemetry;
mod theme;
mod toasts;
mod worker;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use egui::containers::{CentralPanel, Panel};
use tokio::sync::mpsc;

use crate::cli::CliError;
use crate::daemon::DaemonHandle;
use model::*;
use worker::worker;

const REFRESH: Duration = Duration::from_millis(1500);

/// Seconds between preference writes while something keeps changing.
const PREFS_DEBOUNCE: f64 = 1.5;

/// How long the peer sky takes to draw itself on.
const SKY_REVEAL: f64 = 0.5;
/// Upper bound on any single graceful-shutdown await, so a wedged actor cannot
/// freeze the worker (on stop) or hang process exit (on teardown).
const GUI_SHUTDOWN: Duration = Duration::from_secs(10);
/// Most version entries the History register assembles. Past this the view says
/// so, rather than silently rendering a prefix as if it were the whole ledger.
const HISTORY_MAX: usize = 200;
/// The same promise for the Audit ledger.
const AUDIT_MAX: usize = 200;
/// Room the session header's own verbs claim at 100% text, so the folder path
/// beside them truncates instead of running under them.
const HEADER_VERBS_W: f32 = 420.0;
/// Width the ⌘K key claims at the title bar's trailing edge. The menu bar is
/// laid out before it, so it has to be told what is coming.
const PALETTE_KEY_W: f32 = 52.0;
/// How long a typed pattern must settle before it is sent. The query runs on
/// the daemon's single actor, so a round trip per keystroke would put the whole
/// session behind the user's typing.
const SEARCH_DEBOUNCE: f64 = 0.12;
/// How long a command may run before the window remarks on it. Below this a
/// note would be noise; above it, silence reads as a hang.
const SLOW_AFTER: f64 = 4.0;

/// One line of the Files register. Groups, files and the versions of an opened
/// file share one row stream so every line is the same height and the whole
/// register stays virtualised.
enum FileLine<'a> {
    Group(&'a grouping::Group),
    /// The file, and its folio — its ordinal among the files on screen, which
    /// is not its row index because groups and versions share the row stream.
    File(&'a FileRow, usize),
    Version(&'a str, &'a VersionRow),
}

/// Per-peer figures sampled once, before the register draws, so the row
/// closure needs no borrow of the telemetry store.
struct PeerRow {
    rtt: Option<u64>,
    series: Vec<f32>,
    lit: u8,
}

/// What the file row's context menu asked for.
enum FileMenu {
    Rename,
    Diff,
    CopyPath,
}

/// Whether a ledger entry survives the Audit tab's filters. Pure so the
/// matching rule is testable without a window: a filter that quietly drops
/// entries is indistinguishable on screen from a ledger that never had them.
fn audit_matches(a: &AuditRow, kind: Option<&str>, needle: &str) -> bool {
    if kind.is_some_and(|k| a.kind != k) {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    let hit = |f: &Option<String>| {
        f.as_deref()
            .is_some_and(|v| v.to_lowercase().contains(needle))
    };
    hit(&a.path) || hit(&a.peer) || hit(&a.detail)
}

/// The preserved copies older than `older_than_ms`, and what deleting them
/// would reclaim. Pure, because the user confirms against these exact numbers
/// and an off-by-one here deletes bytes nobody agreed to.
fn prunable(rows: &[ConflictRow], now_ms: u64, older_than_ms: u64) -> (Vec<String>, u64) {
    let doomed = rows
        .iter()
        .filter(|c| now_ms.saturating_sub(c.ts_ms) >= older_than_ms);
    let mut names = Vec::new();
    let mut bytes = 0u64;
    for c in doomed {
        names.push(c.name.clone());
        bytes = bytes.saturating_add(c.size);
    }
    (names, bytes)
}

/// The frame's clock, read through an entry's own context.
fn ui_time(e: &register::Entry<'_>) -> f64 {
    e.response().ctx.input(|i| i.time)
}

/// One line of the History register.
enum HistoryLine<'a> {
    Day(String),
    Version(&'a String, &'a VersionRow),
}

/// Who holds a file, in the palette's custody vocabulary. A stopped session
/// cannot grant anything, so its files read as refused rather than free.
fn file_custody(f: &FileRow, running: bool) -> theme::Custody {
    if !running {
        return theme::Custody::Blocked;
    }
    match (&f.locked_by, f.mine_lock) {
        (Some(_), true) => theme::Custody::Mine,
        (Some(_), false) => theme::Custody::Peer,
        (None, _) => theme::Custody::Free,
    }
}

/// One option of a small exclusive choice. The selected option is stated in
/// ink on a gold wash rather than by a stock `selectable_label`, which was the
/// last piece of unstyled egui left in the window.
fn choice(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let text = egui::RichText::new(label)
        .font(theme::font(theme::step::META, theme::fam_medium()))
        .color(if selected {
            theme::ink()
        } else {
            theme::ink_muted()
        });
    let fill = if selected {
        theme::wash::of(theme::gold(), theme::wash::SELECT)
    } else {
        egui::Color32::TRANSPARENT
    };
    let stroke = egui::Stroke::new(
        theme::RULE_W,
        if selected {
            theme::gold()
        } else {
            theme::rule_divider()
        },
    );
    let r = ui.add(
        egui::Button::new(text)
            .fill(fill)
            .stroke(stroke)
            .corner_radius(theme::R_CONTROL),
    );
    a11y::label_selectable(&r, label, selected);
    if r.has_focus() {
        register::focus_ring(ui.painter(), r.rect);
    }
    r.clicked()
}

/// Whether this session's role may take a lease at all. The daemon refuses
/// every lock, unlock, restore and conflict-apply on a viewer or archive
/// folder, so the interface must disable those verbs rather than offer them and
/// let the refusal arrive as an error.
fn role_can_edit(role: &str) -> bool {
    !matches!(role, "viewer" | "archive")
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
    /// Which preserved copy the conflict detail pane is weighing.
    conflict_sel: usize,
    /// Whether the Overview's invite disclosure is open.
    invite_open: bool,
    /// The path being renamed, and the buffer holding its new name.
    rename: Option<(String, String)>,
    /// Whether the rename field has already been handed focus. Re-requesting it
    /// every frame pins the keyboard and makes Tab dead inside the dialog.
    rename_focused: bool,
    /// The same, for the command palette's query field.
    palette_focused: bool,
    /// Age cutoff the conflict prune control is set to.
    prune_age_ms: u64,
    /// Free-text filter over the audit ledger.
    audit_query: String,
    /// Kind the audit ledger is filtered to, or every kind.
    audit_kind: Option<String>,
    /// Role the next minted invite is scoped to.
    invite_role: String,
    /// Expiry of the next minted invite; `None` never expires.
    invite_ttl_ms: Option<u64>,
    /// The last refused edit, held until the user clears it or one succeeds.
    refusal: Option<Refusal>,
    /// Commands the worker has accepted and not finished, mirrored each frame.
    inflight: Vec<InFlight>,
    /// Per-session reachability of the poll itself.
    reach: BTreeMap<String, Reach>,
    /// When each session last answered, for the staleness line.
    fetched_at: BTreeMap<String, f64>,
    /// A long answer — a diff, a doctor report — waiting to be read.
    report: Option<Report>,
    /// The daemon's answer to the current file query.
    file_page: Option<FilePage>,
    /// Version and update status, mirrored for the menu bar.
    update: UpdateState,
    /// The file the Files register has marked. The bar's Rename acts on it, so
    /// the verb is reachable without hunting for the row's context menu.
    marked_file: Option<String>,
    /// The query already asked, so typing does not re-ask it every frame.
    file_query: Option<(String, String, bool, usize)>,
    /// Which page of a server-side file query is on screen.
    file_offset: usize,
    /// When the pending file query may go out. Typing moves it forward; a sort
    /// or page change fires immediately.
    file_query_due: Option<f64>,
    telemetry: telemetry::TelemetryStore,
    seen_tick: u64,
    toasts: toasts::Queue,
    shortcuts_open: bool,
    text_scale: f32,
    /// Appearance, owned here and pushed into `theme`'s globals by
    /// [`App::apply_style`]. Held rather than read back out of `theme` so the
    /// preference file has one source of truth.
    mode: theme::Mode,
    density: theme::Density,
    reduced_motion: bool,
    /// Set whenever any of the four appearance values changes; consumed on the
    /// next frame, because `theme::install` also writes the text table and a
    /// style pushed at construction would be overwritten by it.
    style_dirty: bool,
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
            conflict_sel: 0,
            invite_open: false,
            rename: None,
            rename_focused: false,
            palette_focused: false,
            prune_age_ms: copy::PRUNE_AGE_DEFAULT,
            audit_query: String::new(),
            audit_kind: None,
            invite_role: copy::INVITE_ROLE_DEFAULT.to_string(),
            invite_ttl_ms: None,
            refusal: None,
            inflight: Vec::new(),
            reach: BTreeMap::new(),
            fetched_at: BTreeMap::new(),
            report: None,
            file_page: None,
            update: UpdateState::default(),
            marked_file: None,
            file_query: None,
            file_offset: 0,
            file_query_due: None,
            telemetry: telemetry::TelemetryStore::default(),
            seen_tick: 0,
            toasts: toasts::Queue::default(),
            shortcuts_open: false,
            text_scale: saved.text_scale,
            // The capture hook may override the appearance so a docs shot is
            // reproducible whatever the last run left in prefs — same reason
            // the tab and session are overridable.
            mode: theme::Mode::from_key(
                &std::env::var("TAZAMUN_GUI_SHOT_MODE").unwrap_or_else(|_| saved.mode.clone()),
            ),
            density: theme::Density::from_key(
                &std::env::var("TAZAMUN_GUI_SHOT_DENSITY")
                    .unwrap_or_else(|_| saved.density.clone()),
            ),
            reduced_motion: saved.reduced_motion,
            style_dirty: true,
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
            mode: self.mode.key().to_string(),
            density: self.density.key().to_string(),
            reduced_motion: self.reduced_motion,
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

    /// What the menu bar reads this frame. `reserve` is how much of the bar's
    /// trailing edge the window buttons and the palette key will take: they are
    /// laid out right-to-left *after* the bar, so `available_width` still counts
    /// their strip and the bar would otherwise run under them.
    fn bar_state<'a>(
        &'a self,
        overview: &'a Option<Overview>,
        reserve: f32,
    ) -> menubar::BarState<'a> {
        let row = self.selected.as_ref().and_then(|sel| {
            overview
                .as_ref()
                .and_then(|o| o.sessions.iter().find(|r| &r.path == sel))
        });
        menubar::BarState {
            session: row.map(|r| menubar::SessionState {
                running: r.running,
                paused: r.paused,
                role: r.role.as_str(),
                may_edit: role_can_edit(&r.role),
                conflicts: r.conflicts,
                marked_file: self.marked_file.as_deref(),
            }),
            update: &self.update,
            supervisor: overview.as_ref().is_some_and(|o| o.supervisor),
            mode: self.mode,
            density: self.density,
            reduced_motion: self.reduced_motion,
            text_scale: self.text_scale,
            reserve,
        }
    }

    /// Carries out what the menu bar was asked for.
    fn run_menu(&mut self, action: menubar::MenuAction, ui: &egui::Ui) {
        use menubar::MenuAction as M;
        let dir = self.selected.as_deref().map(PathBuf::from);
        match action {
            M::Start => {
                if let Some(d) = dir {
                    self.send(Cmd::Start(d));
                }
            }
            M::Stop => {
                if let Some(d) = dir {
                    self.send(Cmd::Stop(d));
                }
            }
            M::Pause => {
                if let Some(d) = dir {
                    self.send(Cmd::Pause(d));
                }
            }
            M::Resume => {
                if let Some(d) = dir {
                    self.send(Cmd::Resume(d));
                }
            }
            M::OpenFolder => {
                if let Some(sel) = &self.selected
                    && let Err(e) = sysopen::open_folder(Path::new(sel))
                {
                    self.toasts.push(
                        format!("could not open the file manager: {e}"),
                        toasts::Kind::Bad,
                        ui.input(|i| i.time),
                    );
                }
            }
            M::CopyPath => {
                if let Some(sel) = self.selected.clone() {
                    ui.ctx().copy_text(sel);
                }
            }
            M::Rename => {
                if let Some(f) = self.marked_file.clone() {
                    self.rename = Some((f.clone(), f));
                }
            }
            M::Doctor => {
                if let Some(d) = dir {
                    self.send(Cmd::Doctor { dir: d });
                }
            }
            M::Dashboard => {
                if let Some(d) = dir {
                    self.send(Cmd::Dashboard { dir: d });
                }
            }
            M::Gc => {
                if let Some(d) = dir {
                    self.send(Cmd::Gc { dir: d });
                }
            }
            M::Rekey => {
                if let Some(d) = dir {
                    self.ask(
                        copy::REKEY_TITLE,
                        copy::REKEY_BODY.to_string(),
                        copy::REKEY_VERB,
                        true,
                        Cmd::Rekey { dir: d },
                    );
                }
            }
            // The Conflicts tab owns the cutoff and the count, and the confirm
            // has to name exactly what it will delete — so the bar takes the
            // user there rather than guessing on their behalf.
            M::Prune => self.tab = Tab::Conflicts,
            M::Supervisor { install } => {
                if install {
                    self.send(Cmd::Supervisor { install: true });
                } else {
                    self.ask(
                        copy::SUPERVISOR_REMOVE_TITLE,
                        copy::SUPERVISOR_REMOVE_BODY.to_string(),
                        copy::SUPERVISOR_REMOVE,
                        true,
                        Cmd::Supervisor { install: false },
                    );
                }
            }
            M::Palette(m) => {
                if m != self.mode {
                    self.mode = m;
                    self.style_dirty = true;
                }
            }
            M::Density(d) => {
                if d != self.density {
                    self.density = d;
                    self.style_dirty = true;
                }
            }
            M::Motion { reduced } => {
                if reduced != self.reduced_motion {
                    self.reduced_motion = reduced;
                    self.style_dirty = true;
                }
            }
            M::TextScale(v) => {
                if (v - self.text_scale).abs() > f32::EPSILON {
                    self.text_scale = v;
                    self.style_dirty = true;
                }
            }
            M::Shortcuts => {
                self.shortcuts_open = true;
                self.palette_open = false;
                self.palette_focused = false;
            }
            M::Colophon => self.colophon_open = true,
            M::CheckUpdate => self.send(Cmd::Update { apply: false }),
            M::ApplyUpdate => self.send(Cmd::Update { apply: true }),
        }
    }

    /// Pushes the four appearance values into `theme` and rebuilds the style.
    fn apply_style(&self, ctx: &egui::Context) {
        theme::set_mode(self.mode);
        theme::set_density(self.density);
        theme::set_reduced_motion(self.reduced_motion);
        // `a11y` owns the clamping and ends in `theme::restyle`, so this is the
        // one call that lands all four.
        a11y::apply_text_scale(ctx, self.text_scale);
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
        if std::mem::take(&mut self.style_dirty) {
            self.apply_style(ui.ctx());
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
            // Mirrored rather than moved: the worker overwrites it on the next
            // refusal, and the view clears it when the user acts on it.
            self.refusal = s.refusal.clone();
            self.inflight = s.inflight.clone();
            self.reach = s.reach.clone();
            self.fetched_at = s.fetched_at.clone();
            if s.report.is_some() {
                self.report = s.report.take();
            }
            self.file_page = s.file_page.clone();
            self.update = s.update.clone();
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
            .default_size(statusbar::strip_h())
            .show(ui, |ui| {
                let ov = overview.as_ref();
                let note = self.selected.as_deref().map(base_name);
                let version = ov.map(|o| format!("v{}", o.version));
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
                        version: version.as_deref(),
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
                        left: theme::space::L as i8,
                        right: theme::space::M as i8,
                        top: theme::space::L as i8,
                        bottom: theme::space::M as i8,
                    })
                    .show(ui, |ui| self.sidebar(ui, &overview));
            });
        CentralPanel::default()
            // The page margin, from the scale: a register is read across its
            // full width, so the gutter is the only thing holding it off the
            // window edge.
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: theme::space::XXL as i8,
                right: theme::space::XXL as i8,
                top: theme::space::L as i8,
                bottom: theme::space::L as i8,
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
        self.report_overlay(ui);
        self.rename_overlay(ui);
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
        // Taken out of the layout closure and run after it: acting on a menu
        // choice mutates the view, and the closure already holds it.
        let mut chosen: Option<menubar::MenuAction> = None;
        let bar = ui.max_rect();
        chrome::titlebar_interactions(ui, bar);
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: theme::space::XL as i8,
                right: theme::space::M as i8,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.set_min_height(bar.height());
                ui.style_mut().interaction.selectable_labels = false;
                ui.horizontal_centered(|ui| {
                    chrome::wordmark(ui, theme::sized(theme::step::DISPLAY));
                    if let Some(ov) = overview {
                        ui.add_space(8.0);
                        let running = ov.sessions.iter().filter(|s| s.running).count();
                        let conflicts: usize = ov.sessions.iter().map(|s| s.conflicts).sum();
                        let n = ov.sessions.len();
                        register::tag(
                            ui,
                            &format!("{n} session{}", if n == 1 { "" } else { "s" }),
                            theme::ink_muted(),
                        );
                        if running > 0 {
                            register::tag(ui, &format!("{running} running"), theme::custody_good());
                        }
                        if conflicts > 0 {
                            register::tag(
                                ui,
                                &figures::count(conflicts, "conflict"),
                                theme::custody_stale(),
                            );
                        }
                    }
                    ui.add_space(theme::space::L);
                    let reserve =
                        chrome::window_buttons_width(ui.spacing().item_spacing.x) + PALETTE_KEY_W;
                    chosen = menubar::bar(ui, &self.bar_state(overview, reserve));
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
                                egui::RichText::new("⌘K")
                                    .size(11.0)
                                    .color(theme::ink_muted()),
                            ))
                            .on_hover_text("command palette (Ctrl+K)");
                        if r.clicked() {
                            self.palette_open = true;
                            self.palette_query.clear();
                            self.palette_sel = 0;
                        }
                        if busy {
                            ceremony::loading_mark(ui, theme::sized(theme::step::LABEL));
                        }
                    });
                });
            });
        if let Some(action) = chosen {
            self.run_menu(action, ui);
        }
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
                    .color(if home_selected {
                        theme::gold()
                    } else {
                        theme::ink()
                    }),
            );
            ui.label(
                egui::RichText::new(copy::SIDEBAR_HOME_SUB)
                    .size(11.0)
                    .color(theme::ink_muted()),
            );
        });
        if home_hit {
            self.multi.clear();
            self.select(None);
        }
        ui.add_space(4.0);
        register::heading(ui, copy::SESSIONS_SECTION);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(ov) = overview else {
                    for w in [180.0, 140.0, 160.0] {
                        register::skeleton_line(ui, w);
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
                    ui.label(
                        egui::RichText::new(copy::NO_SESSIONS_TITLE).color(theme::ink_muted()),
                    );
                    ui.label(
                        egui::RichText::new(copy::NO_SESSIONS_HINT)
                            .color(theme::ink_faint())
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
                // The version moved to the status strip, which never scrolls;
                // what stays here is the one fact about this machine that the
                // foot has no room for.
                if ov.supervisor {
                    ui.add_space(theme::space::M);
                    ornament::rule_with_diamond(ui, theme::gold());
                    ui.horizontal(|ui| {
                        let side = theme::sized(theme::step::META);
                        let (mark, _) =
                            ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                        ornament::khatam(
                            ui.painter(),
                            mark.center(),
                            side * 0.4,
                            theme::gold(),
                            true,
                        );
                        ui.label(
                            egui::RichText::new(copy::SUPERVISOR_ON)
                                .font(theme::font(
                                    theme::step::META,
                                    egui::FontFamily::Proportional,
                                ))
                                .color(theme::custody_good()),
                        );
                    });
                }
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
                let fill = theme::mix(egui::Color32::TRANSPARENT, theme::bg_raise(), t * 0.9);
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(theme::R_NONE)
                    .stroke(if selected {
                        egui::Stroke::new(1.0, theme::gold().linear_multiply(0.35))
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
                theme::gold(),
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
                register::status_mark(ui, dot_color);
                ui.label(
                    egui::RichText::new(&s.name)
                        .family(theme::fam_medium())
                        .size(13.5)
                        .color(if selected {
                            theme::gold()
                        } else {
                            theme::ink()
                        }),
                );
                if s.conflicts > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        register::tag(ui, &format!("{}", s.conflicts), theme::custody_stale());
                    });
                }
            });
            ui.label(
                egui::RichText::new(status_text(s))
                    .size(11.0)
                    .color(theme::ink_muted()),
            );
            let mut meta = format!("{} · {}", s.id_short, human_bytes(s.total_bytes));
            if s.hosted_by_gui {
                meta.push_str(" · hosted here");
            }
            ui.label(
                egui::RichText::new(meta)
                    .size(10.0)
                    .color(theme::ink_faint()),
            );
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
                    theme::gold().linear_multiply(0.5),
                );
                ui.add_space(4.0);
                match overview {
                    Some(ov) if ov.sessions.is_empty() => self.first_light(ui),
                    Some(ov) => {
                        ui.label(
                            egui::RichText::new(copy::HOME_TITLE)
                                .family(theme::fam_semibold())
                                .size(18.0)
                                .color(theme::ink()),
                        );
                        ui.label(
                            egui::RichText::new(copy::HOME_SUB)
                                .size(12.0)
                                .color(theme::ink_muted()),
                        );
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
                                    .color(theme::custody_stale()),
                            );
                        }
                        ui.add_space(12.0);
                        register::heading(ui, copy::CREATE_TITLE);
                        components::notched_card(ui, Some(theme::gold()), |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(copy::CREATE_HINT)
                                    .size(11.5)
                                    .color(theme::ink_muted()),
                            );
                            self.create_form_row(ui);
                        });
                        ui.add_space(6.0);
                        register::heading(ui, copy::JOIN_TITLE);
                        components::notched_card(ui, None, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(copy::JOIN_HINT)
                                    .size(11.5)
                                    .color(theme::ink_muted()),
                            );
                            self.join_form_row(ui);
                        });
                    }
                    None => {
                        ui.horizontal(|ui| {
                            for w in [96.0, 96.0, 96.0] {
                                register::skeleton_line(ui, w);
                            }
                        });
                    }
                }
                // Outside the match on purpose: text size is a property of the
                // window, and the reader who most needs it is the first-timer
                // on the zero-session page.
                ui.add_space(10.0);
                rhythm::space(ui, 3);
                rhythm::foot_rule(ui);
            });
    }

    /// The zero-session opening page: three medallioned steps joined by a
    /// strapwork thread, the real create/join forms living inside step one.
    fn first_light(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(copy::FL_TITLE)
                .font(theme::font(theme::step::DISPLAY, theme::fam_serif()))
                .color(theme::ink()),
        );
        ui.label(
            egui::RichText::new(copy::FL_SUB)
                .font(theme::font(
                    theme::step::BODY,
                    egui::FontFamily::Proportional,
                ))
                .color(theme::ink_muted()),
        );
        ui.add_space(theme::space::L);
        onboarding::first_light_frame(ui, |ui| {
            ui.set_width(ui.available_width());
            // One row per step, each numeral beside the words it belongs to.
            // The two used to be separate columns joined by a connector whose
            // length was a guess at how tall the words were; the guess was
            // wrong and steps two and three floated above their own numerals.
            let mut marks: Vec<egui::Rect> = Vec::new();
            marks.push(self.first_light_step(
                ui,
                1,
                onboarding::StepState::Active,
                copy::FL_STEP1_TITLE,
                copy::FL_STEP1_HINT,
                |s, ui| {
                    ui.add_space(theme::space::S);
                    s.create_form_row(ui);
                    s.join_form_row(ui);
                },
            ));
            marks.push(self.first_light_step(
                ui,
                2,
                onboarding::StepState::Future,
                copy::FL_STEP2_TITLE,
                copy::FL_STEP2_HINT,
                |_, _| {},
            ));
            marks.push(self.first_light_step(
                ui,
                3,
                onboarding::StepState::Future,
                copy::FL_STEP3_TITLE,
                copy::FL_STEP3_HINT,
                |_, _| {},
            ));
            // Threaded after the fact, between rects the layout actually
            // produced, so the line cannot disagree with the numerals.
            let p = ui.painter();
            for pair in marks.windows(2) {
                onboarding::thread(p, pair[0], pair[1]);
            }
        });
    }

    /// One step of first light: its numeral, its words, and whatever it asks
    /// the reader to do. Returns the numeral's rect so the caller can thread
    /// the steps together.
    fn first_light_step(
        &mut self,
        ui: &mut egui::Ui,
        n: u8,
        state: onboarding::StepState,
        title: &str,
        hint: &str,
        body: impl FnOnce(&mut Self, &mut egui::Ui),
    ) -> egui::Rect {
        let future = matches!(state, onboarding::StepState::Future);
        let mut mark = egui::Rect::NOTHING;
        ui.horizontal_top(|ui| {
            mark = onboarding::medallion(ui, n, state);
            ui.add_space(theme::space::L);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(title)
                        .font(theme::font(theme::step::TITLE, theme::fam_semibold()))
                        .color(if future {
                            theme::ink_faint()
                        } else {
                            theme::ink()
                        }),
                );
                ui.label(
                    egui::RichText::new(hint)
                        .font(theme::font(
                            theme::step::BODY,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
                body(self, ui);
            });
        });
        ui.add_space(theme::space::XL);
        mark
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
            register::status_mark(
                ui,
                if running {
                    theme::custody_good()
                } else {
                    theme::ink_muted()
                },
            );
            ui.label(
                egui::RichText::new(base_name(&sel))
                    .font(theme::font(theme::step::DISPLAY, theme::fam_serif()))
                    .color(theme::ink()),
            );
            // Truncated rather than wrapped: the verbs to its right are laid
            // out afterwards, so a path allowed to take its natural width runs
            // straight under them at a large text scale. The whole path is on
            // the row's tooltip and in Copy path.
            let avail = (ui.available_width() - HEADER_VERBS_W * theme::scale()).max(0.0);
            let mut job = egui::text::LayoutJob::single_section(
                sel.clone(),
                egui::TextFormat {
                    font_id: theme::font(theme::step::DATA, theme::fam_mono()),
                    color: theme::ink_faint(),
                    ..Default::default()
                },
            );
            job.wrap = egui::text::TextWrapping {
                max_width: avail,
                max_rows: 1,
                break_anywhere: true,
                ..Default::default()
            };
            ui.label(job).on_hover_text(&sel);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if controls::ghost_button(ui, "Stop").clicked() {
                        self.send(Cmd::Stop(dir.clone()));
                    }
                } else if components::bevel_primary(ui, "Start").clicked() {
                    self.send(Cmd::Start(dir.clone()));
                }
                // Only the verb that applies: a session is either paused or
                // it is not, and showing both made one of them a dead control.
                // Only the lifecycle verb lives here now. Pause, the folder,
                // the path and the tools all moved to the `session` and `tools`
                // menus, which carry them with their keyboard routes and their
                // disabled reasons — five ghost buttons and a title cannot
                // share one row once the reader scales the text up, and they
                // collided.
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
                                .color(if selected {
                                    theme::ink()
                                } else {
                                    theme::ink_muted()
                                }),
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
            let x = ui.ctx().animate_value_with_time(
                egui::Id::new("tab-underline-x"),
                r.left(),
                theme::dur(theme::motion::VIEW),
            );
            let w = ui.ctx().animate_value_with_time(
                egui::Id::new("tab-underline-w"),
                r.width(),
                theme::dur(theme::motion::VIEW),
            );
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, bar_bottom + 4.0), egui::vec2(w, 2.0)),
                1.0,
                theme::gold(),
            );
        }
        ui.add_space(8.0);

        let Some(d) = detail else {
            self.tab_waiting(ui);
            return;
        };
        // A failed load is stated once, at the top, with the way out. The tabs
        // below then render their own `Content::Failed` rather than an empty
        // state, so the window never reports "nothing here" about data it did
        // not manage to read.
        if let Some(err) = &d.error {
            register::notice(ui, theme::Custody::Blocked, |ui| {
                ui.label(
                    egui::RichText::new(copy::SESSION_LOAD_FAILED)
                        .font(theme::font(theme::step::BODY, theme::fam_semibold()))
                        .color(theme::ink()),
                );
                ui.add_space(theme::space::XS);
                ui.label(
                    egui::RichText::new(err)
                        .font(theme::font(theme::step::META, theme::fam_mono()))
                        .color(theme::ink_muted()),
                );
                ui.add_space(theme::space::S);
                if controls::ghost_small(ui, copy::ACTION_RETRY).clicked() {
                    self.send(Cmd::Refresh);
                }
            });
            ui.add_space(theme::space::M);
        }

        let now = ui.input(|i| i.time);
        self.staleness_line(ui, &d.dir, now);
        self.working_band(ui, &d.dir, now);
        self.refusal_notice(ui, d);

        // Each tab owns its own scrolling: the register virtualises through its
        // own `ScrollArea`, and nesting that inside a second one would give the
        // tabular views two scrollbars and no virtualisation.
        match self.tab {
            Tab::Overview => self.scrolled(ui, "overview", |s, ui| s.tab_overview(ui, d)),
            Tab::Peers => self.tab_peers(ui, d),
            Tab::Files => self.tab_files(ui, &dir, d),
            Tab::Conflicts => self.tab_conflicts(ui, &dir, d),
            Tab::History => self.tab_history(ui, &dir, d),
            Tab::Audit => self.tab_audit(ui, d),
            Tab::Settings => self.scrolled(ui, "settings", |s, ui| s.tab_settings(ui, &dir, d)),
        }
    }

    /// The refused-edit panel: which of the three lease preconditions blocked
    /// the edit, why that rule exists, and what clears it. This is the whole
    /// product in one panel, and it used to be a four-second toast carrying
    /// only the daemon's one-line message.
    fn refusal_notice(&mut self, ui: &mut egui::Ui, d: &Detail) {
        let Some(r) = self.refusal.clone() else {
            return;
        };
        let mut clear = false;
        register::notice(ui, theme::Custody::Blocked, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(copy::precondition_title(&r.precondition))
                        .font(theme::font(theme::step::TITLE, theme::fam_serif()))
                        .color(theme::ink()),
                );
                if !r.precondition.is_empty() {
                    register::tag(ui, &r.precondition, theme::custody_blocked());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if controls::ghost_small(ui, copy::REFUSED_DISMISS).clicked() {
                        clear = true;
                    }
                });
            });
            ui.add_space(theme::space::XS);
            ui.label(
                egui::RichText::new(&r.path)
                    .font(theme::font(theme::step::DATA, theme::fam_mono()))
                    .color(theme::ink_muted()),
            );
            ui.add_space(theme::space::S);
            ui.label(
                egui::RichText::new(&r.message)
                    .font(theme::font(theme::step::BODY, theme::fam_medium()))
                    .color(theme::ink()),
            );
            ui.add_space(theme::space::XS);
            ui.label(
                egui::RichText::new(copy::precondition_why(&r.precondition))
                    .font(theme::font(
                        theme::step::BODY,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_muted()),
            );
            if let Some(h) = &r.held_by {
                ui.add_space(theme::space::S);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(copy::REFUSED_HELD_BY)
                            .font(theme::font(
                                theme::step::META,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink_faint()),
                    );
                    register::tag(ui, h, theme::custody_peer());
                });
            }
            if !r.peers.is_empty() {
                ui.add_space(theme::space::S);
                ui.label(
                    egui::RichText::new(copy::REFUSED_PEERS)
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_faint()),
                );
                for line in &r.peers {
                    ui.label(
                        egui::RichText::new(line)
                            .font(theme::font(theme::step::DATA, theme::fam_mono()))
                            .color(theme::ink_muted()),
                    );
                }
            }
            if !r.hint.is_empty() {
                ui.add_space(theme::space::S);
                ui.label(
                    egui::RichText::new(&r.hint)
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::custody_stale()),
                );
            }
            // The refusal names a remedy the user can act on from here: a
            // FRESHNESS block clears itself once the pull finishes, and a
            // REACHABILITY block is answered on the Peers tab.
            ui.add_space(theme::space::M);
            ui.horizontal(|ui| match r.precondition.as_str() {
                "REACHABILITY" => {
                    if controls::ghost_small(ui, copy::REFUSED_SEE_PEERS).clicked() {
                        self.tab = Tab::Peers;
                        clear = true;
                    }
                }
                "FRESHNESS" => {
                    let pulling = d.pulls.iter().find(|p| p.path == r.path);
                    if let Some(p) = pulling {
                        ui.label(
                            egui::RichText::new(copy::refused_pull_progress(p.percent))
                                .font(theme::font(theme::step::META, theme::fam_mono()))
                                .color(theme::ink_muted()),
                        );
                    }
                    if controls::ghost_small(ui, copy::REFUSED_RETRY).clicked() {
                        clear = true;
                    }
                }
                _ => {
                    if controls::ghost_small(ui, copy::REFUSED_RETRY).clicked() {
                        clear = true;
                    }
                }
            });
        });
        ui.add_space(theme::space::M);
        if clear {
            self.refusal = None;
            if let Ok(mut g) = self.shared.lock() {
                g.refusal = None;
            }
        }
    }

    /// Everything the worker is doing for this session, with the step it has
    /// reached and a way out. Before this, a click on Lock and a click on
    /// nothing looked the same until a toast arrived seconds later.
    fn working_band(&mut self, ui: &mut egui::Ui, dir: &str, now: f64) {
        let mine: Vec<InFlight> = self
            .inflight
            .iter()
            .filter(|f| f.dir == dir)
            .cloned()
            .collect();
        if mine.is_empty() {
            return;
        }
        let mut cancel = None;
        register::notice(ui, theme::Custody::Mine, |ui| {
            for f in &mine {
                ui.horizontal(|ui| {
                    ceremony::loading_mark(ui, theme::sized(theme::step::LABEL));
                    ui.label(
                        egui::RichText::new(copy::working_step(f.what, &f.subject, f.step))
                            .font(theme::font(
                                theme::step::BODY,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink()),
                    );
                    // A command that has run long enough to worry about says so
                    // rather than leaving the reader to guess whether it hung.
                    if now - f.started > SLOW_AFTER {
                        register::tag(ui, copy::WORKING_SLOW, theme::custody_stale());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if f.cancelling {
                            ui.label(
                                egui::RichText::new(copy::WORKING_CANCEL)
                                    .font(theme::font(
                                        theme::step::META,
                                        egui::FontFamily::Proportional,
                                    ))
                                    .color(theme::ink_faint()),
                            );
                        } else if controls::ghost_small(ui, copy::WORKING_CANCEL).clicked() {
                            cancel = Some(f.id);
                        }
                    });
                });
            }
        });
        ui.add_space(theme::space::M);
        if let Some(id) = cancel {
            self.send(Cmd::Cancel(id));
        }
    }

    /// Says how old the data on screen is when a session stops answering, so a
    /// wedged daemon is never mistaken for a quiet one.
    fn staleness_line(&mut self, ui: &mut egui::Ui, dir: &str, now: f64) {
        match self.reach.get(dir).copied().unwrap_or_default() {
            Reach::Live => {}
            Reach::Slow => {
                ui.label(
                    egui::RichText::new(copy::REACH_SLOW)
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_faint()),
                );
                ui.add_space(theme::space::S);
            }
            Reach::Stalled => {
                let age = self
                    .fetched_at
                    .get(dir)
                    .map(|t| (now - t).max(0.0) as u64)
                    .unwrap_or(0);
                register::notice(ui, theme::Custody::Stale, |ui| {
                    ui.label(
                        egui::RichText::new(copy::stale_for(age))
                            .font(theme::font(
                                theme::step::BODY,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink_muted()),
                    );
                });
                ui.add_space(theme::space::M);
            }
        }
    }

    /// Whether the worker is already acting on this path, so the view can
    /// disable the control rather than let the user queue a second write.
    fn busy_with(&self, dir: &str, subject: &str) -> bool {
        self.inflight
            .iter()
            .any(|f| f.dir == dir && f.subject == subject)
    }

    /// Renaming a synced path. A rename in the file manager is half a delete,
    /// and the delete half is reverted by design, so this is the only way to do
    /// it — and it is a guided lease-move-publish, not a local file operation.
    fn rename_overlay(&mut self, ui: &mut egui::Ui) {
        let Some((from, _)) = self.rename.clone() else {
            return;
        };
        let Some(sel) = self.selected.clone() else {
            self.rename = None;
            return;
        };
        let mut go = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("rename"))
            .backdrop_color(theme::scrim())
            .frame(register::overlay())
            .show(ui.ctx(), |ui| {
                ui.set_width(460.0);
                ui.label(
                    egui::RichText::new(copy::RENAME_TITLE)
                        .font(theme::font(theme::step::TITLE, theme::fam_serif()))
                        .color(theme::ink()),
                );
                ui.add_space(theme::space::S);
                ui.label(
                    egui::RichText::new(copy::RENAME_BODY)
                        .font(theme::font(
                            theme::step::BODY,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
                ui.add_space(theme::space::M);
                ui.label(
                    egui::RichText::new(&from)
                        .font(theme::font(theme::step::DATA, theme::fam_mono()))
                        .color(theme::ink_faint()),
                );
                ui.add_space(theme::space::S);
                let valid = if let Some((_, to)) = self.rename.as_mut() {
                    let trimmed = to.trim().to_string();
                    let ok = !trimmed.is_empty() && trimmed != from;
                    let state = if trimmed.is_empty() {
                        fields::FieldState::Neutral
                    } else if ok {
                        fields::FieldState::Valid
                    } else {
                        fields::FieldState::Invalid
                    };
                    let r = fields::text_field(ui, to, copy::RENAME_TITLE, 400.0, state);
                    if !self.rename_focused {
                        r.request_focus();
                        self.rename_focused = true;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ok {
                        go = true;
                    }
                    ok
                } else {
                    false
                };
                ui.add_space(theme::space::M);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Disabled rather than silently doing nothing, which is
                        // what an empty-name click used to do elsewhere.
                        let r = ui
                            .add_enabled_ui(valid, |ui| {
                                components::bevel_primary(ui, copy::RENAME_VERB)
                            })
                            .inner;
                        if r.clicked() {
                            go = true;
                        }
                        if controls::ghost_button(ui, copy::ACTION_CANCEL).clicked() {
                            close = true;
                        }
                    });
                });
            });
        if go && let Some((from, to)) = self.rename.take() {
            self.rename_focused = false;
            self.send(Cmd::Move {
                dir: PathBuf::from(&sel),
                from,
                to: to.trim().to_string(),
            });
            return;
        }
        if close || modal.should_close() {
            self.rename = None;
            self.rename_focused = false;
        }
    }

    /// A long answer — a diff, a doctor report, a dashboard address — shown as
    /// a real modal rather than crammed into a four-second toast.
    fn report_overlay(&mut self, ui: &mut egui::Ui) {
        let Some(r) = self.report.clone() else {
            return;
        };
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("report"))
            .backdrop_color(theme::scrim())
            .frame(register::overlay())
            .show(ui.ctx(), |ui| {
                ui.set_width(640.0);
                ui.label(
                    egui::RichText::new(&r.title)
                        .font(theme::font(theme::step::TITLE, theme::fam_serif()))
                        .color(if r.failed {
                            theme::custody_blocked()
                        } else {
                            theme::ink()
                        }),
                );
                ui.add_space(theme::space::M);
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&r.body)
                                .font(theme::font(theme::step::DATA, theme::fam_mono()))
                                .color(theme::ink_muted()),
                        );
                    });
                ui.add_space(theme::space::M);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if controls::ghost_button(ui, copy::REPORT_CLOSE).clicked() {
                            close = true;
                        }
                        if controls::ghost_button(ui, copy::REPORT_COPY).clicked() {
                            ui.ctx().copy_text(r.body.clone());
                        }
                        if let Some(link) = &r.link
                            && components::bevel_primary(ui, copy::REPORT_OPEN).clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(link));
                        }
                    });
                });
            });
        if close || modal.should_close() {
            self.report = None;
        }
    }

    /// The register a tab shows before its detail has arrived: the ruler the
    /// real entries will land in, with blanks where they will be written.
    fn tab_waiting(&mut self, ui: &mut egui::Ui) {
        let cols = [
            register::Col::new("", register::ColW::Flex(3.0)),
            register::Col::new("", register::ColW::Fixed(78.0)).right(),
            register::Col::new("", register::ColW::Fixed(132.0)),
        ];
        register::Register::new("waiting", &cols).show(ui, register::Content::Loading, |_| {});
    }

    /// Runs a narrative (non-tabular) tab inside its own scroll area.
    fn scrolled(
        &mut self,
        ui: &mut egui::Ui,
        salt: &str,
        add: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .auto_shrink([false, false])
            .show(ui, |ui| add(self, ui));
    }

    /// The session's title page.
    ///
    /// Every other tab is a register. This one is not: it answers "what is the
    /// state of this folder" in a sentence, then says what — if anything —
    /// needs the reader, then shows what has been moving. The invite is a
    /// setup action and lives behind a disclosure rather than sitting
    /// permanently at the foot of a monitoring view.
    fn tab_overview(&mut self, ui: &mut egui::Ui, d: &Detail) {
        let standing = copy::Standing {
            running: d.running,
            strict: d.strict,
            files: d.files_total.max(d.files.len()),
            held_by_you: d.files.iter().filter(|f| f.mine_lock).count(),
            held_by_peers: d
                .files
                .iter()
                .filter(|f| f.locked_by.is_some() && !f.mine_lock)
                .count(),
            conflicts: d.conflicts.len(),
            peers_online: d.members.iter().filter(|m| m.online).count(),
            peers_total: d.members.len(),
        };

        ui.add_space(theme::space::M);
        ui.label(
            egui::RichText::new(copy::standing(&standing))
                .font(theme::font(theme::step::DISPLAY, theme::fam_serif()))
                .color(theme::ink()),
        );
        ui.add_space(theme::space::M);

        // Only what applies. An untroubled session says its sentence and stops.
        for a in copy::attention(&standing) {
            let kind = match a {
                copy::Attention::Conflicts(_) => theme::Custody::Quarantined,
                copy::Attention::NoPeers => theme::Custody::Blocked,
                copy::Attention::Unpublished(_) => theme::Custody::Mine,
                copy::Attention::Stopped => theme::Custody::Free,
            };
            let mut go = false;
            register::notice(ui, kind, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(a.line())
                            .font(theme::font(
                                theme::step::BODY,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(v) = a.verb()
                            && controls::ghost_small(ui, v).clicked()
                        {
                            go = true;
                        }
                    });
                });
            });
            ui.add_space(theme::space::S);
            if go {
                match a {
                    copy::Attention::Conflicts(_) => self.tab = Tab::Conflicts,
                    copy::Attention::NoPeers => self.tab = Tab::Peers,
                    copy::Attention::Unpublished(_) => self.tab = Tab::Files,
                    copy::Attention::Stopped => {
                        self.send(Cmd::Start(PathBuf::from(&d.dir)));
                    }
                }
            }
        }

        // The mesh in one line rather than a second peer list: the Peers tab
        // renders the same members in more detail, and two renderings of one
        // record taught the reader to distrust both.
        register::heading(ui, copy::OVERVIEW_STATE);
        let cols = [
            register::Col::new("", register::ColW::Flex(2.0)),
            register::Col::new("", register::ColW::Flex(3.0)),
        ];
        let rows: Vec<(&str, String)> = vec![
            (
                copy::STATE_MODE,
                if d.strict {
                    copy::MODE_STRICT.to_string()
                } else {
                    copy::MODE_EASY.to_string()
                },
            ),
            (
                copy::STATE_ROLE,
                if d.role.is_empty() {
                    copy::UNKNOWN.to_string()
                } else {
                    d.role.clone()
                },
            ),
            (copy::STATE_FILES, figures::count(standing.files, "file")),
            (
                copy::STATE_PEERS,
                format!("{} of {}", standing.peers_online, standing.peers_total),
            ),
            (
                copy::STATE_CONFLICTS,
                copy::plural(standing.conflicts, "copy", "copies"),
            ),
        ];
        register::Register::new("session-state", &cols)
            .no_margin()
            .show(ui, register::Content::Entries(rows.len()), |e| {
                let (k, v) = &rows[e.index()];
                e.no_mark();
                let k = *k;
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(k)
                            .font(theme::font(
                                theme::step::LABEL,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink_muted()),
                    );
                });
                let v = v.clone();
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(&v)
                            .font(theme::font(theme::step::DATA, theme::fam_mono()))
                            .color(theme::ink()),
                    );
                });
            });

        self.overview_movement(ui, d);
        self.overview_invite(ui, d);
    }

    /// Transfers and events are one stream — what has moved, and what has
    /// happened — so they are one register rather than a progress card above a
    /// bulleted list in a different visual language.
    fn overview_movement(&mut self, ui: &mut egui::Ui, d: &Detail) {
        let pulls = d.pulls.len();
        let events = d.events.len();
        if pulls == 0 && events == 0 {
            return;
        }
        register::heading(ui, copy::OVERVIEW_MOVEMENT);

        if d.backlog > 0 || d.resuming > 0 || d.download_limit_bps > 0 {
            ui.label(
                egui::RichText::new(copy::transfer_meta(
                    d.backlog,
                    d.resuming,
                    (d.download_limit_bps > 0).then(|| human_bytes(d.download_limit_bps)),
                ))
                .font(theme::font(
                    theme::step::META,
                    egui::FontFamily::Proportional,
                ))
                .color(theme::ink_faint()),
            );
            ui.add_space(theme::space::S);
        }

        let cols = [
            register::Col::new("what", register::ColW::Flex(3.0)),
            register::Col::new("progress", register::ColW::Fixed(150.0)),
            register::Col::new("rate", register::ColW::Fixed(96.0)).right(),
        ];
        register::Register::new("movement", &cols)
            .max_height(theme::density().row_h() * 8.0)
            .show(ui, register::Content::Entries(pulls + events), |e| {
                e.no_mark();
                if e.index() < pulls {
                    let p = &d.pulls[e.index()];
                    let path = p.path.clone();
                    e.cell(|ui| {
                        ui.label(
                            egui::RichText::new(&path)
                                .font(theme::font(theme::step::LABEL, theme::fam_medium()))
                                .color(theme::ink()),
                        );
                    });
                    let share = p.percent as f32 / 100.0;
                    let done = human_bytes(p.bytes_done);
                    let whole = human_bytes(p.bytes_total);
                    e.cell(|ui| {
                        ui.vertical(|ui| {
                            components::progress_gold(ui, share);
                            ui.label(
                                egui::RichText::new(format!("{done} of {whole}"))
                                    .font(theme::font(theme::step::META, theme::fam_mono()))
                                    .color(theme::ink_faint()),
                            );
                        });
                    });
                    e.figure(&format!("{}/s", human_bytes(p.rate)), theme::ink_muted());
                } else {
                    let ev = &d.events[events - 1 - (e.index() - pulls)];
                    let text = ev.text.clone();
                    e.cell(|ui| {
                        controls::diamond_bullet(ui);
                        ui.label(
                            egui::RichText::new(&text)
                                .font(theme::font(theme::step::DATA, theme::fam_mono()))
                                .color(theme::ink_muted()),
                        );
                    });
                    e.cell(|_| {});
                    e.cell(|_| {});
                }
            });
    }

    /// Bringing someone in is a setup act, not something to monitor, so it sits
    /// behind a disclosure — and the ticket it mints is scoped, which the
    /// window previously could not express at all.
    fn overview_invite(&mut self, ui: &mut egui::Ui, d: &Detail) {
        register::heading(ui, copy::OVERVIEW_INVITE);
        let open = self.invite_open;
        ui.horizontal(|ui| {
            if controls::chevron(ui, open).clicked() {
                self.invite_open = !open;
            }
            ui.label(
                egui::RichText::new(copy::INVITE_CAUTION)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_faint()),
            );
        });
        if !open {
            return;
        }
        ui.add_space(theme::space::S);

        // A ticket carries the session secret, so who may use it and for how
        // long are the two questions worth asking before minting one.
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(copy::INVITE_ROLE)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_muted()),
            );
            for r in copy::INVITE_ROLES {
                if choice(ui, r, self.invite_role == *r) {
                    self.invite_role = (*r).to_string();
                }
            }
        });
        ui.add_space(theme::space::XS);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(copy::INVITE_EXPIRY)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_muted()),
            );
            for (label, ms) in copy::INVITE_TTLS {
                if choice(ui, label, self.invite_ttl_ms == *ms) {
                    self.invite_ttl_ms = *ms;
                }
            }
        });
        ui.add_space(theme::space::M);
        if components::bevel_primary(ui, copy::INVITE_MINT).clicked() {
            self.send(Cmd::Invite {
                dir: PathBuf::from(&d.dir),
                role: (self.invite_role != copy::INVITE_ROLE_DEFAULT)
                    .then(|| self.invite_role.clone()),
                ttl_ms: self.invite_ttl_ms,
            });
        }

        let qr = d.invite.as_ref().and_then(|t| self.qr_texture(ui.ctx(), t));
        match &d.invite {
            Some(t) => {
                ui.add_space(theme::space::M);
                ceremony::ticket_card(ui, t, qr.as_ref());
                if controls::ghost_button(ui, copy::INVITE_COPY).clicked() {
                    ui.ctx().copy_text(t.clone());
                    self.toasts.push(
                        copy::TOAST_TICKET_COPIED.into(),
                        toasts::Kind::Info,
                        ui.input(|i| i.time),
                    );
                }
            }
            None => {
                ui.add_space(theme::space::S);
                ui.label(
                    egui::RichText::new(copy::INVITE_EMPTY)
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_faint()),
                );
            }
        }
    }

    fn tab_peers(&mut self, ui: &mut egui::Ui, d: &Detail) {
        ui.label(
            egui::RichText::new(copy::PEERS_INTRO)
                .font(theme::font(
                    theme::step::META,
                    egui::FontFamily::Proportional,
                ))
                .color(theme::ink_muted()),
        );
        rhythm::space(ui, 2);

        // The mesh as a shape before the mesh as a list — but only when there
        // is a mesh. With nobody to plot, the sky is a large empty circle sitting
        // on top of the answer, which is the register's own empty state.
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
        let hovered = if stars.is_empty() {
            None
        } else {
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
            let h = constellation::sky(ui, rect, &stars, sky_t);
            rhythm::space(ui, 2);
            h
        };

        // Sampled before the register so the row closure needs no borrow of
        // `self`: the whole point of the table is that every peer's figures sit
        // in one column, and that only works if they are gathered up front.
        let rows: Vec<PeerRow> = d
            .members
            .iter()
            .map(|m| PeerRow {
                rtt: if has_path(m) {
                    self.telemetry.last_ms(&m.id_short).or(m.rtt_ms)
                } else {
                    None
                },
                series: self.telemetry.series(&m.id_short),
                lit: telemetry::grade_lit(&m.grade),
            })
            .collect();

        let cols = [
            register::Col::new("", register::ColW::Fixed(26.0)),
            register::Col::new("peer", register::ColW::Flex(2.0)),
            register::Col::new("id", register::ColW::Fixed(92.0)),
            register::Col::new("link", register::ColW::Fixed(126.0)),
            register::Col::new("rtt", register::ColW::Fixed(96.0)).right(),
            register::Col::new("trend", register::ColW::Fixed(128.0)),
            register::Col::new("up / down", register::ColW::Fixed(132.0)),
        ];
        let content = if let Some(err) = &d.error {
            register::Content::Failed {
                what: copy::PEERS_FAILED,
                because: err,
            }
        } else if d.members.is_empty() {
            register::Content::Empty {
                title: copy::PEERS_EMPTY_TITLE,
                hint: if d.running {
                    copy::PEERS_EMPTY_RUNNING
                } else {
                    copy::PEERS_EMPTY_STOPPED
                },
            }
        } else {
            register::Content::Entries(d.members.len())
        };

        let out = register::Register::new("peers", &cols).show(ui, content, |e| {
            let (Some(m), Some(r)) = (d.members.get(e.index()), rows.get(e.index())) else {
                return;
            };
            let grade_color = match m.grade.as_str() {
                "Good" => theme::custody_good(),
                "Fair" => theme::custody_stale(),
                "Poor" => theme::custody_blocked(),
                _ => theme::ink_faint(),
            };
            e.no_mark();
            // Hovering a star in the sky selects its entry, so the shape and
            // the table are legibly the same peers.
            e.selected(hovered == Some(e.index()));

            let lit = r.lit;
            e.cell(|ui| health::signal_arcs(ui, lit, grade_color));

            let name = m.name.clone().unwrap_or_else(|| m.id_short.clone());
            e.cell(|ui| {
                ui.label(
                    egui::RichText::new(&name)
                        .font(theme::font(theme::step::LABEL, theme::fam_medium()))
                        .color(if m.online {
                            theme::ink()
                        } else {
                            theme::ink_muted()
                        }),
                );
            });
            let id = m.id_short.clone();
            e.cell(|ui| {
                ui.label(
                    egui::RichText::new(&id)
                        .font(theme::font(theme::step::DATA, theme::fam_mono()))
                        .color(theme::ink_faint()),
                );
            });

            let conn = m.conn.clone();
            let online = m.online;
            let lan = m.via_lan;
            let relay = m.relay_url.is_some();
            let flaps = m.flaps;
            e.cell(|ui| {
                register::tag(
                    ui,
                    &conn,
                    if online {
                        theme::custody_good()
                    } else {
                        theme::ink_faint()
                    },
                );
                if lan {
                    register::tag(ui, copy::PEER_LAN, theme::custody_peer());
                } else if relay {
                    register::tag(ui, copy::PEER_RELAY, theme::ink_muted());
                }
                if flaps > 0 {
                    register::tag(ui, &format!("{flaps}/min"), theme::custody_stale());
                }
            });

            // No path, no round-trip. The ring's last sample is history, and
            // `online` alone can mean "seen in gossip via someone else" —
            // showing that as a live figure is the same lie as showing an
            // offline peer its last RTT.
            let rtt_text = match r.rtt {
                Some(v) if m.jitter_ms > 0.05 => format!("{v} ms ±{:.1}", m.jitter_ms),
                Some(v) => format!("{v} ms"),
                None => "—".into(),
            };
            e.figure(
                &rtt_text,
                if r.rtt.is_some() {
                    theme::ink_muted()
                } else {
                    theme::ink_faint()
                },
            );

            let series = r.series.clone();
            let trend_color = if online {
                theme::custody_peer()
            } else {
                theme::ink_faint()
            };
            e.cell(|ui| {
                let h = theme::density().row_h() - theme::space::M;
                let w = ui.available_width();
                health::sparkline(ui, &series, egui::vec2(w, h), trend_color);
            });

            let up = telemetry::fmt_rate(m.rate_tx);
            let down = telemetry::fmt_rate(m.rate_rx);
            e.cell(|ui| health::rate_arrows(ui, &up, &down));

            // Lifetime totals and the direct-path timing are detail, not a
            // column — they belong on hover rather than widening every row.
            let lifetime = copy::peer_lifetime(
                &telemetry::fmt_total(m.bytes_tx),
                &telemetry::fmt_total(m.bytes_rx),
                m.ttd_ms,
            );
            e.response().clone().on_hover_text(lifetime);
        });
        if out.retry {
            self.send(Cmd::Refresh);
        }
    }

    fn tab_files(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        let now = ui.input(|i| i.time);
        ui.horizontal(|ui| {
            let out = fields::search_field(ui, &mut self.file_filter, copy::HINT_FILTER, 280.0);
            if out.cleared {
                self.file_filter.clear();
            }
            // A new pattern is a new result set, so it starts at its first
            // page rather than part-way through the previous one's.
            if out.response.changed() {
                self.file_offset = 0;
            }
            // Ctrl+F asked for this field; the tab switch only reaches it now.
            if std::mem::take(&mut self.focus_filter) {
                out.response.request_focus();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(figures::count(d.files_total, "file"))
                        .font(theme::font(theme::step::META, theme::fam_mono()))
                        .color(theme::ink_faint()),
                );
            });
        });

        // The daemon only publishes the first `FILES_LIST_MAX` paths, and the
        // filter below runs over that slice — so say plainly that the rest are
        // reachable from the CLI rather than implying the list is the folder.
        if d.files_truncated {
            ui.add_space(theme::space::S);
            register::notice(ui, theme::Custody::Stale, |ui| {
                ui.label(
                    egui::RichText::new(copy::files_truncated(d.files.len(), d.files_total))
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
            });
        }
        ui.add_space(theme::space::M);

        let filter = self.file_filter.to_lowercase();
        let by_size = self.files_sort == grouping::SortMode::Size;
        // A client-side filter over a capped list can only find what the cap
        // let through, so anything the snapshot cannot answer goes to the
        // daemon, which matches against the whole index.
        let ask_daemon = d.running && (d.files_truncated || !filter.is_empty());
        let served: Vec<FileRow>;
        let rows: Vec<&FileRow> = if ask_daemon {
            let want = (d.dir.clone(), filter.clone(), by_size, self.file_offset);
            // Debounced: the query runs on the daemon's single actor, so one
            // round trip per keystroke would put the whole session behind the
            // user's typing. A sort or page change is not typing and goes at
            // once.
            if self.file_query.as_ref() != Some(&want) {
                let typed = self.file_query.as_ref().is_some_and(|(dir, _, size, off)| {
                    *dir == want.0 && *size == want.2 && *off == want.3
                });
                if self.file_query_due.is_none() {
                    self.file_query_due = Some(if typed { now + SEARCH_DEBOUNCE } else { now });
                }
                if self.file_query_due.is_some_and(|due| now >= due) {
                    self.file_query = Some(want.clone());
                    self.file_query_due = None;
                    self.send(Cmd::SearchFiles {
                        dir: PathBuf::from(&d.dir),
                        pattern: self.file_filter.clone(),
                        by_size,
                        desc: by_size,
                        offset: self.file_offset,
                    });
                } else {
                    // Nothing else drives a frame this soon; without this the
                    // debounce would not fire until the next 1.5s refresh.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(SEARCH_DEBOUNCE));
                }
            } else {
                self.file_query_due = None;
            }
            served = match &self.file_page {
                // Only an answer to *this* folder and *this* pattern; a stale
                // page would read as the answer to what was just typed.
                Some(p) if p.dir == d.dir && p.pattern.to_lowercase() == filter => p.rows.clone(),
                _ => Vec::new(),
            };
            served.iter().collect()
        } else {
            self.file_query = None;
            served = Vec::new();
            let _ = &served;
            d.files.iter().collect()
        };

        // Grouped by top-level folder so the weight of the session is visible;
        // the groups are flattened into the same row stream as the files and
        // their open version histories, which keeps every line one row tall and
        // therefore keeps the whole register virtualised.
        let keys: Vec<(String, u64)> = rows.iter().map(|f| (f.path.clone(), f.size)).collect();
        let groups = grouping::group_files(&keys, self.files_sort);
        let vctx = VersionCtx::new(d);
        let mut lines: Vec<FileLine<'_>> = Vec::with_capacity(rows.len() + groups.len());
        let mut folio = 0usize;
        for g in &groups {
            lines.push(FileLine::Group(g));
            for i in &g.indices {
                let Some(f) = rows.get(*i) else { continue };
                folio += 1;
                lines.push(FileLine::File(f, folio));
                if self.open_versions.contains(&f.path)
                    && let Some(vs) = d.versions.get(&f.path)
                {
                    for v in vs {
                        lines.push(FileLine::Version(&f.path, v));
                    }
                }
            }
        }

        let cols = [
            register::Col::new("path", register::ColW::Flex(3.0)).sortable("name"),
            register::Col::new("kind", register::ColW::Fixed(46.0)),
            register::Col::new("bytes", register::ColW::Fixed(78.0))
                .right()
                .sortable("size"),
            register::Col::new("custody", register::ColW::Fixed(132.0)),
            register::Col::new("", register::ColW::Fixed(86.0)).right(),
        ];
        let content = if let Some(e) = &d.error {
            register::Content::Failed {
                what: copy::FILES_FAILED,
                because: e,
            }
        } else if !lines.is_empty() {
            register::Content::Entries(lines.len())
        } else if filter.is_empty() {
            register::Content::Empty {
                title: copy::FILES_EMPTY_TITLE,
                hint: copy::FILES_EMPTY_HINT,
            }
        } else {
            register::Content::Empty {
                title: copy::FILES_FILTER_EMPTY,
                hint: copy::FILES_FILTER_EMPTY_HINT,
            }
        };

        if ask_daemon
            && let Some(page) = self.file_page.clone()
            && page.dir == d.dir
        {
            ui.add_space(theme::space::S);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(copy::files_page(
                        page.offset,
                        page.rows.len(),
                        page.matched,
                    ))
                    .font(theme::font(theme::step::META, theme::fam_mono()))
                    .color(theme::ink_faint()),
                );
                if page.offset > 0 && controls::ghost_small(ui, copy::PAGE_PREV).clicked() {
                    self.file_offset = page.offset.saturating_sub(page.rows.len().max(1));
                }
                if let Some(next) = page.next_offset
                    && controls::ghost_small(ui, copy::PAGE_NEXT).clicked()
                {
                    self.file_offset = next;
                }
            });
            ui.add_space(theme::space::S);
        }

        let sort_key = match self.files_sort {
            grouping::SortMode::Size => "size",
            grouping::SortMode::Name => "name",
        };
        let out = register::Register::new("files", &cols)
            .sorted_by(sort_key, self.files_sort == grouping::SortMode::Size)
            .show(ui, content, |e| match lines[e.index()] {
                FileLine::Group(g) => self.file_group_line(e, g),
                FileLine::File(f, folio) => self.file_line(e, dir, d, f, folio),
                FileLine::Version(path, v) => self.version_line(e, dir, path, v, d.running, vctx),
            });
        if let Some(k) = out.sort {
            self.files_sort = match k {
                "size" => grouping::SortMode::Size,
                _ => grouping::SortMode::Name,
            };
        }
        if out.retry {
            self.send(Cmd::Refresh);
        }
    }

    /// A folder heading inside the files register: the group's name in the
    /// engraved voice, its file count and its share of the session's bytes.
    fn file_group_line(&mut self, e: &mut register::Entry<'_>, g: &grouping::Group) {
        e.no_mark().divider();
        let name = g.name.clone();
        let root = g.root;
        e.cell(|ui| {
            ui.label(
                egui::RichText::new(name)
                    .font(theme::font(
                        theme::step::LABEL,
                        if root {
                            egui::FontFamily::Proportional
                        } else {
                            theme::fam_serif()
                        },
                    ))
                    .color(if root {
                        theme::ink_faint()
                    } else {
                        theme::ink()
                    }),
            );
        });
        e.cell(|_| {});
        e.figure(&human_bytes(g.bytes), theme::ink_faint());
        let share = copy::group_share(g.indices.len(), g.share);
        e.cell(|ui| {
            ui.label(
                egui::RichText::new(share)
                    .font(theme::font(theme::step::META, theme::fam_mono()))
                    .color(theme::ink_faint()),
            );
        });
        e.cell(|_| {});
    }

    /// One file entry: path, kind, size, who holds it, and the one action its
    /// state actually permits.
    fn file_line(
        &mut self,
        e: &mut register::Entry<'_>,
        dir: &Path,
        d: &Detail,
        f: &FileRow,
        folio: usize,
    ) {
        let custody = file_custody(f, d.running);
        e.custody(custody);
        e.folio(folio);
        // Clicking a row marks it, which is what the bar's Rename acts on —
        // the verb is otherwise only reachable from the row's context menu.
        e.selected(self.marked_file.as_deref() == Some(f.path.as_str()));
        if e.response().clicked() {
            self.marked_file = Some(f.path.clone());
        }

        let open = self.open_versions.contains(&f.path);
        let has_versions = d.versions.contains_key(&f.path);
        let path = f.path.clone();
        let mut toggle = false;
        e.indent(1.0).cell(|ui| {
            if has_versions {
                if controls::chevron(ui, open).clicked() {
                    toggle = true;
                }
            } else {
                ui.add_space(theme::space::XL);
            }
            ui.label(
                egui::RichText::new(&path)
                    .font(theme::font(theme::step::LABEL, theme::fam_medium()))
                    .color(theme::ink()),
            );
        });
        if toggle {
            if open {
                self.open_versions.remove(&f.path);
            } else {
                self.open_versions.insert(f.path.clone());
            }
        }

        let path_for_chip = f.path.clone();
        e.cell(|ui| components::ext_chip(ui, &path_for_chip));
        e.figure(&human_bytes(f.size), theme::ink_muted());

        let word = match (&f.locked_by, f.mine_lock) {
            (Some(_), true) => copy::CUSTODY_HELD_YOU.to_string(),
            (Some(h), false) => format!("{} {}", copy::CUSTODY_HELD_BY, short(h)),
            (None, _) => copy::CUSTODY_FREE.to_string(),
        };
        let held = f.locked_by.is_some();
        e.cell(|ui| {
            if held {
                register::tag(ui, &word, custody.color());
            } else {
                ui.label(
                    egui::RichText::new(&word)
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_faint()),
                );
            }
        });

        // The daemon refuses every lease on a viewer or archive folder, so the
        // control is disabled with the reason rather than offered and refused.
        let may_edit = role_can_edit(&d.role);
        let running = d.running;
        let mine = f.mine_lock;
        let mut action = None;
        let mut wait = false;
        let working = self.busy_with(&d.dir, &f.path);
        e.cell(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if working {
                    ceremony::loading_mark(ui, theme::sized(theme::step::META));
                } else if !running {
                    ui.label(
                        egui::RichText::new(copy::FILES_STOPPED_ACTION)
                            .font(theme::font(
                                theme::step::META,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink_faint()),
                    );
                } else if !may_edit {
                    ui.add_enabled_ui(false, |ui| {
                        let _ = controls::ghost_small(ui, copy::ACTION_LOCK);
                    })
                    .response
                    .on_disabled_hover_text(copy::role_cannot_edit(&d.role));
                } else if held {
                    if mine {
                        if controls::ghost_small(ui, copy::ACTION_UNLOCK).clicked() {
                            action = Some(false);
                        }
                    } else if controls::ghost_small(ui, copy::ACTION_WAIT)
                        .on_hover_text(copy::ACTION_WAIT_HOVER)
                        .clicked()
                    {
                        action = Some(true);
                        wait = true;
                    }
                } else if controls::ghost_small(ui, copy::ACTION_LOCK).clicked() {
                    action = Some(true);
                }
            });
        });
        let mut menu: Option<FileMenu> = None;
        e.response().clone().context_menu(|ui| {
            ui.set_min_width(180.0);
            if ui.button(copy::MENU_RENAME).clicked() {
                menu = Some(FileMenu::Rename);
                ui.close();
            }
            if ui.button(copy::MENU_DIFF).clicked() {
                menu = Some(FileMenu::Diff);
                ui.close();
            }
            if ui.button(copy::MENU_COPY_PATH).clicked() {
                menu = Some(FileMenu::CopyPath);
                ui.close();
            }
        });
        match menu {
            Some(FileMenu::Rename) => {
                self.rename = Some((f.path.clone(), f.path.clone()));
            }
            Some(FileMenu::Diff) => {
                // Against the newest kept version: "what changed since the
                // last published state" is the question a diff answers here.
                if let Some(v) = d.versions.get(&f.path).and_then(|vs| vs.first()) {
                    self.send(Cmd::Diff {
                        dir: dir.to_path_buf(),
                        path: f.path.clone(),
                        n: v.n as usize,
                    });
                } else {
                    self.toasts.push(
                        copy::DIFF_NO_VERSIONS.into(),
                        toasts::Kind::Warn,
                        ui_time(e),
                    );
                }
            }
            Some(FileMenu::CopyPath) => {
                e.response().ctx.copy_text(f.path.clone());
            }
            None => {}
        }

        match action {
            Some(true) if wait => self.send(Cmd::LockWait {
                dir: dir.to_path_buf(),
                path: f.path.clone(),
            }),
            Some(true) => self.send(Cmd::Lock {
                dir: dir.to_path_buf(),
                path: f.path.clone(),
            }),
            Some(false) => self.send(Cmd::Unlock {
                dir: dir.to_path_buf(),
                path: f.path.clone(),
            }),
            None => {}
        }
    }

    /// One version entry, ruled into whichever register is showing it. The
    /// secondary verbs (pin, tag) live on the row's context menu so the action
    /// lane carries only the one that matters; the tag editor borrows the path
    /// lane, which is the only lane wide enough for a field.
    fn version_line(
        &mut self,
        e: &mut register::Entry<'_>,
        dir: &Path,
        path: &str,
        v: &VersionRow,
        running: bool,
        ctx: VersionCtx,
    ) {
        e.no_mark();
        let editing = self
            .tag_edit
            .as_ref()
            .is_some_and(|(p, n, _)| p == path && *n == v.n as usize);

        let mut commit: Option<Option<String>> = None;
        let mut cancel = false;
        let label = format!("v{}", v.n);
        let when = fmt_ts_full(v.ts_ms);
        let tag_text = v.tag.clone();
        let tag_buf = self.tag_edit.as_mut().map(|(_, _, b)| b);
        e.indent(2.0).cell(|ui| {
            ui.label(
                egui::RichText::new(&label)
                    .font(theme::font(theme::step::DATA, theme::fam_mono_medium()))
                    .color(theme::custody_peer()),
            );
            if editing && let Some(buf) = tag_buf {
                let r =
                    fields::text_field(ui, buf, copy::HINT_TAG, 160.0, fields::FieldState::Neutral);
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let t = buf.trim().to_string();
                    commit = Some(if t.is_empty() { None } else { Some(t) });
                }
                // Consumed, so Escape closes the editor without also reaching
                // the overlay handlers that would close the whole view.
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    cancel = true;
                }
            } else {
                ui.label(
                    egui::RichText::new(&when)
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
                if let Some(t) = &tag_text {
                    register::tag(ui, t, theme::custody_peer());
                }
            }
        });
        if cancel {
            self.tag_edit = None;
        }
        if let Some(name) = commit {
            self.send(Cmd::Tag {
                dir: dir.to_path_buf(),
                path: path.to_string(),
                n: v.n as usize,
                name,
            });
            self.tag_edit = None;
        }

        self.version_tail(e, dir, path, v, running, ctx);
    }

    /// The lanes every version entry shares, whatever leads it: the pin mark,
    /// the size, the age, the restore verb, and the context menu carrying the
    /// secondary verbs.
    fn version_tail(
        &mut self,
        e: &mut register::Entry<'_>,
        dir: &Path,
        path: &str,
        v: &VersionRow,
        running: bool,
        ctx: VersionCtx,
    ) {
        let pinned = v.pinned;
        e.cell(|ui| {
            if pinned {
                register::status_mark(ui, theme::gold());
            }
        });
        e.figure(
            &figures::align(&figures::split(&human_bytes(v.size)), ctx.width),
            theme::ink_muted(),
        );
        let age = figures::ago(v.ts_ms / 1000, ctx.now_s);
        e.cell(|ui| {
            ui.label(
                egui::RichText::new(&age)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_faint()),
            );
        });

        let mut restore = false;
        e.cell(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if controls::ghost_small(ui, copy::ACTION_RESTORE).clicked() {
                        restore = true;
                    }
                } else {
                    ui.label(
                        egui::RichText::new(copy::VERSION_STOPPED_ACTION)
                            .font(theme::font(
                                theme::step::META,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink_faint()),
                    );
                }
            });
        });
        if restore {
            self.ask(
                copy::RESTORE_TITLE,
                copy::restore_body(path, v.n, &human_bytes(v.size)),
                copy::ACTION_RESTORE,
                false,
                Cmd::Restore {
                    dir: dir.to_path_buf(),
                    path: path.to_string(),
                    n: v.n as usize,
                },
            );
        }

        let mut pin_toggle = false;
        let mut start_tag = false;
        e.response().clone().context_menu(|ui| {
            if ui
                .button(if pinned {
                    copy::ACTION_UNPIN
                } else {
                    copy::ACTION_PIN
                })
                .clicked()
            {
                pin_toggle = true;
                ui.close();
            }
            if ui.button(copy::ACTION_TAG).clicked() {
                start_tag = true;
                ui.close();
            }
        });
        if pin_toggle {
            self.send(Cmd::Pin {
                dir: dir.to_path_buf(),
                path: path.to_string(),
                n: v.n as usize,
                pinned: !v.pinned,
            });
        }
        if start_tag {
            self.tag_edit = Some((
                path.to_string(),
                v.n as usize,
                v.tag.clone().unwrap_or_default(),
            ));
        }
    }

    fn tab_history(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        // Flattened newest-first across every path, with a day heading opening
        // each run. The path is its own column rather than a label printed
        // above every entry, so a file with thirty versions names itself once
        // per line instead of thirty times over.
        let mut all: Vec<(&String, &VersionRow)> = d
            .versions
            .iter()
            .flat_map(|(p, vs)| vs.iter().map(move |v| (p, v)))
            .collect();
        all.sort_by_key(|e| std::cmp::Reverse(e.1.ts_ms));
        let total = all.len();
        all.truncate(HISTORY_MAX);

        let mut lines: Vec<HistoryLine<'_>> = Vec::with_capacity(all.len());
        let mut last_day = String::new();
        for (path, v) in all {
            let day = fmt_day(v.ts_ms);
            if day != last_day {
                lines.push(HistoryLine::Day(day.clone()));
                last_day = day;
            }
            lines.push(HistoryLine::Version(path, v));
        }

        if total > HISTORY_MAX {
            register::notice(ui, theme::Custody::Stale, |ui| {
                ui.label(
                    egui::RichText::new(copy::history_truncated(HISTORY_MAX, total))
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
            });
            ui.add_space(theme::space::M);
        }

        let vctx = VersionCtx::new(d);
        let cols = [
            register::Col::new("version", register::ColW::Flex(3.0)),
            register::Col::new("", register::ColW::Fixed(28.0)),
            register::Col::new("bytes", register::ColW::Fixed(78.0)).right(),
            register::Col::new("age", register::ColW::Fixed(96.0)),
            register::Col::new("", register::ColW::Fixed(86.0)).right(),
        ];
        let content = if let Some(err) = &d.error {
            register::Content::Failed {
                what: copy::HISTORY_FAILED,
                because: err,
            }
        } else if lines.is_empty() {
            register::Content::Empty {
                title: copy::HISTORY_EMPTY_TITLE,
                hint: if d.running {
                    copy::HISTORY_EMPTY_HINT_RUNNING
                } else {
                    copy::HISTORY_EMPTY_HINT_STOPPED
                },
            }
        } else {
            register::Content::Entries(lines.len())
        };

        let out = register::Register::new("history", &cols)
            .no_margin()
            .show(ui, content, |e| match &lines[e.index()] {
                HistoryLine::Day(day) => {
                    e.no_mark().divider();
                    let day = day.clone();
                    e.cell(|ui| {
                        ui.label(
                            egui::RichText::new(&day)
                                .font(theme::font(theme::step::LABEL, theme::fam_serif()))
                                .color(theme::ink()),
                        );
                    });
                }
                HistoryLine::Version(path, v) => {
                    // In History the path is what identifies the entry, so it
                    // leads the lane and the version number follows it.
                    let p = (*path).clone();
                    e.no_mark().cell(|ui| {
                        ui.label(
                            egui::RichText::new(&p)
                                .font(theme::font(theme::step::LABEL, theme::fam_medium()))
                                .color(theme::ink()),
                        );
                    });
                    self.version_tail(e, dir, path, v, d.running, vctx);
                }
            });
        if out.retry {
            self.send(Cmd::Refresh);
        }
    }

    fn tab_conflicts(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        ui.label(
            egui::RichText::new(copy::CONFLICTS_INTRO)
                .font(theme::font(
                    theme::step::META,
                    egui::FontFamily::Proportional,
                ))
                .color(theme::ink_muted()),
        );
        ui.add_space(theme::space::M);
        let cols = [
            register::Col::new("preserved copy", register::ColW::Flex(3.0)),
            register::Col::new("reason", register::ColW::Fixed(118.0)),
            register::Col::new("bytes", register::ColW::Fixed(78.0)).right(),
            register::Col::new("kept", register::ColW::Fixed(150.0)),
        ];
        let content = if let Some(err) = &d.error {
            register::Content::Failed {
                what: copy::CONFLICTS_FAILED,
                because: err,
            }
        } else if d.conflicts.is_empty() {
            register::Content::Empty {
                title: copy::CONFLICTS_EMPTY_TITLE,
                hint: copy::CONFLICTS_EMPTY_HINT,
            }
        } else {
            register::Content::Entries(d.conflicts.len())
        };

        self.conflict_sel = self.conflict_sel.min(d.conflicts.len().saturating_sub(1));
        let sel = self.conflict_sel;
        let mut pick = None;
        // Capped so the scales below stay on screen: the register lists, the
        // pane weighs, and the decision is made against one drawing rather than
        // against a stack of them.
        let body_h = (theme::density().row_h() * 6.0).min(ui.available_height() * 0.42);
        let out = register::Register::new("conflicts", &cols)
            .max_height(body_h)
            .show(ui, content, |e| {
                let Some(c) = d.conflicts.get(e.index()) else {
                    return;
                };
                let selected = e.index() == sel;
                e.custody(theme::Custody::Quarantined);
                e.selected(selected);
                if e.response().clicked() {
                    pick = Some(e.index());
                }
                let label = if c.path.is_empty() {
                    c.name.clone()
                } else {
                    c.path.clone()
                };
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(&label)
                            .font(theme::font(theme::step::LABEL, theme::fam_medium()))
                            .color(theme::ink()),
                    );
                });
                let reason = c.reason.clone();
                e.cell(|ui| register::tag(ui, &reason, theme::custody_quarantine()));
                e.figure(&human_bytes(c.size), theme::ink_muted());
                let when = fmt_ts_full(c.ts_ms);
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(&when)
                            .font(theme::font(theme::step::META, theme::fam_mono()))
                            .color(theme::ink_faint()),
                    );
                });
            });
        if let Some(i) = pick {
            self.conflict_sel = i;
        }
        if out.retry {
            self.send(Cmd::Refresh);
        }
        self.prune_bar(ui, dir, d);
        let Some(c) = d.conflicts.get(self.conflict_sel).cloned() else {
            return;
        };
        self.conflict_detail(ui, dir, d, &c);
    }

    /// Bulk-clearing preserved copies that have aged out. Destructive, so the
    /// count and the bytes are computed and shown before anything is asked —
    /// the user confirms against exactly what will be deleted.
    fn prune_bar(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail) {
        if d.conflicts.is_empty() || !d.running || !role_can_edit(&d.role) {
            return;
        }
        let (names, bytes) = prunable(&d.conflicts, crate::now_ms(), self.prune_age_ms);

        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(copy::PRUNE_OLDER_THAN)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_muted()),
            );
            for (label, ms) in copy::PRUNE_AGES {
                if choice(ui, label, self.prune_age_ms == *ms) {
                    self.prune_age_ms = *ms;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = !names.is_empty();
                let r = ui
                    .add_enabled_ui(enabled, |ui| controls::bevel_danger(ui, copy::PRUNE_VERB))
                    .inner;
                if r.clicked() {
                    self.ask(
                        copy::PRUNE_TITLE,
                        copy::prune_body(names.len(), &human_bytes(bytes)),
                        copy::PRUNE_VERB,
                        true,
                        Cmd::PruneConflicts {
                            dir: dir.to_path_buf(),
                            older_than_ms: self.prune_age_ms,
                            names: names.clone(),
                        },
                    );
                }
                ui.label(
                    egui::RichText::new(copy::prune_preview(names.len(), &human_bytes(bytes)))
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_faint()),
                );
            });
        });
    }

    /// The decision pane for one preserved copy: the two candidates weighed
    /// against each other, then the three resolutions ordered by what each one
    /// does to the user's bytes.
    fn conflict_detail(&mut self, ui: &mut egui::Ui, dir: &Path, d: &Detail, c: &ConflictRow) {
        register::heading(ui, copy::CONFLICT_WEIGH_TITLE);
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
                accent: theme::custody_quarantine(),
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
                accent: theme::custody_peer(),
            },
        );
        ui.add_space(theme::space::M);

        if !d.running {
            ui.label(
                egui::RichText::new(copy::CONFLICTS_STOPPED_NOTE)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_faint()),
            );
            return;
        }
        if !role_can_edit(&d.role) {
            register::notice(ui, theme::Custody::Blocked, |ui| {
                ui.label(
                    egui::RichText::new(copy::role_cannot_edit(&d.role))
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
            });
            return;
        }

        // `keep both` writes the copy beside the synced file under a name that
        // collides with nothing indexed. An orphaned copy has no original path,
        // so its name is derived from the copy's own id — it stays restorable,
        // which is the point: the only action previously offered on an orphan
        // was the one that deleted it.
        let stem = if c.path.is_empty() {
            c.name.as_str()
        } else {
            c.path.as_str()
        };
        let both_target = crate::conflicts::both_name(stem, crate::now_ms(), |cand| {
            d.files.iter().any(|f| f.path == cand)
        });

        if c.path.is_empty() {
            ui.label(
                egui::RichText::new(copy::CONFLICT_ORPHAN_NOTE)
                    .font(theme::font(
                        theme::step::META,
                        egui::FontFamily::Proportional,
                    ))
                    .color(theme::ink_muted()),
            );
            ui.add_space(theme::space::S);
        }

        ui.horizontal(|ui| {
            if components::bevel_primary(ui, copy::CONFLICT_KEEP_BOTH).clicked() {
                self.send(Cmd::ResolveBoth {
                    dir: dir.to_path_buf(),
                    id: c.name.clone(),
                    target: both_target.clone(),
                });
            }
            // Both remaining verbs destroy bytes, so both are styled as
            // destructive and both ask first. Keep-mine is the heavier of the
            // two — it overwrites what every peer holds *and* deletes the
            // preserved copy — and it used to be the unconfirmed gold primary.
            if !c.path.is_empty() && controls::bevel_danger(ui, copy::CONFLICT_KEEP_MINE).clicked()
            {
                self.ask(
                    copy::CONFLICT_MINE_TITLE,
                    copy::conflict_mine_body(&c.path, &human_bytes(c.size)),
                    copy::CONFLICT_MINE_VERB,
                    true,
                    Cmd::ResolveMine {
                        dir: dir.to_path_buf(),
                        id: c.name.clone(),
                        target: c.path.clone(),
                    },
                );
            }
            if controls::bevel_danger(ui, copy::CONFLICT_KEEP_THEIRS).clicked() {
                self.ask(
                    copy::CONFLICT_DISCARD_TITLE,
                    copy::conflict_discard_body(&c.name),
                    copy::CONFLICT_DISCARD_VERB,
                    true,
                    Cmd::ConflictDiscard {
                        dir: dir.to_path_buf(),
                        id: c.name.clone(),
                    },
                );
            }
        });
        ui.add_space(theme::space::XS);
        ui.label(
            egui::RichText::new(format!(
                "{} — {}",
                copy::CONFLICT_KEEP_BOTH_NOTE,
                both_target
            ))
            .font(theme::font(
                theme::step::META,
                egui::FontFamily::Proportional,
            ))
            .color(theme::ink_faint()),
        );
    }

    fn tab_audit(&mut self, ui: &mut egui::Ui, d: &Detail) {
        let kinds: Vec<String> = {
            let mut k: Vec<String> = d.audit.iter().map(|a| a.kind.clone()).collect();
            k.sort_unstable();
            k.dedup();
            k
        };
        ui.horizontal(|ui| {
            let out = fields::search_field(ui, &mut self.audit_query, copy::AUDIT_FILTER, 260.0);
            if out.cleared {
                self.audit_query.clear();
            }
            if choice(ui, copy::AUDIT_ALL_KINDS, self.audit_kind.is_none()) {
                self.audit_kind = None;
            }
            for k in &kinds {
                if choice(ui, k, self.audit_kind.as_deref() == Some(k.as_str())) {
                    self.audit_kind = Some(k.clone());
                }
            }
        });
        ui.add_space(theme::space::M);

        let q = self.audit_query.to_lowercase();
        let rows: Vec<&AuditRow> = d
            .audit
            .iter()
            .filter(|a| audit_matches(a, self.audit_kind.as_deref(), &q))
            .collect();

        let matched = rows.len();
        if matched > AUDIT_MAX {
            register::notice(ui, theme::Custody::Stale, |ui| {
                ui.label(
                    egui::RichText::new(copy::audit_truncated(AUDIT_MAX, matched))
                        .font(theme::font(
                            theme::step::META,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
            });
            ui.add_space(theme::space::M);
        }

        let cols = [
            register::Col::new("when", register::ColW::Fixed(150.0)),
            register::Col::new("event", register::ColW::Fixed(118.0)),
            register::Col::new("subject", register::ColW::Flex(3.0)),
            register::Col::new("peer", register::ColW::Fixed(104.0)),
        ];
        let content = if let Some(err) = &d.error {
            register::Content::Failed {
                what: copy::AUDIT_FAILED,
                because: err,
            }
        } else if rows.is_empty() {
            register::Content::Empty {
                title: if d.audit.is_empty() {
                    copy::AUDIT_EMPTY_TITLE
                } else {
                    copy::AUDIT_FILTER_EMPTY
                },
                hint: if d.audit.is_empty() {
                    copy::AUDIT_EMPTY_HINT
                } else {
                    copy::AUDIT_FILTER_EMPTY_HINT
                },
            }
        } else {
            register::Content::Entries(rows.len().min(AUDIT_MAX))
        };
        let out = register::Register::new("audit", &cols)
            .stacked_rows(&[theme::step::LABEL, theme::step::META])
            .show(ui, content, |e| {
                let Some(a) = rows.get(e.index()).copied() else {
                    return;
                };
                let when = fmt_ts(a.ts_ms);
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(&when)
                            .font(theme::font(theme::step::DATA, theme::fam_mono()))
                            .color(theme::ink_faint()),
                    );
                });
                let kind = a.kind.clone();
                let color = audit_color(&kind);
                e.cell(|ui| register::tag(ui, &kind, color));
                let subject = a
                    .path
                    .clone()
                    .or_else(|| a.detail.clone())
                    .unwrap_or_default();
                let detail = match (&a.path, &a.detail) {
                    (Some(_), Some(de)) => Some(de.clone()),
                    _ => None,
                };
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(&subject)
                            .font(theme::font(
                                theme::step::LABEL,
                                egui::FontFamily::Proportional,
                            ))
                            .color(theme::ink()),
                    );
                    if let Some(de) = &detail {
                        ui.label(
                            egui::RichText::new(de)
                                .font(theme::font(
                                    theme::step::META,
                                    egui::FontFamily::Proportional,
                                ))
                                .color(theme::ink_faint()),
                        );
                    }
                });
                let peer = a.peer.as_deref().map(short).unwrap_or_default();
                e.cell(|ui| {
                    ui.label(
                        egui::RichText::new(&peer)
                            .font(theme::font(theme::step::DATA, theme::fam_mono()))
                            .color(theme::ink_muted()),
                    );
                });
            });
        if out.retry {
            self.send(Cmd::Refresh);
        }
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

        register::heading(ui, "Live settings");
        ui.label(
            egui::RichText::new(copy::LIVE_SECTION_NOTE)
                .size(11.0)
                .color(theme::ink_faint()),
        );
        register::well().show(ui, |ui| {
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
            self.cfg_toggle(ui, dir, "audit", "audit log", Some(cfg.audit));
            self.cfg_toggle(ui, dir, "hooks", "event hooks", Some(cfg.hooks));
            self.cfg_toggle(ui, dir, "notify", "desktop notifications", Some(cfg.notify));
        });

        ui.add_space(6.0);
        register::heading(ui, "Fixed until restart");
        register::well().show(ui, |ui| {
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
                    ui.label(egui::RichText::new(k).size(12.0).color(theme::ink_muted()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(v)
                                .size(12.0)
                                .family(egui::FontFamily::Monospace)
                                .color(theme::ink()),
                        );
                    });
                });
            }
            ui.label(
                egui::RichText::new(copy::FIXED_SECTION_NOTE)
                    .size(10.5)
                    .color(theme::ink_faint()),
            );
        });

        ui.add_space(6.0);
        register::heading(ui, "Peers");
        register::well().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(copy::PEERS_NAME_HINT)
                    .size(11.5)
                    .color(theme::ink_muted()),
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
                        .color(theme::ink_faint()),
                );
            }
        });

        if !d.leases.is_empty() {
            ui.add_space(6.0);
            register::heading(ui, "Active leases");
            register::well().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for l in &d.leases {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&l.path).size(12.0).color(theme::ink()));
                        register::tag(
                            ui,
                            &if l.mine {
                                "you".to_string()
                            } else {
                                short(&l.holder)
                            },
                            if l.mine {
                                theme::gold()
                            } else {
                                theme::ink_muted()
                            },
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} left", human_dur(l.expires_in_ms)))
                                    .size(11.0)
                                    .color(theme::ink_faint()),
                            );
                        });
                    });
                }
            });
        }
        self.maintenance_section(ui);
    }

    /// Machine-level upkeep: whether tazamun comes back after a reboot. Not a
    /// per-session setting, which is why it sits at the foot of the page under
    /// its own heading rather than among the folder's config keys.
    fn maintenance_section(&mut self, ui: &mut egui::Ui) {
        register::heading(ui, copy::MAINTENANCE_TITLE);
        ui.label(
            egui::RichText::new(copy::SUPERVISOR_NOTE)
                .font(theme::font(
                    theme::step::META,
                    egui::FontFamily::Proportional,
                ))
                .color(theme::ink_faint()),
        );
        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            if controls::ghost_button(ui, copy::SUPERVISOR_INSTALL).clicked() {
                self.send(Cmd::Supervisor { install: true });
            }
            if controls::ghost_button(ui, copy::SUPERVISOR_REMOVE).clicked() {
                self.ask(
                    copy::SUPERVISOR_REMOVE_TITLE,
                    copy::SUPERVISOR_REMOVE_BODY.to_string(),
                    copy::SUPERVISOR_REMOVE,
                    true,
                    Cmd::Supervisor { install: false },
                );
            }
        });
    }

    /// One editable live-config row: label, current value, edit buffer, apply.
    fn cfg_row(&mut self, ui: &mut egui::Ui, dir: &Path, key: &str, label: &str, current: &str) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(12.0)
                    .color(theme::ink_muted()),
            );
            ui.label(
                egui::RichText::new(format!("({key})"))
                    .size(10.0)
                    .color(theme::ink_faint()),
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

    /// An on/off live key. `current` is None only if the daemon's config
    /// summary omits the key entirely, in which case neither side is marked
    /// rather than one being marked wrongly.
    fn cfg_toggle(
        &mut self,
        ui: &mut egui::Ui,
        dir: &Path,
        key: &str,
        label: &str,
        current: Option<bool>,
    ) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(12.0)
                    .color(theme::ink_muted()),
            );
            ui.label(
                egui::RichText::new(format!("({key})"))
                    .size(10.0)
                    .color(theme::ink_faint()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let on_sel = current == Some(true);
                let off_sel = current == Some(false);
                if choice(ui, copy::TOGGLE_OFF, off_sel) {
                    self.send(Cmd::ConfigSet {
                        dir: dir.to_path_buf(),
                        key: key.to_string(),
                        value: "off".into(),
                    });
                }
                if choice(ui, copy::TOGGLE_ON, on_sel) {
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
    /// Dispatches every global chord from [`shortcuts::sections`].
    ///
    /// The table is the registry: a binding printed on the sheet is the same
    /// binding matched here, so the two cannot drift. Bindings marked
    /// [`shortcuts::Action::Convention`] are left alone — egui, a list widget
    /// or whichever overlay is open already answers them.
    fn keyboard(&mut self, ui: &egui::Ui, overview: &Option<Overview>) {
        // While a text field owns the keyboard, a bare "?" is a character being
        // typed and Ctrl+A selects the field's text.
        let typing = ui.ctx().memory(|m| m.focused()).is_some();
        let has_session = self.selected.is_some();

        let mut fired: Vec<shortcuts::Action> = Vec::new();
        ui.input_mut(|i| {
            for b in shortcuts::sections().iter().flat_map(|s| s.bindings) {
                let Some((mods, keys)) = b.action.chord() else {
                    continue;
                };
                if typing && b.action.yields_to_typing() {
                    continue;
                }
                // Every spelling is consumed, not just the first to match: a
                // short-circuit would leave the other pending for the next
                // frame, where it would fire again.
                let mut hit = false;
                for k in keys {
                    hit |= i.consume_key(mods, *k);
                }
                if hit && !(b.action.needs_session() && !has_session) {
                    fired.push(b.action);
                }
            }
        });

        let now = ui.input(|i| i.time);
        for action in fired {
            match action {
                shortcuts::Action::Palette => {
                    self.palette_open = !self.palette_open;
                    self.palette_query.clear();
                    self.palette_sel = 0;
                    self.palette_focused = false;
                }
                shortcuts::Action::Refresh => self.send(Cmd::Refresh),
                shortcuts::Action::Settings => self.tab = Tab::Settings,
                shortcuts::Action::Sheet => {
                    self.shortcuts_open = !self.shortcuts_open;
                    // Keep the two modals mutually exclusive so Escape is
                    // unambiguous.
                    self.palette_open = false;
                    self.palette_focused = false;
                }
                shortcuts::Action::SelectAll => {
                    if let Some(ov) = overview {
                        let keys: Vec<String> =
                            ov.sessions.iter().map(|s| s.path.clone()).collect();
                        self.multi.select_all(&keys);
                    }
                }
                shortcuts::Action::FileFilter => {
                    self.tab = Tab::Files;
                    self.focus_filter = true;
                }
                shortcuts::Action::Tab(n) => {
                    if let Some(t) = Tab::ALL.get(n) {
                        self.tab = *t;
                    }
                }
                shortcuts::Action::TextBigger
                | shortcuts::Action::TextSmaller
                | shortcuts::Action::TextReset => {
                    let next = match action {
                        shortcuts::Action::TextReset => a11y::SCALE_DEFAULT,
                        shortcuts::Action::TextBigger => a11y::step_scale(self.text_scale, true),
                        _ => a11y::step_scale(self.text_scale, false),
                    };
                    if next != self.text_scale {
                        self.text_scale = next;
                        self.style_dirty = true;
                        self.toasts
                            .push(a11y::scale_label(next), toasts::Kind::Info, now);
                    }
                }
                shortcuts::Action::Convention => {}
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
            self.palette_focused = false;
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
            /// A session operation that needs nothing but the open folder.
            Session(&'static str),
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
        if self.selected.is_some() {
            for (label, key) in [
                (copy::MENU_DOCTOR, "doctor"),
                (copy::MENU_DASHBOARD, "dashboard"),
                (copy::MENU_GC, "gc"),
            ] {
                acts.push((label.into(), Act::Session(key)));
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
            self.palette_focused = false;
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
                    .fill(theme::bg_chrome())
                    .stroke(egui::Stroke::new(1.0, theme::gold().linear_multiply(0.35)))
                    .corner_radius(theme::R_NONE + 2)
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
                        if !te.has_focus() && !self.palette_focused {
                            te.request_focus();
                            self.palette_focused = true;
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
                            theme::gold().linear_multiply(0.12),
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
                                            egui::RichText::new(label).size(13.0).color(
                                                if selected {
                                                    theme::ink()
                                                } else {
                                                    theme::ink_muted()
                                                },
                                            ),
                                        )
                                        .fill(if selected {
                                            theme::bg_raise()
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
                                            theme::gold(),
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
                            ui.label(
                                egui::RichText::new("move")
                                    .size(10.0)
                                    .color(theme::ink_faint()),
                            );
                            ui.add_space(8.0);
                            ceremony::keycap(ui, "Enter");
                            ui.label(
                                egui::RichText::new("run")
                                    .size(10.0)
                                    .color(theme::ink_faint()),
                            );
                            ui.add_space(8.0);
                            ceremony::keycap(ui, "Esc");
                            ui.label(
                                egui::RichText::new("close")
                                    .size(10.0)
                                    .color(theme::ink_faint()),
                            );
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
                Act::Session(key) => {
                    if let Some(sel) = self.selected.clone() {
                        let dir = PathBuf::from(sel);
                        self.send(match *key {
                            "doctor" => Cmd::Doctor { dir },
                            "dashboard" => Cmd::Dashboard { dir },
                            _ => Cmd::Gc { dir },
                        });
                    }
                }
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
            self.palette_focused = false;
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
                    .fill(theme::bg_chrome())
                    .stroke(egui::Stroke::new(theme::RULE_W, theme::rule_hair()))
                    .corner_radius(theme::R_NONE + 2)
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
                    .fill(theme::bg_chrome())
                    .stroke(egui::Stroke::new(theme::RULE_W, theme::rule_hair()))
                    .corner_radius(theme::R_NONE + 2)
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
                                .color(theme::ink()),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(copy::SHORTCUTS_SUB)
                                .size(11.5)
                                .color(theme::ink_muted()),
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
    ///
    /// Built on [`egui::Modal`] rather than a hand-rolled backdrop. The
    /// hand-rolled one blocked the pointer but not the keyboard, so Tab walked
    /// straight out of an open "discard this preserved copy?" dialog into the
    /// live controls behind it — and those controls had no visible focus. A
    /// real modal layer makes that impossible.
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

        let mut decided: Option<bool> = None;
        let modal = egui::Modal::new(egui::Id::new("confirm"))
            .backdrop_color(theme::scrim())
            .frame(register::overlay().stroke(egui::Stroke::new(
                theme::RULE_W,
                if danger {
                    theme::custody_blocked()
                } else {
                    theme::rule_emphasis()
                },
            )))
            .show(ui.ctx(), |ui| {
                ui.set_width(420.0);
                ui.label(
                    egui::RichText::new(&title)
                        .font(theme::font(theme::step::TITLE, theme::fam_serif()))
                        .color(theme::ink()),
                );
                ui.add_space(theme::space::S);
                ui.label(
                    egui::RichText::new(&body)
                        .font(theme::font(
                            theme::step::BODY,
                            egui::FontFamily::Proportional,
                        ))
                        .color(theme::ink_muted()),
                );
                ui.add_space(theme::space::L);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = if danger {
                            controls::bevel_danger(ui, &verb)
                        } else {
                            components::bevel_primary(ui, &verb)
                        };
                        // Cancel takes focus, not the destructive verb: a
                        // dialog that arms Enter on the irreversible choice is
                        // a trap for anyone who confirms by reflex.
                        let cancel = controls::ghost_button(ui, copy::ACTION_CANCEL);
                        if !r.has_focus() && !cancel.has_focus() {
                            cancel.request_focus();
                        }
                        if r.clicked() {
                            decided = Some(true);
                        }
                        if cancel.clicked() {
                            decided = Some(false);
                        }
                    });
                });
                // Adorned over the finished card: the alphas are ghost-level,
                // so the flourishes never fight the text.
                ceremony::adorn_dialog(ui.painter(), ui.min_rect(), danger);
            });

        if modal.should_close() {
            decided = decided.or(Some(false));
        }
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
        theme::custody_stale()
    } else if kind.contains("error") || kind.contains("refus") {
        theme::custody_blocked()
    } else if kind.contains("lock") || kind.contains("publish") || kind.contains("restore") {
        theme::custody_good()
    } else {
        theme::custody_peer()
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
        ("○", theme::custody_blocked())
    } else if s.paused {
        ("⏸", theme::custody_stale())
    } else if s.running && s.peers_online > 0 {
        ("●", theme::custody_good())
    } else if s.running {
        ("●", theme::custody_stale())
    } else {
        ("○", theme::ink_muted())
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
    /// `TAZAMUN_GUI_SHOT_TAB`, `_SELECT`, `_MODE` and `_DENSITY` pin what is on
    /// screen, so a capture does not depend on the last run's preferences.
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

#[cfg(test)]
mod view_tests {
    use super::*;

    fn audit(kind: &str, path: Option<&str>, peer: Option<&str>, detail: Option<&str>) -> AuditRow {
        AuditRow {
            ts_ms: 0,
            kind: kind.into(),
            path: path.map(str::to_string),
            peer: peer.map(str::to_string),
            detail: detail.map(str::to_string),
        }
    }

    fn conflict(name: &str, ts_ms: u64, size: u64) -> ConflictRow {
        ConflictRow {
            name: name.into(),
            path: String::new(),
            reason: String::new(),
            ts_ms,
            size,
        }
    }

    #[test]
    fn an_empty_filter_keeps_every_entry() {
        let a = audit("lock", Some("a.txt"), None, None);
        assert!(audit_matches(&a, None, ""));
    }

    #[test]
    fn the_kind_filter_is_exact() {
        let a = audit("lock", None, None, None);
        assert!(audit_matches(&a, Some("lock"), ""));
        assert!(!audit_matches(&a, Some("unlock"), ""));
        // A prefix is not a match: "lock" must not select "lock_denied".
        let b = audit("lock_denied", None, None, None);
        assert!(!audit_matches(&b, Some("lock"), ""));
    }

    #[test]
    fn the_needle_searches_every_text_field() {
        let a = audit(
            "lock",
            Some("notes/plan.md"),
            Some("3f9a0b2c"),
            Some("ttl 90s"),
        );
        assert!(audit_matches(&a, None, "plan"));
        assert!(audit_matches(&a, None, "3f9a"));
        assert!(audit_matches(&a, None, "ttl"));
        assert!(!audit_matches(&a, None, "absent"));
    }

    /// The caller lower-cases the needle once; an entry with capitals must
    /// still match, or a search for "README" silently finds nothing.
    #[test]
    fn matching_ignores_case_in_the_entry() {
        let a = audit("lock", Some("README.md"), None, None);
        assert!(audit_matches(&a, None, "readme"));
    }

    #[test]
    fn kind_and_needle_must_both_hold() {
        let a = audit("lock", Some("a.txt"), None, None);
        assert!(audit_matches(&a, Some("lock"), "a.txt"));
        assert!(!audit_matches(&a, Some("unlock"), "a.txt"));
        assert!(!audit_matches(&a, Some("lock"), "b.txt"));
    }

    #[test]
    fn an_entry_with_no_text_matches_only_an_empty_needle() {
        let a = audit("gc", None, None, None);
        assert!(audit_matches(&a, None, ""));
        assert!(!audit_matches(&a, None, "anything"));
    }

    #[test]
    fn nothing_is_prunable_when_nothing_is_old_enough() {
        let rows = [conflict("a", 900, 10), conflict("b", 950, 20)];
        let (names, bytes) = prunable(&rows, 1_000, 500);
        assert!(names.is_empty());
        assert_eq!(bytes, 0);
    }

    #[test]
    fn prunable_selects_only_what_is_past_the_cutoff() {
        let rows = [conflict("old", 100, 10), conflict("new", 950, 20)];
        let (names, bytes) = prunable(&rows, 1_000, 500);
        assert_eq!(names, vec!["old".to_string()]);
        assert_eq!(bytes, 10);
    }

    /// Exactly at the cutoff counts as old enough, and the boundary is worth
    /// pinning: this decides whether a copy is deleted.
    #[test]
    fn the_cutoff_boundary_is_inclusive() {
        let rows = [conflict("edge", 500, 7)];
        let (names, _) = prunable(&rows, 1_000, 500);
        assert_eq!(names, vec!["edge".to_string()]);
    }

    /// A clock that has gone backwards must not make everything prunable.
    #[test]
    fn a_future_timestamp_is_never_prunable() {
        let rows = [conflict("future", 5_000, 1)];
        let (names, bytes) = prunable(&rows, 1_000, 0);
        assert_eq!(names, vec!["future".to_string()], "a zero cutoff takes all");
        let (names, _) = prunable(&rows, 1_000, 500);
        assert!(names.is_empty(), "{names:?}");
        assert_eq!(bytes, 1);
    }

    #[test]
    fn prunable_totals_do_not_overflow() {
        let rows = [conflict("a", 0, u64::MAX), conflict("b", 0, u64::MAX)];
        let (names, bytes) = prunable(&rows, 1_000, 0);
        assert_eq!(names.len(), 2);
        assert_eq!(bytes, u64::MAX);
    }

    /// A viewer or archive folder refuses every lease, so the window must not
    /// offer the verbs.
    #[test]
    fn only_editing_roles_may_take_a_lease() {
        assert!(role_can_edit("editor"));
        assert!(role_can_edit("owner"));
        assert!(!role_can_edit("viewer"));
        assert!(!role_can_edit("archive"));
    }

    /// A stopped session cannot grant anything, so its files read as refused
    /// rather than free — offering Lock there would always fail.
    #[test]
    fn custody_of_a_stopped_session_is_blocked() {
        let f = FileRow {
            path: "a.txt".into(),
            size: 1,
            locked_by: None,
            mine_lock: false,
        };
        assert_eq!(file_custody(&f, false), theme::Custody::Blocked);
        assert_eq!(file_custody(&f, true), theme::Custody::Free);
    }

    #[test]
    fn custody_distinguishes_your_lease_from_a_peers() {
        let mine = FileRow {
            path: "a".into(),
            size: 0,
            locked_by: Some("me".into()),
            mine_lock: true,
        };
        let theirs = FileRow {
            mine_lock: false,
            ..mine.clone()
        };
        assert_eq!(file_custody(&mine, true), theme::Custody::Mine);
        assert_eq!(file_custody(&theirs, true), theme::Custody::Peer);
    }
}
