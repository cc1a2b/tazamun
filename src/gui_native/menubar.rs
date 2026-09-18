//! The menu bar: the register's own head-band, ruled across the title bar.
//!
//! This window is a deed book, so the bar at the top of it is the head-band
//! over the page rather than a strip of File/Edit/View. Each head is a *ruled
//! compartment*: the lowercase tracked word of [`super::register::Col`] with a
//! rule capping it and a rule under its foot, and the house diamond pivoting
//! between two hairline stubs in the gap to the next one. Reaching a head
//! closes those two rules on the word from opposite ends, the way a scribe
//! rules a compartment before writing in it; opening one widens them to the
//! whole compartment in gold, so the head reads as the mouth the page dropped
//! out of.
//!
//! What drops out is a *leaf*: the engraved title with the head rule running
//! through the line to a folio numeral at its outer end, a ruled gutter down
//! the spine of each block of entries, the choice in force marked by a swell in
//! that gutter rather than by a bar at the page edge, and a cusp of
//! [`super::ornament::corner_flourish`] at each end of the spine — the same
//! illumination [`super::ceremony::adorn_dialog`] puts on a dialog, because a
//! menu here is the same kind of floating leaf and not a control.
//!
//! What it holds is what was buried. The session lifecycle was three buttons on
//! one tab; the operations the CLI has always had — doctor, dashboard, gc,
//! rekey, prune, the supervisor — were behind a "More" button on another; the
//! display preferences were reachable only from Home; and whether a newer
//! release exists could not be asked from the window at all.
//!
//! The content is data, not painting. [`rows`] turns a [`BarState`] into a list
//! of [`Row`]s and [`plan`] decides which heads a given width can afford, both
//! without touching a [`egui::Ui`] — so what an item says, whether it can run,
//! and what a narrow bar drops first are all unit-testable.
//!
//! The module never acts. [`bar`] returns the [`MenuAction`] the user chose and
//! nothing else; what an action *means* stays in `gui_native.rs`.

use eframe::egui;
use egui::{Id, Key, Modifiers, Rangef, Rect, Sense, Stroke, pos2, vec2};

use super::model::UpdateState;
use super::{a11y, chrome, copy, figures, focusnav, ornament, shortcuts, theme};

// ─── what the bar can ask the window for ─────────────────────────────────────

/// Everything the bar can ask for. One variant per item; the window decides
/// what each one costs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MenuAction {
    /// Start the open session.
    Start,
    /// Stop it.
    Stop,
    /// Suspend syncing without stopping the daemon.
    Pause,
    /// Resume a paused session.
    Resume,
    /// Reveal the open session's folder in the file manager.
    OpenFolder,
    /// Copy the open session's folder path.
    CopyPath,
    /// Rename the path named by [`SessionState::marked_file`], under a lease.
    Rename,
    /// Run the daemon's connectivity self-check.
    Doctor,
    /// Start the web dashboard and report its address.
    Dashboard,
    /// Drop unreferenced blobs.
    Gc,
    /// Rotate the session secret.
    Rekey,
    /// Open the confirm that deletes preserved copies past a cutoff.
    Prune,
    /// Install or remove the OS supervisor.
    Supervisor {
        /// True installs it, false removes it.
        install: bool,
    },
    /// Switch the palette.
    Palette(theme::Mode),
    /// Switch the register density.
    Density(theme::Density),
    /// Turn decorative motion on or off.
    Motion {
        /// True is the reduced setting.
        reduced: bool,
    },
    /// Set the text scale, already stepped and clamped by [`a11y`].
    TextScale(f32),
    /// Open the keyboard sheet.
    Shortcuts,
    /// Open the colophon.
    Colophon,
    /// Ask whether a newer release exists.
    CheckUpdate,
    /// Install the release the last check found.
    ApplyUpdate,
}

// ─── what the bar needs to know ──────────────────────────────────────────────

/// Everything the bar reads, borrowed for the frame. No handles, no channels,
/// no clock: the bar renders what it is handed and hands back a choice.
pub struct BarState<'a> {
    /// The open session, or `None` while Home is showing.
    pub session: Option<SessionState<'a>>,
    /// Version and update status, from `Shared::update`.
    pub update: &'a UpdateState,
    /// Whether the OS supervisor is installed on this machine.
    pub supervisor: bool,
    /// The palette in force.
    pub mode: theme::Mode,
    /// The register density in force.
    pub density: theme::Density,
    /// Whether reduced motion is on.
    pub reduced_motion: bool,
    /// The text scale in force.
    pub text_scale: f32,
    /// How much of the bar's trailing edge is already spoken for — the window
    /// buttons, the palette key, the busy mark, any padding after them.
    ///
    /// They sit in a right-to-left layout that is allocated *after* this one,
    /// so `available_width` still counts their strip and the bar would run
    /// underneath them. [`chrome::window_buttons_width`] is what the buttons
    /// take; the bar holds that much clear whatever it is passed, because a
    /// Close that cannot be clicked is a window that cannot be shut and that
    /// must not rest on a caller's arithmetic.
    pub reserve: f32,
}

/// The open session, as the bar needs it.
pub struct SessionState<'a> {
    /// Whether its daemon is up.
    pub running: bool,
    /// Whether syncing is suspended.
    pub paused: bool,
    /// The role this folder is joined as, for the refusal that names it.
    pub role: &'a str,
    /// Whether that role may take a lease at all (`role_can_edit`). The policy
    /// lives in `gui_native.rs`; the bar only reports its answer.
    pub may_edit: bool,
    /// How many preserved copies are waiting in it.
    pub conflicts: usize,
    /// The path the Files register has marked, if any — what Rename acts on.
    pub marked_file: Option<&'a str>,
}

// ─── the heads ───────────────────────────────────────────────────────────────

/// The leaf the capture hook was told to open, if any.
fn shot_menu() -> Option<String> {
    std::env::var("TAZAMUN_GUI_SHOT_MENU")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The heads, in the order they are ruled across the bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Menu {
    /// The open session's lifecycle and its folder.
    Session,
    /// The operations that act on a session or on this machine.
    Tools,
    /// Palette, density, motion, text size.
    Display,
    /// The sheet, the colophon, and updates.
    Help,
}

impl Menu {
    /// Reading order across the bar.
    pub const ALL: [Self; 4] = [Self::Session, Self::Tools, Self::Display, Self::Help];

    /// The word on the head.
    pub fn head(self) -> &'static str {
        match self {
            Self::Session => copy::MENUBAR_SESSION,
            Self::Tools => copy::MENUBAR_TOOLS,
            Self::Display => copy::MENUBAR_DISPLAY,
            Self::Help => copy::MENUBAR_HELP,
        }
    }

    /// The same word as a serif section title, for the `more` popup.
    pub fn title(self) -> &'static str {
        match self {
            Self::Session => copy::MENUBAR_SESSION_TITLE,
            Self::Tools => copy::MENUBAR_TOOLS_TITLE,
            Self::Display => copy::MENUBAR_DISPLAY_TITLE,
            Self::Help => copy::MENUBAR_HELP_TITLE,
        }
    }

    /// A stable key for this head's widget id.
    fn key(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Tools => "tools",
            Self::Display => "display",
            Self::Help => "help",
        }
    }
}

// ─── the content (pure) ──────────────────────────────────────────────────────

/// How loudly a line speaks. Colour in this window means custody and nothing
/// else, so a menu line has only three voices: the quiet default, the gold that
/// marks something on offer, and the stale colour that marks something wrong.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// The ordinary voice.
    Muted,
    /// Something is waiting to be taken up.
    Offer,
    /// Something did not work.
    Trouble,
}

impl Tone {
    fn color(self) -> egui::Color32 {
        match self {
            Self::Muted => theme::ink_muted(),
            Self::Offer => theme::gold(),
            Self::Trouble => theme::custody_stale(),
        }
    }
}

/// One line of a menu.
#[derive(Clone, PartialEq, Debug)]
pub enum Row {
    /// A serif title over an emphasis rule — used only in the `more` popup,
    /// where several menus share one page and each needs naming.
    Title(&'static str),
    /// A tracked lowercase section head, with an optional value in its right
    /// column: the register's ruler.
    Head {
        /// The word.
        title: &'static str,
        /// What the section currently reads, when it has a value.
        value: Option<String>,
    },
    /// Something the user can choose.
    Item(Item),
    /// A line that only reports.
    Note {
        /// What it says.
        text: String,
        /// How loudly.
        tone: Tone,
    },
}

/// One verb, and everything that decides how it is drawn.
#[derive(Clone, PartialEq, Debug)]
pub struct Item {
    /// What the row reads.
    pub label: String,
    /// What the window is asked for when it is chosen.
    pub action: MenuAction,
    /// The chord that does the same thing, spelled the way the sheet spells it.
    pub chord: Option<String>,
    /// Why it cannot run, when it cannot. An item is never hidden for being
    /// unavailable — the reason is the interesting part.
    pub blocked: Option<String>,
    /// `Some(true)` for the choice in force in its group, `Some(false)` for its
    /// siblings, `None` for a plain verb.
    pub choice: Option<bool>,
}

impl Item {
    /// Whether this item can be chosen.
    pub fn enabled(&self) -> bool {
        self.blocked.is_none()
    }

