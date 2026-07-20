//! Centralized user-facing copy for the native GUI.
//!
//! Every string the window shows a person lives here, in one place, so the
//! voice stays consistent as views come and go. The voice is the project's:
//! confident, concrete, a little warm — it explains the mechanism (a lease is
//! taken, a publish is seen by a peer, a copy is quarantined) rather than
//! selling a vibe. No emoji, no exclamation marks, sentence case, and the
//! Golden Invariant stated plainly wherever a deletion is on the table:
//! nothing is removed unless you choose it.
//!
//! Items are `pub const &'static str` where the text is fixed, and small
//! `pub fn … -> String` where a path, count, or size has to be interpolated.
//! No I/O, no allocation beyond the `format!` helpers. Each item's `///` doc
//! names the exact call site so the integrator can wire it without re-reading
//! this file.

// ─── Home (device-wide overview) ─────────────────────────────────────────────

/// Home view title, top of `home()` — replaces the flat "Your device" heading.
pub const HOME_TITLE: &str = "This machine's workshop";

/// Home view subtitle under the title in `home()`; orients toward the sidebar and Ctrl+K.
pub const HOME_SUB: &str = "Every folder this machine syncs. Pick one from the sidebar, or press Ctrl+K to jump to any session or action.";

/// Home warning line in `home()`, shown when `conflicts > 0` across sessions.
pub const HOME_CONFLICTS_NOTE: &str =
    "a session is holding preserved copies — open its Conflicts tab to decide what stays";

/// Section header for the create form in `home()` (`theme::section`).
pub const CREATE_TITLE: &str = "Create a session";

/// Body under CREATE_TITLE in `home()`; what `init` actually does.
pub const CREATE_HINT: &str = "Point at a folder to make it a session. tazamun indexes what is inside and mints a tzm1 invite ticket you can hand to a collaborator.";

/// Section header for the join form in `home()` (`theme::section`).
pub const JOIN_TITLE: &str = "Join with a ticket";

/// Body under JOIN_TITLE in `home()`; what `join` needs and does.
pub const JOIN_HINT: &str = "Paste a tzm1 invite into an empty folder. It fills as the index syncs from the peer who invited you. The ticket carries the session secret, so treat it like a key.";

/// Toast after a new (unregistered) folder is dropped on the window; from the `PrefillInit` arm.
pub const DROP_TOAST_PREFILLED: &str =
    "folder loaded into the create form — press Create to turn it into a session";

// ─── Sidebar ─────────────────────────────────────────────────────────────────

/// Sidebar section header above the session list in `sidebar()` (`theme::section`).
pub const SESSIONS_SECTION: &str = "Sessions";

/// Sidebar empty title in `sidebar()` when the registry has no sessions.
pub const NO_SESSIONS_TITLE: &str = "no sessions here";

/// Sidebar empty hint in `sidebar()` beneath NO_SESSIONS_TITLE.
pub const NO_SESSIONS_HINT: &str = "create one from a folder or join with a ticket, both on Home";

/// Sidebar Home nav-card title in `sidebar()` (extra; pairs with SIDEBAR_HOME_SUB).
pub const SIDEBAR_HOME_TITLE: &str = "Home";

/// Sidebar Home nav-card subtitle in `sidebar()` (extra).
pub const SIDEBAR_HOME_SUB: &str = "every session on this device";

// ─── Overview tab ────────────────────────────────────────────────────────────

/// Members card in `tab_overview()` when the running session has no peers connected.
pub const MEMBERS_EMPTY_RUNNING: &str =
    "no peers connected — share the invite below and they appear here as they dial in";

/// Members card in `tab_overview()` when the session is stopped.
pub const MEMBERS_EMPTY_STOPPED: &str = "session is stopped — start it to reach your peers";

/// Invite card caption in `tab_overview()` above the ticket text/QR.
pub const INVITE_CAUTION: &str = "Share this ticket to bring in a collaborator. It carries the session secret, so anyone holding it can join — treat it like a key.";

/// Invite card in `tab_overview()` when there is no ticket to show (`None` arm).
pub const INVITE_EMPTY: &str = "no invite to show — start the session to mint a fresh ticket";

// ─── Files tab ───────────────────────────────────────────────────────────────

/// Files empty-state title in `tab_files()` when the index carries no files.
pub const FILES_EMPTY_TITLE: &str = "Nothing tracked here";

/// Files empty-state hint in `tab_files()`; the lock/edit/unlock mechanism that lands a file here.
pub const FILES_EMPTY_HINT: &str = "files land here once a publish has reached a peer. The path is lock, edit, unlock — releasing the lease is the publish";