    /// Whether the house diamond marks this row.
    fn marked(&self) -> bool {
        self.choice == Some(true)
    }
}

impl Row {
    /// Prints the chord this row's action is already bound to.
    fn keyed(mut self, action: shortcuts::Action) -> Self {
        if let Self::Item(item) = &mut self {
            item.chord = chord_caps(action);
        }
        self
    }
}

/// A plain verb.
fn verb(label: &str, action: MenuAction, blocked: Option<String>) -> Row {
    Row::Item(Item {
        label: label.to_owned(),
        action,
        chord: None,
        blocked,
        choice: None,
    })
}

/// One of a group of mutually exclusive settings.
fn choice(label: &str, action: MenuAction, current: bool) -> Row {
    Row::Item(Item {
        label: label.to_owned(),
        action,
        chord: None,
        blocked: None,
        choice: Some(current),
    })
}

/// A section head with no value.
fn head(title: &'static str) -> Row {
    Row::Head { title, value: None }
}

/// The chord bound to `action`, spelled the way the keyboard sheet spells it.
///
/// Read out of [`shortcuts::sections`] rather than written again here: the
/// sheet and the window already share one registry, and a menu printing its own
/// copy of a chord is a menu that will one day advertise a key that does
/// nothing.
fn chord_caps(action: shortcuts::Action) -> Option<String> {
    shortcuts::sections()
        .iter()
        .flat_map(|s| s.bindings)
        .find(|b| b.action == action)
        .map(|b| b.keys.join(" "))
}

/// Every row of one menu, in reading order.
pub fn rows(menu: Menu, state: &BarState<'_>, now: f64) -> Vec<Row> {
    match menu {
        Menu::Session => session_rows(state),
        Menu::Tools => tools_rows(state),
        Menu::Display => display_rows(state),
        Menu::Help => help_rows(state, now),
    }
}

/// The lifecycle, then the folder.
///
/// All four lifecycle verbs are listed even though at most two of them can run:
/// the header's button row shows only the verb that applies, because a dead
/// button there wastes the prime space on the page, but a menu is a map. A map
/// that redraws itself cannot be learned, and a reader who cannot find Pause
/// has no way to discover that it is Start that is missing.
fn session_rows(state: &BarState<'_>) -> Vec<Row> {
    let session = state.session.as_ref();
    let running = session.is_some_and(|s| s.running);
    let paused = session.is_some_and(|s| s.paused);
    let unopened = || Some(copy::MENUBAR_NEEDS_SESSION.to_owned());
    let stopped = || Some(copy::MENUBAR_NEEDS_RUNNING.to_owned());

    let start = if session.is_none() {
        unopened()
    } else if running {
        Some(copy::MENUBAR_ALREADY_RUNNING.to_owned())
    } else {
        None
    };
    let stop = if session.is_none() {
        unopened()
    } else if !running {
        Some(copy::MENUBAR_NOT_RUNNING.to_owned())
    } else {
        None
    };
    let pause = if session.is_none() {
        unopened()
    } else if !running {
        stopped()
    } else if paused {
        Some(copy::MENUBAR_ALREADY_PAUSED.to_owned())
    } else {
        None
    };
    let resume = if session.is_none() {
        unopened()
    } else if !running {
        stopped()
    } else if !paused {
        Some(copy::MENUBAR_NOT_PAUSED.to_owned())
    } else {
        None
    };
    // The order of these checks is the order the reader would have to clear
    // them in, so the reason shown is always the first thing in the way.
    let rename = match session {
        None => unopened(),
        Some(s) if !s.running => stopped(),
        Some(s) if !s.may_edit => Some(copy::role_cannot_edit(s.role)),
        Some(s) if s.marked_file.is_none() => Some(copy::MENUBAR_NEEDS_FILE.to_owned()),
        Some(_) => None,
    };
    let folder = if session.is_none() { unopened() } else { None };

    vec![
        head(copy::MENUBAR_LIFECYCLE),
        verb(copy::MENU_START, MenuAction::Start, start),
        verb(copy::MENU_STOP, MenuAction::Stop, stop),
        verb(copy::ACTION_PAUSE, MenuAction::Pause, pause),
        verb(copy::ACTION_RESUME, MenuAction::Resume, resume),
        head(copy::MENUBAR_FOLDER),
        verb(
            copy::MENU_OPEN_FOLDER,
            MenuAction::OpenFolder,
            folder.clone(),
        ),
        verb(copy::MENU_COPY_FOLDER, MenuAction::CopyPath, folder),
        verb(copy::MENU_RENAME, MenuAction::Rename, rename),
    ]
}

/// The operations that need a daemon, then the one that needs only this
/// machine.
fn tools_rows(state: &BarState<'_>) -> Vec<Row> {
    let session = state.session.as_ref();
    let unopened = || Some(copy::MENUBAR_NEEDS_SESSION.to_owned());
    // Everything in the first block is answered by the session's daemon, so a
    // stopped session refuses them all with the same sentence.
    let live = match session {
        None => unopened(),
        Some(s) if !s.running => Some(copy::MENUBAR_NEEDS_RUNNING.to_owned()),
        Some(_) => None,
    };
    // Rotating the key rewrites the session's own state; it does not ask the
    // network anything, so it is the one operation here a stopped session can
    // still perform.
    let rekey = if session.is_none() { unopened() } else { None };
    let prune = match session {
        Some(s) if live.is_none() && s.conflicts == 0 => {
            Some(copy::MENUBAR_NO_CONFLICTS.to_owned())
        }
        _ => live.clone(),
    };
    let (supervisor_label, install) = if state.supervisor {
        (copy::SUPERVISOR_REMOVE, false)
    } else {
        (copy::SUPERVISOR_INSTALL, true)
    };

    vec![
        head(copy::MENUBAR_THIS_SESSION),
        verb(copy::MENU_DOCTOR, MenuAction::Doctor, live.clone()),
        verb(copy::MENU_DASHBOARD, MenuAction::Dashboard, live.clone()),
        verb(copy::MENU_GC, MenuAction::Gc, live),
        verb(copy::MENU_REKEY, MenuAction::Rekey, rekey),
        verb(copy::MENU_PRUNE, MenuAction::Prune, prune),
        head(copy::MENUBAR_THIS_MACHINE),
        verb(supervisor_label, MenuAction::Supervisor { install }, None),
    ]
}

/// The window's own appearance. These used to be reachable only from the Home
/// page, which meant a reader who had opened a session had to leave it to make
/// the text bigger.
fn display_rows(state: &BarState<'_>) -> Vec<Row> {
    let mut rows = vec![head(copy::MENUBAR_PALETTE)];
    for mode in theme::Mode::ALL {
        rows.push(choice(
            mode.label(),
            MenuAction::Palette(mode),
            mode == state.mode,
        ));
    }
    rows.push(head(copy::MENUBAR_DENSITY));
    for density in theme::Density::ALL {
        rows.push(choice(
            density.label(),
            MenuAction::Density(density),
            density == state.density,
        ));
    }
    rows.push(head(copy::MENUBAR_MOTION));
    rows.push(choice(
        copy::MOTION_FULL,
        MenuAction::Motion { reduced: false },
        !state.reduced_motion,
    ));
    rows.push(choice(
        copy::MOTION_REDUCED,
        MenuAction::Motion { reduced: true },
        state.reduced_motion,
    ));

    let scale = a11y::clamp_scale(state.text_scale);
    let bigger = a11y::step_scale(scale, true);
    let smaller = a11y::step_scale(scale, false);
    rows.push(Row::Head {
        title: copy::MENUBAR_TEXT,
        value: Some(a11y::scale_label(scale)),
    });
    rows.push(
        verb(
            copy::MENU_TEXT_BIGGER,
            MenuAction::TextScale(bigger),
            (bigger == scale).then(|| copy::MENUBAR_TEXT_AT_MAX.to_owned()),
        )
        .keyed(shortcuts::Action::TextBigger),
    );
    rows.push(
        verb(
            copy::MENU_TEXT_SMALLER,
            MenuAction::TextScale(smaller),
            (smaller == scale).then(|| copy::MENUBAR_TEXT_AT_MIN.to_owned()),
        )
        .keyed(shortcuts::Action::TextSmaller),
    );
    rows.push(
        verb(
            copy::MENU_TEXT_RESET,
            MenuAction::TextScale(a11y::SCALE_DEFAULT),
            (scale == a11y::SCALE_DEFAULT).then(|| copy::MENUBAR_TEXT_AT_DEFAULT.to_owned()),
        )
        .keyed(shortcuts::Action::TextReset),
    );
    rows
}

/// The sheet, the colophon, and the update block the bar exists to carry.
fn help_rows(state: &BarState<'_>, now: f64) -> Vec<Row> {
    let mut rows = vec![
        head(copy::MENUBAR_KEYBOARD),
        verb(copy::MENU_KEYS, MenuAction::Shortcuts, None).keyed(shortcuts::Action::Sheet),
        head(copy::MENUBAR_ABOUT),
        verb(copy::MENU_ABOUT, MenuAction::Colophon, None),
        head(copy::MENUBAR_UPDATES),
    ];
    rows.extend(update_rows(state.update, now));
    rows
}

/// The update block: one verb, and the line under it that says where the
/// window stands.
///
/// `tazamun update` was a CLI-only command, so a window left open for weeks had
/// no way to learn it was stale and no way to say so. Every state the check can
/// be in is named here — never checked, checking, up to date, newer release
/// waiting, installed and waiting on a restart, and failed — because an item
/// that only ever reads "Check for updates" tells a reader nothing about the
/// answer they already asked for.
pub fn update_rows(update: &UpdateState, now: f64) -> Vec<Row> {
    let latest = update
        .latest
        .clone()
        .unwrap_or_else(|| update.current.clone());
    let action = if update.busy {
        verb(
            copy::MENU_UPDATE_CHECKING,
            MenuAction::CheckUpdate,
            Some(copy::MENUBAR_UPDATE_BUSY.to_owned()),
        )
    } else if update.available() {
        verb(
            &copy::menu_update_install(&latest),
            MenuAction::ApplyUpdate,
            None,
        )
    } else {
        verb(copy::MENU_UPDATE_CHECK, MenuAction::CheckUpdate, None)
    };

    let (text, tone) = if let Some(why) = &update.error {
        (copy::update_failed(why), Tone::Trouble)
    } else if update.busy {
        (copy::UPDATE_CHECKING_NOTE.to_owned(), Tone::Muted)
    } else if update.applied {
        (copy::update_applied(&latest), Tone::Offer)
    } else if update.available() {
        (copy::update_offer(&update.current, &latest), Tone::Offer)
    } else if let Some(at) = update.checked_at {
        // Both clocks are egui's seconds-since-start, so only their difference
        // is meaningful — which is all `ago` reads.
        let when = figures::ago(at.max(0.0) as u64, now.max(0.0) as u64);
        (copy::update_up_to_date(&update.current, &when), Tone::Muted)
    } else {
        (copy::update_unchecked(&update.current), Tone::Muted)
    };

    vec![action, Row::Note { text, tone }]
}

/// What the `help` head must report on its own face, without being opened.
///
/// An offer outranks a failure: if the window already knows a newer release
/// exists, that is the more useful thing to show, and the failure is still
/// spelled out on the line inside.
pub fn update_mark(update: &UpdateState) -> Option<Tone> {
    if update.available() || update.applied {
        Some(Tone::Offer)
    } else if update.error.is_some() {
        Some(Tone::Trouble)
    } else {
        None
    }
}

// ─── the plan (pure) ─────────────────────────────────────────────────────────

/// Which heads a given width can afford to rule, and which fold into `more`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Plan {
    /// Ruled across the bar, in reading order.
    pub shown: Vec<Menu>,
    /// Folded into the trailing `more` head, in reading order.
    pub folded: Vec<Menu>,
}

impl Plan {
    /// Nothing can be drawn at all — there is no room even for `more`.
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty() && self.folded.is_empty()
    }
}

/// The order heads are given up in when the bar runs out of room.
///
/// Later heads fold first, so the session a reader is working in outlives the
/// preferences — except that a head with something to report is moved to the
/// front, because a bar that hides the one mark it was drawn to show has
/// nothing left to say.
fn keep_order(pinned: Option<Menu>) -> Vec<Menu> {
    let mut order: Vec<Menu> = Vec::with_capacity(Menu::ALL.len());
    if let Some(p) = pinned {
        order.push(p);
    }
    order.extend(Menu::ALL.iter().copied().filter(|m| Some(*m) != pinned));
    order
}

/// Fits the heads into `avail`, folding what will not go into `more`.
///
/// `widths` is each head's drawn width in reading order, `gap` the space the
/// layout puts between two of them, `more_w` the width of the trailing `more`
/// head. Folding stops at the first head that will not fit rather than skipping
/// it for a narrower one behind: a bar that drops `tools` but keeps `help` is a
/// bar the reader cannot predict.
pub fn plan(
    widths: &[(Menu, f32)],
    gap: f32,
    more_w: f32,
    avail: f32,
    pinned: Option<Menu>,
) -> Plan {
    if widths.is_empty() || !avail.is_finite() || avail <= 0.0 {
        return Plan::default();
    }
    let gap = if gap.is_finite() { gap.max(0.0) } else { 0.0 };
    let sum: f32 = widths.iter().map(|(_, w)| w.max(0.0)).sum();
    let total = sum + gap * (widths.len() - 1) as f32;
    if total <= avail {
        return Plan {
            shown: widths.iter().map(|(m, _)| *m).collect(),
            folded: Vec::new(),
        };
    }
    if !more_w.is_finite() || avail < more_w {
        return Plan::default();
    }

    let mut budget = avail - more_w;
    let mut keep: Vec<Menu> = Vec::new();
    for menu in keep_order(pinned) {
        let Some((_, w)) = widths.iter().find(|(m, _)| *m == menu) else {
            continue;
        };
        // A kept head costs its own width plus the gap to whatever follows it,
        // which is either the next head or `more`.
        let need = w.max(0.0) + gap;
        if need > budget {
            break;
        }
        budget -= need;
        keep.push(menu);
    }
    let shown: Vec<Menu> = widths
        .iter()
        .map(|(m, _)| *m)
        .filter(|m| keep.contains(m))
        .collect();
    let folded: Vec<Menu> = widths
        .iter()
        .map(|(m, _)| *m)
        .filter(|m| !keep.contains(m))
        .collect();
    Plan { shown, folded }
}

// ─── keyboard focus among the items (pure) ───────────────────────────────────

/// The item focus moves to, given the ids in reading order and a step of `+1`
/// or `-1`. Wraps at both ends, and starts at the appropriate end when nothing
/// in the menu holds focus yet.
fn step_focus(ids: &[Id], focused: Option<Id>, step: isize) -> Option<Id> {
    if ids.is_empty() {
        return None;
    }
    let last = ids.len() - 1;
    let at = focused.and_then(|f| ids.iter().position(|id| *id == f));
    let next = match (at, step >= 0) {
        (None, true) => 0,
        (None, false) => last,
        (Some(i), true) => {
            if i == last {
                0
            } else {
                i + 1
            }
        }
        (Some(i), false) => {
            if i == 0 {
                last
            } else {
                i - 1
            }
        }
    };
    ids.get(next).copied()
}

// ─── painting ────────────────────────────────────────────────────────────────

/// The gutter either side of a head's word. Measured off the type rather than
/// fixed, so a compartment keeps its proportion to the word in it when the
/// reader enlarges the text — a literal that fit at 100% squeezed the word at
/// 220%.
fn head_pad_x() -> f32 {
    theme::sized(theme::step::META)
}
/// The head strip's own height inside the title bar, leaving the girih band at
/// the bar's foot clear.
fn strip_h() -> f32 {
    theme::sized(theme::step::META) + theme::space::L * 2.0
}
/// The diamond that marks a head, pivots a separator, or stands for a choice in
/// force — one mark size for the whole module, sized off the type so it keeps
/// its proportion at every text scale.
fn mark_r() -> f32 {
    theme::sized(theme::step::CAPTION) * 0.26
}
/// The mark margin of a leaf: the column left of the gutter rule that the
/// choice diamond and the null mark sit in, and nothing else.
fn margin_w() -> f32 {
    theme::sized(theme::step::LABEL)
}
/// Where an entry's text begins — past the mark margin and the rule that closes
/// it.
fn row_gutter() -> f32 {
    margin_w() + theme::space::M
}
/// The narrowest a leaf may be ruled. A page has a measure: a two-word menu
/// that came out as wide as its longest word would read as a tooltip.
fn min_page_w() -> f32 {
    theme::sized(theme::step::LABEL) * 16.0
}

/// What was clicked: one of the heads, or the head holding the folded ones.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Menu(Menu),
    More,
}

impl Trigger {
    fn key(self) -> &'static str {
        match self {
            Self::Menu(m) => m.key(),
            Self::More => "more",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Menu(m) => m.head(),
            Self::More => copy::MENUBAR_MORE,
        }
    }
}

/// What the keyboard left half-done between two frames.
///
/// The bar owns no state — the window hands it borrowed data and takes an
/// action back — so the little that has to survive a frame boundary lives in
/// egui's own temporary memory.
#[derive(Clone, Copy, Default)]
struct Nav {
    /// The popup that should hand focus to its first item.
    focus_into: Option<Id>,
    /// The popup the keyboard opened, so it can be closed behind a Tab and give
    /// its head the focus back when it closes.
    by_key: Option<Id>,
}

fn nav_id() -> Id {
    Id::new("tzm-menubar-nav")
}

fn read_nav(ui: &egui::Ui) -> Nav {
    ui.data(|d| d.get_temp::<Nav>(nav_id()).unwrap_or_default())
}

fn write_nav(ui: &egui::Ui, nav: Nav) {
    ui.data_mut(|d| d.insert_temp(nav_id(), nav));
}

/// Draws the bar; returns the action the user chose this frame, if any.
///
/// Call it inside the title bar's own horizontal layout, after
/// [`chrome::titlebar_interactions`]: the heads sense drags as well as clicks,
/// so a press that lands on one opens a menu instead of dragging the window,
/// while the gaps between them still carry the window.
///
/// It draws only as many heads as the width left over from
/// [`BarState::reserve`] and the window buttons can hold, and nothing at all
/// when that is less than one head — the bar gives way to the chrome rather
/// than running under it.
pub fn bar(ui: &mut egui::Ui, state: &BarState<'_>) -> Option<MenuAction> {
    let now = ui.input(|i| i.time);
    let gap = ui.spacing().item_spacing.x;
    let mark = update_mark(state.update);
    let widths: Vec<(Menu, f32)> = Menu::ALL
        .iter()
        .map(|m| (*m, head_width(ui, m.head(), mark_of(*m, mark).is_some())))
        .collect();
    let more_w = head_width(ui, copy::MENUBAR_MORE, mark.is_some());
    // A floor rather than a sum: the caller's figure already counts the window
    // buttons, but if it ever stops counting them the bar still will.
    let reserve = state.reserve.max(chrome::window_buttons_width(gap));
    let avail = (ui.available_width() - reserve).max(0.0);
    let layout = plan(&widths, gap, more_w, avail, mark.map(|_| Menu::Help));
    if layout.is_empty() {
        return None;
    }

    let height = strip_h().min(ui.available_height().max(0.0));
    if height <= 0.0 {
        return None;
    }

    let mut nav = read_nav(ui);
    let bar_rect = ui.max_rect();
    let mut heads: Vec<(Trigger, egui::Response)> = Vec::new();
    for (i, menu) in layout.shown.iter().enumerate() {
        let width = widths
            .iter()
            .find(|(m, _)| m == menu)
            .map(|(_, w)| *w)
            .unwrap_or_default();
        let resp = draw_head(
            ui,
            &Head {
                trigger: Trigger::Menu(*menu),
                width,
                height,
                gap_before: (i > 0).then_some(gap),
                mark: mark_of(*menu, mark),
                hint: i == 0,
                bar: bar_rect,
            },
        );
        // The capture hook opens a named leaf, so a docs shot can show a menu
        // as the reader meets it. Same contract as the tab and palette
        // overrides: it pins what is on screen so a capture does not depend on
        // whatever the last run left behind.
        if shot_menu().is_some_and(|w| menu.head().eq_ignore_ascii_case(&w)) {
            egui::Popup::open_id(ui.ctx(), popup_id(&resp));
        }
        heads.push((Trigger::Menu(*menu), resp));
    }
    if !layout.folded.is_empty() {
        // A mark that folded away is still a mark the reader has to see, so
        // `more` carries whatever the heads behind it were carrying.
        let folded_mark = layout.folded.iter().find_map(|m| mark_of(*m, mark));
        let first = heads.is_empty();
        let resp = draw_head(
            ui,
            &Head {
                trigger: Trigger::More,
                width: more_w,
                height,
                gap_before: (!first).then_some(gap),
                mark: folded_mark,
                hint: first,
                bar: bar_rect,
            },
        );
        heads.push((Trigger::More, resp));
    }

    let open_at = heads
        .iter()
        .position(|(_, r)| egui::Popup::is_id_open(ui.ctx(), popup_id(r)));
    step_menus(ui, &heads, open_at, &mut nav);

    let mut chosen = None;
    for (trigger, resp) in &heads {
        let popup = popup_id(resp);
        // A press that opened the menu with the keyboard takes its focus with
        // it; one that opened it with the pointer leaves focus where it was.
        if resp.clicked() && !ui.input(|i| i.pointer.any_click()) {
            nav.focus_into = Some(popup);
            nav.by_key = Some(popup);
        } else if resp.clicked() {
            nav.by_key = None;
        }
        // A shut menu is not laid out: building four pages of rows every frame
        // to throw them away is work the register never asked for. The click
        // that shuts an open one still has to reach `show`, which is what
        // toggles it.
        let out = if egui::Popup::is_id_open(ui.ctx(), popup) || resp.clicked() {
            let content = content_of(*trigger, &layout.folded, state, now);
            let width = content_width(ui, &content);
            let settings = matches!(trigger, Trigger::Menu(Menu::Display) | Trigger::More);
            show_menu(ui, resp, &content, settings, width)
        } else {
            MenuOut::default()
        };
        if let Some(action) = out.action {
            chosen = Some(action);
        }
        if !out.open {
            // Closing behind a keyboard gesture hands the head its focus back,
            // so Escape leaves the reader on the bar rather than nowhere.
            if nav.by_key == Some(popup) {
                nav.by_key = None;
                nav.focus_into = None;
                ui.ctx().memory_mut(|m| m.request_focus(resp.id));
            }
            continue;
        }
        if nav.focus_into == Some(popup)
            && let Some(first) = out.ids.first()
        {
            nav.focus_into = None;
            ui.ctx().memory_mut(|m| m.request_focus(*first));
            ui.ctx().request_repaint();
        }
        step_items(ui, &out.ids);
        if nav.by_key == Some(popup) {
            let focused = ui.ctx().memory(|m| m.focused());
            // Tab must lead on through the window rather than leave a page
            // hanging over it, so a menu the keyboard has walked out of closes.
            if let Some(f) = focused
                && f != resp.id
                && !out.ids.contains(&f)
            {
                nav.by_key = None;
                egui::Popup::close_id(ui.ctx(), popup);
            }
        }
    }

    write_nav(ui, nav);
    chosen
}

/// The mark a given head carries.
fn mark_of(menu: Menu, mark: Option<Tone>) -> Option<Tone> {
    mark.filter(|_| menu == Menu::Help)
}

/// The popup id egui derives from a trigger's response.
fn popup_id(resp: &egui::Response) -> Id {
    egui::Popup::default_response_id(resp)
}

/// The rows behind one head, as a leaf: its engraved title first, then its
/// sections. The `more` head carries every folded menu on one leaf, each under
/// its own title and folio.
///
/// A leaf is titled even when the head above it carries the same word, because
/// a page that is scrolled, or reached by the keyboard from another head, has
/// nothing else on it that says which register it belongs to.
fn content_of(trigger: Trigger, folded: &[Menu], state: &BarState<'_>, now: f64) -> Vec<Row> {
    let leaf = |m: &Menu| {
        let mut page = vec![Row::Title(m.title())];
        page.extend(rows(*m, state, now));
        page
    };
    match trigger {
        Trigger::Menu(m) => leaf(&m),
        Trigger::More => folded.iter().flat_map(leaf).collect(),
    }
}