/// Files list message in `tab_files()` when the filter matches nothing.
pub const FILES_FILTER_EMPTY: &str = "no files match that filter";

/// Mini-label in `tab_files()` shown in a file row while the session is stopped.
pub const FILES_STOPPED_ACTION: &str = "start the session to edit";

/// Files banner in `tab_files()` when the index is paged for a large folder.
pub fn files_truncated(shown: usize, total: usize) -> String {
    format!(
        "showing the first {shown} of {total} files — a folder this large is paged for the view, the full set still syncs"
    )
}

/// Mini-label in `version_row()` shown in a version row while the session is stopped (extra).
pub const VERSION_STOPPED_ACTION: &str = "start the session to act";

// ─── Conflicts tab ───────────────────────────────────────────────────────────

/// Conflicts empty-state title in `tab_conflicts()`.
pub const CONFLICTS_EMPTY_TITLE: &str = "No conflicts waiting";

/// Conflicts empty-state hint in `tab_conflicts()`; the both-copies promise, kept.
pub const CONFLICTS_EMPTY_HINT: &str = "every preserved copy is resolved. When tazamun cannot safely pick a winner it keeps both here rather than overwrite, so nothing is waiting on you now";

/// Conflicts intro paragraph in `tab_conflicts()` above the rows.
pub const CONFLICTS_INTRO: &str = "Each row is a copy tazamun set aside instead of overwriting it. Resolving a row is the only step here that deletes bytes, and only the ones you point it at.";

/// Confirm title for "keep theirs" in `tab_conflicts()` (`self.ask`).
pub const CONFLICT_DISCARD_TITLE: &str = "Discard this preserved copy?";

/// Confirm body for "keep theirs" in `tab_conflicts()` (`self.ask`); danger path.
pub fn conflict_discard_body(name: &str) -> String {
    format!(
        "This deletes the quarantined copy {name} and keeps the version currently synced. Nothing else is touched, and this one deletion cannot be undone."
    )
}

/// Note in `tab_conflicts()` shown when the session is stopped (resolution runs through the daemon).
pub const CONFLICTS_STOPPED_NOTE: &str = "start the session to resolve — every resolution runs through the daemon, so the lease and publish rules still hold";

// ─── History tab ─────────────────────────────────────────────────────────────

/// History empty-state title in `tab_history()`.
pub const HISTORY_EMPTY_TITLE: &str = "No versions recorded";

/// History empty-state hint in `tab_history()` while the session is running.
pub const HISTORY_EMPTY_HINT_RUNNING: &str = "a version is recorded every time a lease is released with changes — the first published edit starts the timeline";

/// History empty-state hint in `tab_history()` while the session is stopped.
pub const HISTORY_EMPTY_HINT_STOPPED: &str = "start the session to read its version history";

/// Confirm title for a restore in `version_row()` (`self.ask`).
pub const RESTORE_TITLE: &str = "Restore this version?";

/// Confirm body for a restore in `version_row()` (`self.ask`); the replaced content is kept in history.
pub fn restore_body(path: &str, n: u64, size_human: &str) -> String {
    format!(
        "{path} goes back to version {n} ({size_human}). The content it has now is pushed to history first, so the restore adds a version instead of losing one."
    )
}

// ─── Audit tab ───────────────────────────────────────────────────────────────

/// Audit empty-state title in `tab_audit()`.
pub const AUDIT_EMPTY_TITLE: &str = "No audit events";

/// Audit empty-state hint in `tab_audit()`.
pub const AUDIT_EMPTY_HINT: &str = "locks, publishes, restores, and quarantines on this folder are logged here — readable even while the session is stopped";

// ─── Settings tab ────────────────────────────────────────────────────────────

/// Settings empty-state title in `tab_settings()` when the daemon is not running.
pub const SETTINGS_NEED_DAEMON_TITLE: &str = "Settings need a running daemon";

/// Settings empty-state hint in `tab_settings()` when the daemon is not running.
pub const SETTINGS_NEED_DAEMON_HINT: &str = "start the session to read and change its configuration — settings are served by the running daemon";

/// Caption under the "Live settings" section in `tab_settings()`.
pub const LIVE_SECTION_NOTE: &str =
    "Changes here take hold the moment you press Apply and are written to state.json — no restart.";

/// Caption under the "Fixed until restart" section in `tab_settings()`.
pub const FIXED_SECTION_NOTE: &str = "these are read once at startup — change them with `tazamun config set` or `tazamun setup`, then restart the session for it to take hold";

/// Caption above the peer-naming form in `tab_settings()`; names are local only.
pub const PEERS_NAME_HINT: &str = "Give a peer a name you will recognize. It is stored on this device only — it never leaves and never reaches the peer.";

// ─── Toasts (worker functions) ───────────────────────────────────────────────

/// Toast after copying the invite in `tab_overview()`.
pub const TOAST_TICKET_COPIED: &str = "ticket copied to the clipboard";

/// Toast after copying the folder path in `session_view()`.
pub const TOAST_PATH_COPIED: &str = "folder path copied to the clipboard";

/// Toast on a successful GUI-hosted start in `start_session()`.
pub const TOAST_STARTED_HOSTED: &str =
    "started — hosted in this window, and stopped cleanly when you close it";

/// Toast on a successful stop in `stop_session()`.
pub const TOAST_STOPPED: &str = "session stopped";

/// Toast in `stop_session()` when shutdown exceeds the bounded timeout.
pub const TOAST_STOP_TIMEOUT: &str = "stop timed out — the actor did not answer in time, so the session was dropped rather than left half-running";

/// Toast in `start_session()` when a daemon already owns this folder.
pub const TOAST_ALREADY_RUNNING: &str = "a daemon is already running for this folder";

/// Toast in `stop_session()` when nothing is running for this folder.
pub const TOAST_NOT_RUNNING: &str = "no daemon is running for this folder";

/// Toast in `set_paused()` when a live supervisor accepts a pause.
pub const TOAST_PAUSED_LIVE: &str = "paused — the supervisor is holding this session";

/// Toast in `set_paused()` when a live supervisor accepts a resume.
pub const TOAST_RESUMED_LIVE: &str = "resumed — syncing again";

/// Toast in `set_paused()` on a folder that is not a session (extra).
pub const TOAST_NOT_SESSION_FOLDER: &str = "not a session folder — nothing to pause here";

/// Toast in `set_paused()` when no supervisor is live and the flag is deferred to the registry.
pub fn toast_paused_deferred(paused: bool) -> String {
    let state = if paused { "paused" } else { "resumed" };
    format!("{state} — no supervisor is running, so this takes effect on the next `start --all`")
}

// ─── Command palette (Ctrl+K) ────────────────────────────────────────────────

/// Placeholder for the palette input in `palette_overlay()`.
pub const PALETTE_HINT: &str = "jump to a session, start or stop one, or open a tab";

// ─── First light (zero-session onboarding) ───────────────────────────────────

/// First-light panel title in `home()` when no sessions exist.
pub const FL_TITLE: &str = "First light";

/// First-light subtitle under the title.
pub const FL_SUB: &str = "Three steps and this machine is part of a session. Nothing leaves it until you publish, and nothing arrives unverified.";

/// Step 1 title (hosts the create/join forms).
pub const FL_STEP1_TITLE: &str = "Begin";

/// Step 1 hint above the forms.
pub const FL_STEP1_HINT: &str =
    "point at a folder to create a session, or paste a ticket to join one";

/// Step 2 title (future step).
pub const FL_STEP2_TITLE: &str = "Invite";

/// Step 2 hint.
pub const FL_STEP2_HINT: &str =
    "the session mints a tzm1 ticket — hand it to a collaborator like a key";

/// Step 3 title (future step).
pub const FL_STEP3_TITLE: &str = "Sync, truthfully";

/// Step 3 hint — the Golden Invariant in one breath.
pub const FL_STEP3_HINT: &str =
    "edits travel under leases; anything ambiguous keeps both copies and says so";

// ─── Field hints (centralized so placeholder voice cannot drift) ─────────────

/// Placeholder in the create-session path field.
pub const HINT_FOLDER: &str = "path to a folder";

/// Placeholder in the join path field.
pub const HINT_EMPTY_FOLDER: &str = "path to an empty folder";

/// Placeholder in the ticket field.
pub const HINT_TICKET: &str = "paste a tzm1 ticket";

/// Placeholder in the Files search field.
pub const HINT_FILTER: &str = "filter by name";

/// Placeholder in the peer-id field (Settings).
pub const HINT_PEER_ID: &str = "peer id, a prefix is enough";

/// Placeholder in the peer-name field (Settings).
pub const HINT_PEER_NAME: &str = "a name you will recognize";

/// Placeholder in the version tag field.
pub const HINT_TAG: &str = "name this version";

// ─── Files ordering ──────────────────────────────────────────────────────────

/// Caption before the Files sort toggle.
pub const FILES_SORT_LABEL: &str = "order by";