/// The folio numeral a leaf carries at the outer end of its head rule: the
/// menu's place in the bar's reading order, set the way a deed book numbers its
/// leaves.
///
/// Read back from the title rather than carried on [`Row::Title`], so the one
/// leaf assembled out of several menus — `more` — numbers each of its sections
/// without the row type having to know it is on one.
fn folio_of(title: &str) -> Option<&'static str> {
    let at = Menu::ALL.iter().position(|m| m.title() == title)?;
    copy::MENUBAR_FOLIO.get(at).copied()
}

/// F10 opens the bar and closes it again; the horizontal arrows walk from one
/// head to the next while a menu is open, the way every desktop menu bar does.
fn step_menus(
    ui: &egui::Ui,
    heads: &[(Trigger, egui::Response)],
    open_at: Option<usize>,
    nav: &mut Nav,
) {
    if heads.is_empty() {
        return;
    }
    let f10 = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F10));
    if f10 {
        match open_at {
            Some(i) => {
                if let Some((_, resp)) = heads.get(i) {
                    egui::Popup::close_id(ui.ctx(), popup_id(resp));
                    nav.by_key = None;
                    nav.focus_into = None;
                    ui.ctx().memory_mut(|m| m.request_focus(resp.id));
                }
            }
            None => {
                if let Some((_, resp)) = heads.first() {
                    let id = popup_id(resp);
                    egui::Popup::open_id(ui.ctx(), id);
                    nav.by_key = Some(id);
                    nav.focus_into = Some(id);
                }
            }
        }
        return;
    }

    let Some(at) = open_at else { return };
    // Only while a menu is open: with the bar shut these keys belong to
    // whatever the reader is actually in.
    let (left, right) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowLeft),
            i.consume_key(Modifiers::NONE, Key::ArrowRight),
        )
    });
    let step = match (left, right) {
        (true, false) => -1_isize,
        (false, true) => 1,
        _ => return,
    };
    let len = heads.len() as isize;
    let next = ((at as isize + step) % len + len) % len;
    let Some((_, resp)) = heads.get(next as usize) else {
        return;
    };
    let id = popup_id(resp);
    // Only one popup can be open at a time, so opening the neighbour closes
    // the one that was open without asking.
    egui::Popup::open_id(ui.ctx(), id);
    nav.by_key = Some(id);
    nav.focus_into = Some(id);
}

/// The vertical arrows, Home and End move between a menu's items.
///
/// Done by hand rather than left to egui's directional focus search, which
/// scores every focusable widget in the window: the page under an open menu is
/// full of them, and one row of a register lying behind the popup would win the
/// search and take the focus out of the menu. The keys are locked to the
/// focused item ([`egui::EventFilter`]) so the search never runs at all, and a
/// repaint is asked for after each move so that lock is re-established before
/// the next key can arrive.
fn step_items(ui: &egui::Ui, ids: &[Id]) {
    if ids.is_empty() {
        return;
    }
    let focused = ui.ctx().memory(|m| m.focused());
    if let Some(f) = focused.filter(|f| ids.contains(f)) {
        ui.ctx().memory_mut(|m| {
            m.set_focus_lock_filter(
                f,
                egui::EventFilter {
                    // Tab is deliberately left alone: a menu that swallowed it
                    // would be a keyboard trap.
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            );
        });
    }
    let (down, up, home, end) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Home),
            i.consume_key(Modifiers::NONE, Key::End),
        )
    });
    let next = if home {
        ids.first().copied()
    } else if end {
        ids.last().copied()
    } else if down {
        step_focus(ids, focused, 1)
    } else if up {
        step_focus(ids, focused, -1)
    } else {
        None
    };
    if let Some(id) = next {
        ui.ctx().memory_mut(|m| m.request_focus(id));
        ui.ctx().request_repaint();
    }
}

/// Everything one head needs of the bar, bundled: a head is decided by six
/// independent facts, and a positional argument list that long is a transposed
/// pair waiting to happen.
struct Head {
    trigger: Trigger,
    width: f32,
    height: f32,
    /// The layout gap before this head, or `None` for the leading one — the
    /// separator is ruled in that gap, and the leading head has no gap to rule.
    gap_before: Option<f32>,
    /// What this head has to report on its own face.
    mark: Option<Tone>,
    /// Whether this head also speaks the key that opens the bar.
    hint: bool,
    /// The whole title bar the strip was laid into — where an open head looks
    /// for the girih band it cuts its doorway through.
    bar: Rect,
}

/// Where a head's two compartment rules sit: the cap over the word and the foot
/// under it, both snapped to the pixel grid.
///
/// `None` when the strip is too short to carry them. The title bar's height is
/// fixed while the type is not, so at a large text scale the strip is squeezed
/// around the word — and a rule ruled through the word is worse than no rule.
fn compartment_rules(strip: Rect, text: Rect) -> Option<(f32, f32)> {
    let cap = theme::snap(text.top() - theme::space::S);
    let foot = theme::snap(text.bottom() + theme::space::S);
    (cap > strip.top() && foot < strip.bottom() && cap < foot).then_some((cap, foot))
}

/// The two hairline stubs of a head separator: one from the strip's top down to
/// the diamond that pivots it, one from that diamond down to the strip's foot.
///
/// `None` when the diamond fills the gap on its own — a stub shorter than the
/// space step reads as a speck beside it, not as a rule.
fn separator_stubs(strip: Rangef, r: f32) -> Option<(Rangef, Rangef)> {
    let clear = r + theme::space::S;
    let top = Rangef::new(strip.min + theme::space::S, strip.center() - clear);
    let foot = Rangef::new(strip.center() + clear, strip.max - theme::space::S);
    (top.span() >= theme::space::S && foot.span() >= theme::space::S).then_some((top, foot))
}

/// The doorway an open head cuts through the girih band at the bar's foot: the
/// strip of band left below the head's own compartment, taken down to the seam.
///
/// This is what stops the head-band and the brand band reading as two strips of
/// chrome stacked on one another — an open head interrupts the strapwork and
/// its raised ground runs unbroken from the word down to the seam the page
/// hangs off.
///
/// `None` when the band is not in fact under this head. The title bar is laid
/// out by the window, not by this module, so the band is found rather than
/// assumed: a block of ground painted over a rect that turned out to be
/// somewhere else would be a hole punched in the chrome.
fn open_notch(strip: Rect, bar: Rect) -> Option<Rect> {
    let band = chrome::band_rect(bar);
    let reaches =
        band.is_positive() && band.top() <= strip.bottom() && band.bottom() > strip.bottom();
    reaches.then(|| {
        Rect::from_min_max(
            pos2(strip.left(), strip.bottom()),
            pos2(strip.right(), band.bottom()),
        )
    })
}

/// The division between two compartments: the house diamond pivoting between
/// two hairline stubs, ruled in the gap the layout left so the gap still drags
/// the window.
fn separator(p: &egui::Painter, x: f32, strip: Rangef) {
    let x = theme::snap(x);
    let r = mark_r();
    if let Some((top, foot)) = separator_stubs(strip, r) {
        let hair = Stroke::new(theme::RULE_W, theme::rule_divider());
        p.vline(x, top, hair);
        p.vline(x, foot, hair);
    }
    ornament::diamond(p, pos2(x, strip.center()), r, theme::gold_deep());
}

/// One compartment of the head-band: the tracked lowercase word between the two
/// rules that make it furniture rather than a link, the division to its
/// neighbour, and whatever the head has to report.
fn draw_head(ui: &mut egui::Ui, head: &Head) -> egui::Response {
    let id = ui.id().with(("tzm-menubar", head.trigger.key()));
    let (_, rect) = ui.allocate_space(vec2(head.width, head.height));
    // Click *and* drag: the title bar behind this is a drag surface, and a
    // head that sensed clicks alone would hand every press to it and never
    // open. Drags are simply ignored here, which is what stops the window
    // moving when a menu is pressed.
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    // egui toggles the popup later in the frame, so the click that is about to
    // flip it is folded in here rather than painting a frame behind.
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id(&resp)) != resp.clicked();
    let hovered = resp.hovered();
    let focused = resp.has_focus();

    let spoken = if head.hint {
        format!(
            "{}, {}",
            copy::menu_spoken(head.trigger.label()),
            copy::MENUBAR_KEY_HINT
        )
    } else {
        copy::menu_spoken(head.trigger.label())
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, open, &spoken));
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let reached = ui.ctx().animate_bool_with_time(
        id.with("reached"),
        hovered || focused || open,
        theme::dur(theme::motion::TOUCH),
    );

    let color = if open || hovered || focused {
        theme::ink()
    } else {
        theme::ink_muted()
    };
    let galley = head_galley(ui, head.trigger.label(), color);
    let text_at = pos2(
        rect.left() + head_pad_x(),
        rect.center().y - galley.size().y * 0.5,
    );
    let text_rect = Rect::from_min_size(text_at, galley.size());

    let notch = if open {
        open_notch(rect, head.bar)
    } else {
        None
    };
    let p = ui.painter();
    if open {
        p.rect_filled(rect, theme::R_NONE, theme::bg_raise());
        if let Some(notch) = notch {
            p.rect_filled(notch, theme::R_NONE, theme::bg_raise());
        }
    }
    if let Some(gap) = head.gap_before {
        separator(p, rect.left() - gap * 0.5, rect.y_range());
    }
    if let Some((cap_y, foot_y)) = compartment_rules(rect, text_rect) {
        let word = text_rect.x_range();
        let hair = Stroke::new(theme::RULE_W, theme::rule_divider());
        p.hline(word, cap_y, hair);
        p.hline(word, foot_y, hair);
        if open {
            // The page hangs from the whole compartment, so the rules widen to
            // it: a lintel over the word, and a sill at the foot of whatever
            // ground the head opened — the doorway's own sill when the band
            // gave way to it, the compartment's foot when it did not.
            p.hline(
                rect.x_range(),
                cap_y,
                Stroke::new(theme::RULE_W, theme::gold_deep()),
            );
            let sill = notch.map_or(foot_y, |n| theme::snap(n.bottom()));
            p.hline(
                rect.x_range(),
                sill,
                Stroke::new(theme::RULE_ACCENT_W, theme::gold()),
            );
        } else if reached > 0.0 {
            // Reaching a head closes its two rules on the word from opposite
            // ends — the gesture of ruling a compartment before writing in it.
            // Under reduced motion `dur` is zero, so both arrive whole.
            let span = word.span() * reached;
            let ink = Stroke::new(theme::RULE_W, theme::gold_deep());
            p.hline(Rangef::new(word.min, word.min + span), foot_y, ink);
            p.hline(Rangef::new(word.max - span, word.max), cap_y, ink);
        }
        if let Some(tone) = head.mark {
            // What a head has to report outranks its resting ink: the cap
            // carries it at the accent weight, so the bar says it without being
            // opened and without a coloured word to read past.
            p.hline(word, cap_y, Stroke::new(theme::RULE_ACCENT_W, tone.color()));
        }
    }
    p.galley(text_at, galley, color);
    if let Some(tone) = head.mark {
        let r = mark_r();
        ornament::diamond(
            p,
            pos2(text_rect.right() + theme::space::S + r, rect.center().y),
            r,
            tone.color(),
        );
    }
    focusnav::ring(ui, &resp);
    resp
}