/// Files sort toggle: alphabetical.
pub const FILES_SORT_NAME: &str = "name";

/// Files sort toggle: heaviest first.
pub const FILES_SORT_SIZE: &str = "weight";

// ─── Conflict scales (the balance drawn on each conflict card) ───────────────

/// Left pan title: the copy tazamun set aside.
pub const BAL_KEPT_TITLE: &str = "preserved copy";

/// Right pan title: what is currently synced at that path.
pub const BAL_LIVE_TITLE: &str = "synced version";

/// Right pan time line — the synced side has no quarantine moment.
pub const BAL_LIVE_WHEN: &str = "in the folder now";

/// Right pan note when an indexed file exists at the path.
pub const BAL_LIVE_NOTE: &str = "this is what your peers hold today";

/// Right pan note when nothing is indexed at the path.
pub const BAL_LIVE_MISSING: &str =
    "nothing is indexed at this path, so only the copy holds these bytes";

// ─── Peers tab ───────────────────────────────────────────────────────────────

/// Peers empty-state title in `tab_peers()`.
pub const PEERS_EMPTY_TITLE: &str = "No peers on the wire";

/// Peers empty-state hint in `tab_peers()` while the session is running.
pub const PEERS_EMPTY_RUNNING: &str =
    "hand someone the invite and their connection appears here, path and all";

/// Peers empty-state hint in `tab_peers()` while the session is stopped.
pub const PEERS_EMPTY_STOPPED: &str = "start the session to dial your peers";

/// Intro line at the top of `tab_peers()` above the peer cards.
pub const PEERS_INTRO: &str = "Each card is a live connection: its path, its round-trip, and what has moved over it. The sparkline is this window's own record of the link.";

// ─── Drag-and-drop overlay + hover tooltips (extras) ─────────────────────────

/// Title on the drop overlay card in `dropzone::overlay_if_hovering()` (extra).
pub const DROP_OVERLAY_TITLE: &str = "Drop a folder";

/// Hint on the drop overlay card in `dropzone::overlay_if_hovering()` (extra).
pub const DROP_OVERLAY_HINT: &str =
    "creates a new session here, or opens it if this folder is already one";

/// Reject reason in `dropzone::take_drop()` when the drop had no path (extra).
pub const DROP_REJECT_NO_PATH: &str = "this drop carried no folder path";

/// Reject reason in `dropzone::take_drop()` when a file, not a folder, was dropped (extra).
pub const DROP_REJECT_NOT_FOLDER: &str = "drop a folder, not a single file";

/// Hover tooltip on the "Browse…" buttons in `home()` (extra).
pub const BROWSE_HOVER: &str = "open the system folder picker, or type the path by hand";

/// Hover tooltip on the "Open folder" button in `session_view()` (extra).
pub const OPEN_FOLDER_HOVER: &str = "reveal this folder in your file manager";

/// Hover tooltip on the "Copy path" button in `session_view()` (extra).
pub const COPY_PATH_HOVER: &str = "copy this folder's path to the clipboard";

/// Title of the `?` shortcuts sheet (P34).
pub const SHORTCUTS_TITLE: &str = "Keys";

/// Subtitle under the shortcuts sheet title (P34).
pub const SHORTCUTS_SUB: &str = "everything this window answers to";

/// Label above the text-size stepper in Settings (P34).
pub const A11Y_TEXT_SIZE: &str = "Text size";

/// Hint under the text-size stepper in Settings (P34).
pub const A11Y_TEXT_SIZE_HINT: &str = "scales every label in the window; Ctrl+0 puts it back";

/// Screen-reader label for the close button in the custom title bar (P34).
pub const WIN_CLOSE: &str = "close window";

/// Screen-reader label for the maximize button when the window is restored (P34).
pub const WIN_MAXIMIZE: &str = "maximize window";

/// Screen-reader label for the same button once the window is maximized (P34).
pub const WIN_RESTORE: &str = "restore window";

/// Screen-reader label for the minimize button in the custom title bar (P34).
pub const WIN_MINIMIZE: &str = "minimize window";

/// Label on the skip-to-content link, revealed only while it holds focus (P34).
pub const SKIP_TO_CONTENT: &str = "skip to content";

/// Heading of the app-wide display preferences card on the Home screen (P34).
pub const DISPLAY_TITLE: &str = "Display";

/// Screen-reader label for the text-size increase stepper (P34).
pub const A11Y_BIGGER: &str = "larger text";

/// Screen-reader label for the text-size decrease stepper (P34).
pub const A11Y_SMALLER: &str = "smaller text";