/// The tracked galley a head and a section head are both set in.
fn head_galley(ui: &egui::Ui, label: &str, color: egui::Color32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat {
            font_id: theme::font(theme::step::META, theme::fam_medium()),
            color,
            extra_letter_spacing: theme::HEADER_TRACKING,
            ..Default::default()
        },
    );
    ui.painter().layout_job(job)
}

/// How wide a head is drawn, mark included.
fn head_width(ui: &egui::Ui, label: &str, marked: bool) -> f32 {
    let text = head_galley(ui, label, theme::ink()).size().x;
    let mark = if marked {
        theme::space::S + mark_r() * 2.0
    } else {
        0.0
    };
    text + head_pad_x() * 2.0 + mark
}

/// What one menu answered with this frame.
#[derive(Default)]
struct MenuOut {
    action: Option<MenuAction>,
    /// The items that can take focus, in reading order.
    ids: Vec<Id>,
    open: bool,
}

/// The leaf itself: the chrome ground, a ruled edge, and the one shadow this
/// design allows. Tighter at the sides than [`super::register::overlay`]
/// because the entries on it rule themselves edge to edge.
fn page_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::bg_chrome())
        .stroke(Stroke::new(theme::RULE_W, theme::rule_emphasis()))
        .corner_radius(theme::R_OVERLAY)
        .inner_margin(egui::Margin::same(theme::space::M as i8))
        .shadow(theme::elevation::overlay())
}

/// The leaf's illumination: a cusp at each end of its spine, in the gesture
/// [`super::ceremony::adorn_dialog`] already puts on a dialog. Sized off the
/// type and set in from the corner radius, so it stays inside the rounded
/// corner and clear of the title beside it at every text scale.
fn adorn_page(p: &egui::Painter, rect: Rect) {
    if !rect.is_finite() || !rect.is_positive() {
        return;
    }
    let inset = f32::from(theme::R_OVERLAY);
    let size = theme::sized(theme::step::LABEL);
    ornament::corner_flourish(
        p,
        rect.left_top() + vec2(inset, inset),
        vec2(1.0, 1.0),
        size,
        theme::gold(),
    );
    ornament::corner_flourish(
        p,
        rect.left_bottom() + vec2(inset, -inset),
        vec2(1.0, -1.0),
        size,
        theme::gold(),
    );
}

/// The gutter rule, accumulated as the leaf is laid out.
///
/// Ruled per block rather than down the whole leaf: a section head is
/// out-dented into the margin, and one rule running the full height would
/// strike every head on the page through.
#[derive(Default)]
struct Gutter {
    open: Option<(f32, Rangef)>,
    runs: Vec<(f32, Rangef)>,
}

impl Gutter {
    /// Extends the run in hand over one more entry.
    fn entry(&mut self, rect: Rect) {
        self.open = Some(match self.open.take() {
            Some((x, span)) => (
                x,
                Rangef::new(span.min.min(rect.top()), span.max.max(rect.bottom())),
            ),
            None => (rect.left() + margin_w(), rect.y_range()),
        });
    }

    /// Closes the run in hand — a head, a title, or the foot of the leaf.
    fn brk(&mut self) {
        if let Some(run) = self.open.take() {
            self.runs.push(run);
        }
    }

    fn finish(mut self) -> Vec<(f32, Rangef)> {
        self.brk();
        self.runs
    }
}

/// The leaf under a head: a titled, ruled page on the one surface this design
/// lets float.
fn show_menu(
    ui: &egui::Ui,
    trigger: &egui::Response,
    content: &[Row],
    settings: bool,
    width: f32,
) -> MenuOut {
    let mut out = MenuOut::default();
    let mut popup = egui::Popup::menu(trigger).width(width).frame(page_frame());
    if settings {
        // A page of settings stays put while it is being used: a reader
        // comparing two palettes should not have to re-open the menu between
        // them. The verbs on it still shut it as they run.
        popup = popup.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    }
    // A page taller than the window scrolls rather than running off the screen,
    // but never shrinks past the four rows that make it readable at all.
    let room = ui.ctx().content_rect().height() - chrome::TITLEBAR_H - theme::space::XXL;
    let max_h = room.max(theme::density().row_h() * 4.0);
    let shown = popup.show(|ui| {
        egui::ScrollArea::vertical()
            .max_height(max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let mut gutter = Gutter::default();
                for row in content {
                    match row {
                        Row::Title(title) => {
                            gutter.brk();
                            page_head(ui, title);
                        }
                        Row::Head { title, value } => {
                            gutter.brk();
                            section_row(ui, title, value.as_deref());
                        }
                        Row::Note { text, tone } => gutter.entry(note_row(ui, text, *tone)),
                        Row::Item(item) => {
                            let resp = item_row(ui, item);
                            gutter.entry(resp.rect);
                            if resp.enabled() {
                                out.ids.push(resp.id);
                            }
                            if resp.clicked() {
                                out.action = Some(item.action);
                                // A choice leaves the page open so the next one
                                // can be compared against it; a verb is done
                                // and takes the page with it.
                                if item.choice.is_none() {
                                    ui.close();
                                }
                            }
                        }
                    }
                }
                // Last, so the ruling sits over the washes the rows painted
                // rather than under them — a ledger is ruled before it is
                // written in, and the rule stays visible where it is.
                let hair = Stroke::new(theme::RULE_W, theme::rule_hair());
                for (x, span) in gutter.finish() {
                    ui.painter().vline(theme::snap(x), span, hair);
                }
            });
    });
    if let Some(shown) = &shown {
        adorn_page(
            &ui.ctx().layer_painter(shown.response.layer_id),
            shown.response.rect,
        );
    }
    out.open = shown.is_some();
    out
}

/// The head of a leaf: the engraved title, the rule that carries it across the
/// page, and the folio at the outer end of that rule.
///
/// Silent to assistive technology, and deliberately: the head that opened this
/// leaf is a button that already announces the same word, and a painted title
/// repeating it would make every menu say its own name twice.
fn page_head(ui: &mut egui::Ui, title: &str) {
    ui.add_space(theme::space::S);
    let p = ui.painter();
    let galley = p.layout_no_wrap(
        title.to_owned(),
        theme::font(theme::step::TITLE, theme::fam_serif()),
        theme::ink(),
    );
    let folio = folio_of(title).map(|f| {
        p.layout_no_wrap(
            f.to_owned(),
            theme::font(theme::step::LABEL, theme::fam_serif()),
            theme::gold(),
        )
    });
    let h = galley
        .size()
        .y
        .max(folio.as_ref().map_or(0.0, |g| g.size().y));
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), h + theme::space::S),
        Sense::hover(),
    );
    if !rect.is_positive() {
        return;
    }
    let p = ui.painter();
    // On the text column, with the entries it heads — which also keeps it clear
    // of the cusp `adorn_page` sets in the corner beside it.
    let title_left = rect.left() + row_gutter();
    let title_right = title_left + galley.size().x;
    p.galley(
        pos2(title_left, rect.center().y - galley.size().y * 0.5),
        galley,
        theme::ink(),
    );
    let edge = rect.right() - theme::space::M;
    let folio_left = folio.as_ref().map_or(edge, |g| edge - g.size().x);
    // The rule runs *through* the line between the two marks rather than under
    // it, the way a ledger's head rule does, so the title and the folio read as
    // the two ends of one gesture instead of a label over a separator. With no
    // folio to stop it, it runs the full measure of the page.
    let from = title_right + theme::space::M;
    let to = folio
        .as_ref()
        .map_or(edge, |_| folio_left - theme::space::M);
    if to > from {
        p.hline(
            Rangef::new(from, to),
            theme::snap(rect.center().y),
            Stroke::new(theme::RULE_W, theme::rule_emphasis()),
        );
    }
    if let Some(g) = folio {
        let at = pos2(folio_left, rect.center().y - g.size().y * 0.5);
        p.galley(at, g, theme::gold());
    }
    ui.add_space(theme::space::XS);
}

/// A section head inside a leaf: the tracked word out-dented into the margin,
/// the rule carrying it across the page, and its value at the far end when it
/// has one.
fn section_row(ui: &mut egui::Ui, title: &str, value: Option<&str>) {
    ui.add_space(theme::space::M);
    let h = theme::sized(theme::step::META) + theme::space::S;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    if !rect.is_positive() {
        return;
    }
    let galley = head_galley(ui, title, theme::ink_faint());
    let value = value.map(|v| {
        ui.painter().layout_no_wrap(
            v.to_owned(),
            theme::font(theme::step::DATA, theme::fam_mono()),
            theme::ink_muted(),
        )
    });
    let p = ui.painter();
    let word_right = rect.left() + galley.size().x;
    p.galley(
        pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        theme::ink_faint(),
    );
    let edge = rect.right() - theme::space::M;
    let value_left = value.as_ref().map_or(edge, |g| edge - g.size().x);
    let from = word_right + theme::space::M;
    let to = value
        .as_ref()
        .map_or(edge, |_| value_left - theme::space::M);
    if to > from {
        p.hline(
            Rangef::new(from, to),
            theme::snap(rect.center().y),
            Stroke::new(theme::RULE_W, theme::rule_hair()),
        );
    }
    if let Some(g) = value {
        let at = pos2(value_left, rect.center().y - g.size().y * 0.5);
        p.galley(at, g, theme::ink_muted());
    }
}

/// A line that only reports — the update status, and nothing else so far.
/// Returns the space it took, so the gutter can be ruled beside it.
fn note_row(ui: &mut egui::Ui, text: &str, tone: Tone) -> Rect {
    let width = (ui.available_width() - row_gutter()).max(0.0);
    let galley = ui.painter().layout(
        text.to_owned(),
        theme::font(theme::step::META, egui::FontFamily::Proportional),
        tone.color(),
        width,
    );
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), galley.size().y + theme::space::M),
        Sense::hover(),
    );
    a11y::describe(&resp, text);
    let at = pos2(
        rect.left() + row_gutter(),
        rect.top() + theme::space::S * 0.5,
    );
    ui.painter().galley(at, galley, tone.color());
    rect
}

/// One entry of the leaf: the verb, its chord in the right column, the hairline
/// ruling it off from the next, and the mark it earns in the margin.
fn item_row(ui: &mut egui::Ui, item: &Item) -> egui::Response {
    let enabled = item.enabled();
    let resp = ui.add_enabled_ui(enabled, |ui| item_body(ui, item)).inner;
    match &item.blocked {
        // Refused rather than hidden: which verbs exist here is part of what
        // the menu teaches, and the reason is the useful half of the answer.
        Some(why) => resp.on_disabled_hover_text(why.as_str()),
        None => resp,
    }
}

fn item_body(ui: &mut egui::Ui, item: &Item) -> egui::Response {
    let enabled = ui.is_enabled();
    let h = theme::density().row_h();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    match item.choice {
        Some(current) => a11y::label_selectable(&resp, &item.label, current),
        None => a11y::label_button(&resp, &item.label),
    }
    if resp.gained_focus() {
        // The folded page can be taller than the screen; a focus the reader
        // cannot see is a focus they have lost.
        resp.scroll_to_me(None);
    }
    let hovered = enabled && resp.hovered();
    let focused = enabled && resp.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let p = ui.painter();
    if item.marked() {
        p.rect_filled(
            rect,
            theme::R_NONE,
            theme::wash::of(theme::gold(), theme::wash::SELECT),
        );
    } else if hovered || focused {
        p.rect_filled(rect, theme::R_NONE, theme::bg_raise());
    }

    let ink = if !enabled {
        theme::ink_disabled()
    } else if hovered || focused || item.marked() {
        theme::ink()
    } else {
        theme::ink_muted()
    };
    p.text(
        pos2(rect.left() + row_gutter(), rect.center().y),
        egui::Align2::LEFT_CENTER,
        &item.label,
        theme::font(theme::step::LABEL, theme::fam_medium()),
        ink,
    );
    if let Some(chord) = &item.chord {
        p.text(
            pos2(rect.right() - theme::space::M, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            chord,
            theme::font(theme::step::CAPTION, theme::fam_mono()),
            if enabled {
                theme::ink_faint()
            } else {
                theme::ink_disabled()
            },
        );
    }
    // The margin says at a glance what the row is worth reaching for: the gold
    // diamond against the choice in force, the scribe's null mark against a
    // verb that is refused. The gutter rule swells behind the choice rather
    // than a second bar appearing at the page edge, because there is one column
    // rule on this page and a selection belongs on it.
    if item.marked() {
        p.vline(
            theme::snap(rect.left() + margin_w()),
            rect.y_range(),
            Stroke::new(theme::RULE_ACCENT_W, theme::gold()),
        );
    }
    if item.marked() || !enabled {
        let ink = if enabled {
            theme::gold()
        } else {
            theme::ink_disabled()
        };
        ornament::diamond(
            p,
            pos2(rect.left() + margin_w() * 0.5, rect.center().y),
            mark_r(),
            ink,
        );
    }
    p.hline(
        rect.x_range(),
        theme::snap(rect.bottom()),
        Stroke::new(
            theme::RULE_W,
            if hovered || focused {
                theme::gold_deep()
            } else {
                theme::rule_hair()
            },
        ),
    );
    focusnav::ring(ui, &resp);
    resp
}

/// How wide a leaf has to be ruled to hold its longest line without eliding it,
/// and never narrower than a page's measure.
fn content_width(ui: &egui::Ui, content: &[Row]) -> f32 {
    let p = ui.painter();
    let mut widest: f32 = min_page_w();
    for row in content {
        let w = match row {
            Row::Title(title) => {
                let head = p
                    .layout_no_wrap(
                        (*title).to_owned(),
                        theme::font(theme::step::TITLE, theme::fam_serif()),
                        theme::ink(),
                    )
                    .size()
                    .x;
                let folio = folio_of(title)
                    .map(|f| {
                        p.layout_no_wrap(
                            f.to_owned(),
                            theme::font(theme::step::LABEL, theme::fam_serif()),
                            theme::gold(),
                        )
                        .size()
                        .x + theme::space::XXL
                    })
                    .unwrap_or_default();
                row_gutter() + head + folio
            }
            Row::Head { title, value } => {
                let head = head_galley(ui, title, theme::ink_faint()).size().x;
                let value = value
                    .as_ref()
                    .map(|v| {
                        p.layout_no_wrap(
                            v.clone(),
                            theme::font(theme::step::DATA, theme::fam_mono()),
                            theme::ink_muted(),
                        )
                        .size()
                        .x + theme::space::XXL
                    })
                    .unwrap_or_default();
                head + value
            }
            // A note wraps, so it asks for no width of its own.
            Row::Note { .. } => 0.0,
            Row::Item(item) => {
                let label = p
                    .layout_no_wrap(
                        item.label.clone(),
                        theme::font(theme::step::LABEL, theme::fam_medium()),
                        theme::ink(),
                    )
                    .size()
                    .x;
                let chord = item
                    .chord
                    .as_ref()
                    .map(|c| {
                        p.layout_no_wrap(
                            c.clone(),
                            theme::font(theme::step::CAPTION, theme::fam_mono()),
                            theme::ink_faint(),
                        )
                        .size()
                        .x + theme::space::XXL
                    })
                    .unwrap_or_default();
                row_gutter() + label + chord
            }
        };
        widest = widest.max(w);
    }
    widest + theme::space::XL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(current: &str) -> UpdateState {
        UpdateState {
            current: current.to_owned(),
            ..Default::default()
        }
    }

    fn state<'a>(session: Option<SessionState<'a>>, update: &'a UpdateState) -> BarState<'a> {
        BarState {
            session,
            update,
            supervisor: false,
            mode: theme::Mode::Contrast,
            density: theme::Density::Regular,
            reduced_motion: false,
            text_scale: a11y::SCALE_DEFAULT,
            reserve: 0.0,
        }
    }

    fn session(running: bool, paused: bool) -> SessionState<'static> {
        SessionState {
            running,
            paused,
            role: "editor",
            may_edit: true,
            conflicts: 0,
            marked_file: Some("notes/deed.md"),
        }
    }

    /// The item carrying `action`, wherever it sits in the menu.
    fn find(page: &[Row], action: MenuAction) -> Item {
        page.iter()
            .find_map(|r| match r {
                Row::Item(i) if i.action == action => Some(i.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no item for {action:?}"))
    }

    fn notes(page: &[Row]) -> Vec<(String, Tone)> {
        page.iter()
            .filter_map(|r| match r {
                Row::Note { text, tone } => Some((text.clone(), *tone)),
                _ => None,
            })
            .collect()
    }

    // ─── enablement ──────────────────────────────────────────────────────────

    #[test]
    fn with_no_session_open_every_session_verb_says_so() {
        let u = update("0.1.9");
        let s = state(None, &u);
        let page = rows(Menu::Session, &s, 0.0);
        for action in [
            MenuAction::Start,
            MenuAction::Stop,
            MenuAction::Pause,
            MenuAction::Resume,
            MenuAction::OpenFolder,
            MenuAction::CopyPath,
            MenuAction::Rename,
        ] {
            let item = find(&page, action);
            assert_eq!(
                item.blocked.as_deref(),
                Some(copy::MENUBAR_NEEDS_SESSION),
                "{action:?}"
            );
        }
    }

    /// The verb that would start what is already started is the one that is
    /// refused, and the other one works.
    #[test]
    fn a_running_session_can_be_stopped_but_not_started() {
        let u = update("0.1.9");
        let s = state(Some(session(true, false)), &u);
        let page = rows(Menu::Session, &s, 0.0);
        assert_eq!(
            find(&page, MenuAction::Start).blocked.as_deref(),
            Some(copy::MENUBAR_ALREADY_RUNNING)
        );
        assert!(find(&page, MenuAction::Stop).enabled());
    }

    #[test]
    fn a_stopped_session_cannot_be_paused_or_resumed() {
        let u = update("0.1.9");
        let s = state(Some(session(false, false)), &u);
        let page = rows(Menu::Session, &s, 0.0);
        assert_eq!(
            find(&page, MenuAction::Pause).blocked.as_deref(),
            Some(copy::MENUBAR_NEEDS_RUNNING)
        );
        assert_eq!(
            find(&page, MenuAction::Resume).blocked.as_deref(),
            Some(copy::MENUBAR_NEEDS_RUNNING)
        );
        assert!(find(&page, MenuAction::Start).enabled());
    }

    /// Pause and Resume are never both live: one of them is always the verb
    /// that has already happened.
    #[test]
    fn pause_and_resume_answer_the_paused_flag() {
        let u = update("0.1.9");
        let running = state(Some(session(true, false)), &u);
        let page = rows(Menu::Session, &running, 0.0);
        assert!(find(&page, MenuAction::Pause).enabled());
        assert_eq!(
            find(&page, MenuAction::Resume).blocked.as_deref(),
            Some(copy::MENUBAR_NOT_PAUSED)
        );

        let paused = state(Some(session(true, true)), &u);
        let page = rows(Menu::Session, &paused, 0.0);
        assert!(find(&page, MenuAction::Resume).enabled());
        assert_eq!(
            find(&page, MenuAction::Pause).blocked.as_deref(),
            Some(copy::MENUBAR_ALREADY_PAUSED)
        );
    }

    #[test]
    fn a_viewer_cannot_rename_and_is_told_which_role_it_is() {
        let u = update("0.1.9");
        let viewer = SessionState {
            role: "viewer",
            may_edit: false,
            ..session(true, false)
        };
        let s = state(Some(viewer), &u);
        let item = find(&rows(Menu::Session, &s, 0.0), MenuAction::Rename);
        let why = item.blocked.unwrap_or_default();
        assert!(why.contains("viewer"), "{why}");
        assert_eq!(why, copy::role_cannot_edit("viewer"));
    }

    /// An editor with nothing marked is refused for the reason that actually
    /// applies to them, not for the role they do have.
    #[test]
    fn renaming_needs_a_file_to_rename() {
        let u = update("0.1.9");
        let bare = SessionState {
            marked_file: None,
            ..session(true, false)
        };
        let s = state(Some(bare), &u);
        let item = find(&rows(Menu::Session, &s, 0.0), MenuAction::Rename);
        assert_eq!(item.blocked.as_deref(), Some(copy::MENUBAR_NEEDS_FILE));
    }

    #[test]
    fn the_tools_that_need_a_daemon_refuse_a_stopped_session() {
        let u = update("0.1.9");
        let s = state(Some(session(false, false)), &u);
        let page = rows(Menu::Tools, &s, 0.0);
        for action in [MenuAction::Doctor, MenuAction::Dashboard, MenuAction::Gc] {
            assert_eq!(
                find(&page, action).blocked.as_deref(),
                Some(copy::MENUBAR_NEEDS_RUNNING),
                "{action:?}"
            );
        }
        // Rotating the key rewrites local state and needs no peer.
        assert!(find(&page, MenuAction::Rekey).enabled());
    }

    #[test]
    fn pruning_is_refused_when_nothing_is_preserved() {
        let u = update("0.1.9");
        let clean = state(Some(session(true, false)), &u);
        assert_eq!(
            find(&rows(Menu::Tools, &clean, 0.0), MenuAction::Prune)
                .blocked
                .as_deref(),
            Some(copy::MENUBAR_NO_CONFLICTS)
        );

        let holding = SessionState {
            conflicts: 3,
            ..session(true, false)
        };
        let s = state(Some(holding), &u);
        assert!(find(&rows(Menu::Tools, &s, 0.0), MenuAction::Prune).enabled());
    }

    /// The supervisor offers the one verb that applies, and it applies whether
    /// or not a session is open — it is a property of the machine.
    #[test]
    fn the_supervisor_offers_the_verb_that_applies() {
        let u = update("0.1.9");
        let mut off = state(None, &u);
        off.supervisor = false;
        let item = find(
            &rows(Menu::Tools, &off, 0.0),
            MenuAction::Supervisor { install: true },
        );
        assert_eq!(item.label, copy::SUPERVISOR_INSTALL);
        assert!(item.enabled());

        let mut on = state(None, &u);
        on.supervisor = true;
        let item = find(
            &rows(Menu::Tools, &on, 0.0),
            MenuAction::Supervisor { install: false },
        );
        assert_eq!(item.label, copy::SUPERVISOR_REMOVE);
    }

    // ─── display ─────────────────────────────────────────────────────────────

    #[test]
    fn exactly_one_choice_is_marked_in_every_group() {
        let u = update("0.1.9");
        let mut s = state(None, &u);
        s.mode = theme::Mode::Paper;
        s.density = theme::Density::Compact;
        s.reduced_motion = true;
        let page = rows(Menu::Display, &s, 0.0);

        assert!(find(&page, MenuAction::Palette(theme::Mode::Paper)).marked());
        assert!(!find(&page, MenuAction::Palette(theme::Mode::Night)).marked());
        assert!(find(&page, MenuAction::Density(theme::Density::Compact)).marked());
        assert!(find(&page, MenuAction::Motion { reduced: true }).marked());
        assert!(!find(&page, MenuAction::Motion { reduced: false }).marked());

        let marked = page
            .iter()
            .filter(|r| matches!(r, Row::Item(i) if i.marked()))
            .count();
        assert_eq!(marked, 3);
    }

    #[test]
    fn the_text_steppers_stop_at_the_ends_of_the_scale() {
        let u = update("0.1.9");
        let mut top = state(None, &u);
        top.text_scale = a11y::SCALE_MAX;
        let page = rows(Menu::Display, &top, 0.0);
        let bigger = find(&page, MenuAction::TextScale(a11y::SCALE_MAX));
        assert_eq!(bigger.blocked.as_deref(), Some(copy::MENUBAR_TEXT_AT_MAX));
        assert!(page.iter().any(|r| matches!(
            r,
            Row::Item(i) if i.action == MenuAction::TextScale(a11y::step_scale(a11y::SCALE_MAX, false))
        )));

        let mut bottom = state(None, &u);
        bottom.text_scale = a11y::SCALE_MIN;
        let page = rows(Menu::Display, &bottom, 0.0);
        assert_eq!(
            find(&page, MenuAction::TextScale(a11y::SCALE_MIN))
                .blocked
                .as_deref(),
            Some(copy::MENUBAR_TEXT_AT_MIN)
        );
    }

    #[test]
    fn the_reset_is_refused_when_the_scale_has_not_moved() {
        let u = update("0.1.9");
        let s = state(None, &u);
        let item = find(
            &rows(Menu::Display, &s, 0.0),
            MenuAction::TextScale(a11y::SCALE_DEFAULT),
        );
        assert_eq!(item.blocked.as_deref(), Some(copy::MENUBAR_TEXT_AT_DEFAULT));
    }

    /// The head over the steppers carries the scale in force, which is the only
    /// place the menu can say what "Larger" would be larger than.
    #[test]
    fn the_text_section_head_carries_the_scale() {
        let u = update("0.1.9");
        let mut s = state(None, &u);
        s.text_scale = 1.25;
        let page = rows(Menu::Display, &s, 0.0);
        let value = page.iter().find_map(|r| match r {
            Row::Head { title, value } if *title == copy::MENUBAR_TEXT => value.clone(),
            _ => None,
        });
        assert_eq!(value.as_deref(), Some("125%"));
    }

    /// A scale hand-edited out of range must not produce a stepper that claims
    /// to be at the end of a range it is outside of.
    #[test]
    fn an_out_of_range_scale_is_clamped_before_the_steppers_read_it() {
        let u = update("0.1.9");
        let mut wild = state(None, &u);
        wild.text_scale = 99.0;
        let page = rows(Menu::Display, &wild, 0.0);
        assert_eq!(
            find(&page, MenuAction::TextScale(a11y::SCALE_MAX))
                .blocked
                .as_deref(),
            Some(copy::MENUBAR_TEXT_AT_MAX)
        );

        let mut nan = state(None, &u);
        nan.text_scale = f32::NAN;
        let page = rows(Menu::Display, &nan, 0.0);
        let value = page.iter().find_map(|r| match r {
            Row::Head { title, value } if *title == copy::MENUBAR_TEXT => value.clone(),
            _ => None,
        });
        assert_eq!(value.as_deref(), Some("100%"));
    }

    // ─── updates ─────────────────────────────────────────────────────────────

    #[test]
    fn an_unchecked_window_offers_the_check_and_says_it_has_not_run() {
        let u = update("0.1.9");
        let page = update_rows(&u, 0.0);
        let item = find(&page, MenuAction::CheckUpdate);
        assert_eq!(item.label, copy::MENU_UPDATE_CHECK);
        assert!(item.enabled());
        assert_eq!(
            notes(&page),
            vec![(copy::update_unchecked("0.1.9"), Tone::Muted)]
        );
        assert_eq!(update_mark(&u), None);
    }

    #[test]
    fn a_check_in_flight_says_it_is_checking_and_refuses_a_second_one() {
        let mut u = update("0.1.9");
        u.busy = true;
        let page = update_rows(&u, 0.0);
        let item = find(&page, MenuAction::CheckUpdate);
        assert_eq!(item.label, copy::MENU_UPDATE_CHECKING);
        assert_eq!(item.blocked.as_deref(), Some(copy::MENUBAR_UPDATE_BUSY));
        assert_eq!(
            notes(&page),
            vec![(copy::UPDATE_CHECKING_NOTE.to_owned(), Tone::Muted)]
        );
    }

    #[test]
    fn a_newer_release_becomes_the_verb_and_marks_the_head() {
        let mut u = update("0.1.9");
        u.latest = Some("0.2.1".to_owned());
        let page = update_rows(&u, 0.0);
        let item = find(&page, MenuAction::ApplyUpdate);
        assert_eq!(item.label, "Install 0.2.1");
        assert!(item.enabled());
        assert_eq!(
            notes(&page),
            vec![(copy::update_offer("0.1.9", "0.2.1"), Tone::Offer)]
        );
        assert_eq!(update_mark(&u), Some(Tone::Offer));
    }

    /// A check that came back with the version already running is not an offer.
    #[test]
    fn a_check_that_finds_nothing_newer_reads_as_up_to_date() {
        let mut u = update("0.1.9");
        u.latest = Some("0.1.9".to_owned());
        u.checked_at = Some(10.0);
        let page = update_rows(&u, 130.0);
        let item = find(&page, MenuAction::CheckUpdate);
        assert_eq!(item.label, copy::MENU_UPDATE_CHECK);
        assert_eq!(
            notes(&page),
            vec![(
                copy::update_up_to_date("0.1.9", "2 minutes ago"),
                Tone::Muted
            )]
        );
        assert_eq!(update_mark(&u), None);
    }

    #[test]
    fn an_installed_update_says_it_lands_on_restart() {
        let mut u = update("0.1.9");
        u.latest = Some("0.2.1".to_owned());
        u.applied = true;
        let page = update_rows(&u, 0.0);
        assert_eq!(
            notes(&page),
            vec![(copy::update_applied("0.2.1"), Tone::Offer)]
        );
        assert_eq!(update_mark(&u), Some(Tone::Offer));
    }

    #[test]
    fn a_failed_check_carries_its_reason_and_stays_retryable() {
        let mut u = update("0.1.9");
        u.error = Some("no route to github.com".to_owned());
        let page = update_rows(&u, 0.0);
        assert!(find(&page, MenuAction::CheckUpdate).enabled());
        assert_eq!(
            notes(&page),
            vec![(copy::update_failed("no route to github.com"), Tone::Trouble)]
        );
        assert_eq!(update_mark(&u), Some(Tone::Trouble));
    }

    /// A failure that arrived after an offer must not hide the offer: the
    /// reader can still install what the window already found.
    #[test]
    fn an_offer_outranks_a_failure_on_the_head() {
        let mut u = update("0.1.9");
        u.latest = Some("0.2.1".to_owned());
        u.error = Some("the retry timed out".to_owned());
        assert_eq!(update_mark(&u), Some(Tone::Offer));
        let page = update_rows(&u, 0.0);
        assert!(find(&page, MenuAction::ApplyUpdate).enabled());
        assert_eq!(
            notes(&page),
            vec![(copy::update_failed("the retry timed out"), Tone::Trouble)]
        );
    }

    /// A clock that went backwards between the check and the frame is a skew,
    /// not an event worth reporting.
    #[test]
    fn a_check_stamped_in_the_future_reads_as_just_now() {
        let mut u = update("0.1.9");
        u.latest = Some("0.1.9".to_owned());
        u.checked_at = Some(500.0);
        let page = update_rows(&u, 10.0);
        assert_eq!(
            notes(&page),
            vec![(copy::update_up_to_date("0.1.9", "just now"), Tone::Muted)]
        );
    }

    // ─── the plan ────────────────────────────────────────────────────────────

    fn even(w: f32) -> Vec<(Menu, f32)> {
        Menu::ALL.iter().map(|m| (*m, w)).collect()
    }

    #[test]
    fn a_wide_bar_rules_every_head() {
        let p = plan(&even(60.0), 8.0, 50.0, 1000.0, None);
        assert_eq!(p.shown, Menu::ALL.to_vec());
        assert!(p.folded.is_empty());
    }

    /// Exactly enough room is enough: 4 heads of 60 with 3 gaps of 8 is 264.
    #[test]
    fn a_bar_that_just_fits_folds_nothing() {
        let p = plan(&even(60.0), 8.0, 50.0, 264.0, None);
        assert!(p.folded.is_empty());
        let p = plan(&even(60.0), 8.0, 50.0, 263.0, None);
        assert!(!p.folded.is_empty());
    }

    /// The later heads are the ones given up, and in order.
    #[test]
    fn a_narrow_bar_folds_from_the_end() {
        // 50 for `more` leaves 150: session (60+8) and tools (60+8) fit, display
        // does not.
        let p = plan(&even(60.0), 8.0, 50.0, 200.0, None);
        assert_eq!(p.shown, vec![Menu::Session, Menu::Tools]);
        assert_eq!(p.folded, vec![Menu::Display, Menu::Help]);
    }

    #[test]
    fn a_head_with_something_to_report_is_kept_over_the_ones_before_it() {
        let p = plan(&even(60.0), 8.0, 50.0, 200.0, Some(Menu::Help));
        // Help is taken first, then session; the bar still reads in its own
        // order.
        assert_eq!(p.shown, vec![Menu::Session, Menu::Help]);
        assert_eq!(p.folded, vec![Menu::Tools, Menu::Display]);
    }

    #[test]
    fn a_bar_with_room_for_one_head_keeps_the_first_and_folds_the_rest() {
        let p = plan(&even(60.0), 8.0, 50.0, 125.0, None);
        assert_eq!(p.shown, vec![Menu::Session]);
        assert_eq!(p.folded, vec![Menu::Tools, Menu::Display, Menu::Help]);
    }

    #[test]
    fn a_bar_with_room_for_nothing_but_more_shows_only_more() {
        let p = plan(&even(60.0), 8.0, 50.0, 60.0, None);
        assert!(p.shown.is_empty());
        assert_eq!(p.folded, Menu::ALL.to_vec());
        assert!(!p.is_empty());
    }

    /// The window buttons are the one thing the bar may never reach: with less
    /// room than the `more` head needs, it draws nothing at all.
    #[test]
    fn a_bar_with_no_room_draws_nothing() {
        let p = plan(&even(60.0), 8.0, 50.0, 49.0, None);
        assert!(p.is_empty());
        assert!(plan(&even(60.0), 8.0, 50.0, 0.0, None).is_empty());
        assert!(plan(&even(60.0), 8.0, 50.0, -20.0, None).is_empty());
        assert!(plan(&even(60.0), 8.0, 50.0, f32::NAN, None).is_empty());
    }

    #[test]
    fn an_empty_ruler_plans_nothing() {
        assert!(plan(&[], 8.0, 50.0, 900.0, None).is_empty());
    }

    /// Heads are not all the same width in practice; the greedy fit must honour
    /// the real ones.
    #[test]
    fn uneven_heads_are_fitted_by_their_own_widths() {
        let widths = vec![
            (Menu::Session, 90.0),
            (Menu::Tools, 60.0),
            (Menu::Display, 80.0),
            (Menu::Help, 55.0),
        ];
        let p = plan(&widths, 8.0, 50.0, 220.0, None);
        assert_eq!(p.shown, vec![Menu::Session, Menu::Tools]);
        assert_eq!(p.folded, vec![Menu::Display, Menu::Help]);
    }

    // ─── focus stepping ──────────────────────────────────────────────────────

    fn ids(n: usize) -> Vec<Id> {
        (0..n).map(|i| Id::new(("item", i))).collect()
    }

    #[test]
    fn stepping_into_a_menu_lands_on_an_end() {
        let ids = ids(3);
        assert_eq!(step_focus(&ids, None, 1), Some(ids[0]));
        assert_eq!(step_focus(&ids, None, -1), Some(ids[2]));
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        let ids = ids(3);
        assert_eq!(step_focus(&ids, Some(ids[2]), 1), Some(ids[0]));
        assert_eq!(step_focus(&ids, Some(ids[0]), -1), Some(ids[2]));
        assert_eq!(step_focus(&ids, Some(ids[0]), 1), Some(ids[1]));
    }

    /// The ids handed in are the ones that can take focus, so a refused item is
    /// stepped straight over rather than stopped on.
    #[test]
    fn stepping_from_outside_the_menu_starts_over() {
        let ids = ids(3);
        assert_eq!(
            step_focus(&ids, Some(Id::new("elsewhere")), 1),
            Some(ids[0])
        );
        assert_eq!(step_focus(&[], Some(ids[0]), 1), None);
    }

    // ─── chords ──────────────────────────────────────────────────────────────

    /// The menu prints the chord the sheet prints, because it reads the same
    /// registry rather than keeping a copy.
    #[test]
    fn chords_come_from_the_one_registry() {
        assert_eq!(chord_caps(shortcuts::Action::Sheet).as_deref(), Some("?"));
        assert_eq!(
            chord_caps(shortcuts::Action::TextReset).as_deref(),
            Some("Ctrl 0")
        );
        // Nothing is bound to this, and an unbound action prints no cap.
        assert_eq!(chord_caps(shortcuts::Action::Tab(42)), None);
    }

    #[test]
    fn the_help_menu_prints_the_sheets_own_key() {
        let u = update("0.1.9");
        let s = state(None, &u);
        let item = find(&rows(Menu::Help, &s, 0.0), MenuAction::Shortcuts);
        assert_eq!(item.chord.as_deref(), Some("?"));
    }

    // ─── shape ───────────────────────────────────────────────────────────────

    /// Every menu opens with a section head: a page of this register is never
    /// a bare list.
    #[test]
    fn every_menu_opens_with_a_section_head() {
        let u = update("0.1.9");
        let s = state(Some(session(true, false)), &u);
        for menu in Menu::ALL {
            let page = rows(menu, &s, 0.0);
            assert!(
                matches!(page.first(), Some(Row::Head { .. })),
                "{menu:?} does not open with a head"
            );
            assert!(
                page.iter().any(|r| matches!(r, Row::Item(_))),
                "{menu:?} has no items"
            );
        }
    }

    #[test]
    fn a_head_carries_both_a_lowercase_word_and_a_serif_title() {
        for menu in Menu::ALL {
            assert_eq!(menu.head(), menu.head().to_lowercase());
            assert!(menu.title().starts_with(|c: char| c.is_uppercase()));
        }
    }

    // ─── the leaf ────────────────────────────────────────────────────────────

    /// A page reached from the keyboard, or scrolled away from its head, has
    /// only its own title to say which register it belongs to.
    #[test]
    fn every_leaf_opens_with_its_own_engraved_title() {
        let u = update("0.1.9");
        let s = state(Some(session(true, false)), &u);
        for menu in Menu::ALL {
            let page = content_of(Trigger::Menu(menu), &[], &s, 0.0);
            assert_eq!(page.first(), Some(&Row::Title(menu.title())), "{menu:?}");
        }
    }

    /// The folded leaf is several pages bound into one, so each of them keeps
    /// its own title — and nothing else is titled.
    #[test]
    fn the_folded_leaf_titles_every_menu_it_carries() {
        let u = update("0.1.9");
        let s = state(None, &u);
        let folded = [Menu::Display, Menu::Help];
        let page = content_of(Trigger::More, &folded, &s, 0.0);
        let titles: Vec<&str> = page
            .iter()
            .filter_map(|r| match r {
                Row::Title(t) => Some(*t),
                _ => None,
            })
            .collect();
        assert_eq!(
            titles,
            vec![Menu::Display.title(), Menu::Help.title()],
            "{titles:?}"
        );
    }

    #[test]
    fn every_menu_is_numbered_in_the_bars_reading_order() {
        let numerals: Vec<Option<&str>> = Menu::ALL.iter().map(|m| folio_of(m.title())).collect();
        assert_eq!(
            numerals,
            vec![Some("I"), Some("II"), Some("III"), Some("IV")]
        );
    }

    /// Only a leaf of this register carries a folio; a title from anywhere else
    /// gets a bare head rule rather than somebody else's number.
    #[test]
    fn a_title_that_is_not_a_menu_carries_no_folio() {
        assert_eq!(folio_of("Conflicts"), None);
        assert_eq!(folio_of(""), None);
        assert_eq!(folio_of(&Menu::Help.title().to_lowercase()), None);
    }

    // ─── the ruling (pure geometry) ──────────────────────────────────────────

    fn head_strip(h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, h))
    }

    /// The word of a head is between its two rules, never under both of them.
    #[test]
    fn a_roomy_compartment_is_ruled_over_and_under_its_word() {
        let strip = head_strip(36.0);
        let text = Rect::from_min_size(pos2(12.0, 11.0), vec2(48.0, 14.0));
        let (cap, foot) = compartment_rules(strip, text).expect("room for both rules");
        assert!(cap < text.top(), "cap {cap} is not over the word");
        assert!(foot > text.bottom(), "foot {foot} is not under the word");
        assert!(cap > strip.top() && foot < strip.bottom());
    }

    /// The title bar's height is fixed while the type is not, so a large text
    /// scale squeezes the strip around the word. A rule struck through the word
    /// is worse than no rule at all.
    #[test]
    fn a_squeezed_compartment_is_left_unruled() {
        let text = Rect::from_min_size(pos2(12.0, 1.0), vec2(48.0, 14.0));
        assert_eq!(compartment_rules(head_strip(16.0), text), None);
        // And the degenerate case, where the strip has collapsed entirely.
        assert_eq!(compartment_rules(head_strip(0.0), text), None);
    }

    /// The strip is laid out inside the title bar with the band across its
    /// foot, so an open head has a band to interrupt: the doorway starts where
    /// the compartment ends and runs to the band's own foot.
    #[test]
    fn an_open_head_cuts_its_doorway_through_the_band() {
        let bar = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, chrome::TITLEBAR_H));
        let strip = Rect::from_center_size(bar.center(), vec2(80.0, strip_h()));
        let notch = open_notch(strip, bar).expect("the band crosses the strip's foot");
        assert_eq!(notch.top(), strip.bottom());
        assert_eq!(notch.x_range(), strip.x_range());
        assert!(notch.bottom() < bar.bottom(), "{notch:?}");
        assert!(notch.bottom() > strip.bottom(), "{notch:?}");
    }

    /// Only the window knows where the bar really is. A strip already reaching
    /// the foot, one handed a bar it was never laid into, and one over a window
    /// too narrow to carry a band all cut nothing rather than punching a hole
    /// in the chrome.
    #[test]
    fn a_head_that_cannot_find_the_band_cuts_nothing() {
        let bar = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, chrome::TITLEBAR_H));
        let strip = Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 34.0));
        assert_eq!(open_notch(bar, bar), None);
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 400.0));
        assert_eq!(open_notch(strip, tall), None);
        assert_eq!(
            open_notch(strip, Rect::from_min_size(pos2(0.0, 0.0), vec2(8.0, 46.0))),
            None
        );
    }

    #[test]
    fn a_separator_leaves_its_diamond_clear_of_both_stubs() {
        let span = Rangef::new(0.0, 36.0);
        let r = 3.0;
        let (top, foot) = separator_stubs(span, r).expect("room for both stubs");
        assert!(top.max <= span.center() - r, "{top:?}");
        assert!(foot.min >= span.center() + r, "{foot:?}");
        assert!(top.min >= span.min && foot.max <= span.max);
    }

    /// A stub shorter than the space step reads as a speck beside the diamond,
    /// so a squeezed bar is pivoted by the diamond alone.
    #[test]
    fn a_short_separator_is_the_diamond_alone() {
        assert_eq!(separator_stubs(Rangef::new(0.0, 14.0), 3.0), None);
        assert_eq!(separator_stubs(Rangef::new(0.0, 0.0), 3.0), None);
    }

    // ─── the gutter ──────────────────────────────────────────────────────────

    fn entry_at(top: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, top), vec2(200.0, 20.0))
    }

    /// The rule brackets each block of entries. One rule down the whole leaf
    /// would strike every out-dented section head on the page through.
    #[test]
    fn the_gutter_is_ruled_once_per_block_of_entries() {
        let mut g = Gutter::default();
        g.entry(entry_at(0.0));
        g.entry(entry_at(20.0));
        g.brk();
        g.entry(entry_at(60.0));
        let runs = g.finish();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].1, Rangef::new(0.0, 40.0));
        assert_eq!(runs[1].1, Rangef::new(60.0, 80.0));
    }

    /// A leaf of nothing but heads, and a break with no run in hand, both rule
    /// nothing rather than a zero-length line.
    #[test]
    fn a_leaf_with_no_entries_is_not_ruled() {
        let mut g = Gutter::default();
        g.brk();
        g.brk();
        assert!(g.finish().is_empty());
    }
}
