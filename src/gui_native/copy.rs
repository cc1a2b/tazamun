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
pub const PEERS_INTRO: &str = "One entry per device: how it is reached, its round-trip, and what has moved over the link. The trend is this window's own record, not the daemon's.";

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

// ─── custody vocabulary (P35) ────────────────────────────────────────────────
// One word per state, used wherever custody is named so the register reads the
// same on every tab.

/// Custody column when this device holds the lease.
pub const CUSTODY_HELD_YOU: &str = "held · you";

/// Prefix for the custody column when another device holds the lease; followed
/// by the holder's short id or its label.
pub const CUSTODY_HELD_BY: &str = "held ·";

/// Custody column when nothing holds the path — the resting state.
pub const CUSTODY_FREE: &str = "free";

// ─── verbs ───────────────────────────────────────────────────────────────────

/// Take the exclusive lease on a path.
pub const ACTION_LOCK: &str = "Lock";

/// Give the lease back and publish what changed.
pub const ACTION_UNLOCK: &str = "Unlock";

/// Bring an older version back as the current one.
pub const ACTION_RESTORE: &str = "Restore";

/// Keep a version out of the reach of garbage collection.
pub const ACTION_PIN: &str = "Pin";

/// Release a pinned version.
pub const ACTION_UNPIN: &str = "Unpin";

/// Name a version.
pub const ACTION_TAG: &str = "Tag";

/// Retry a read that failed.
pub const ACTION_RETRY: &str = "Try again";

// ─── failed reads (P35) ──────────────────────────────────────────────────────
// A read that failed must never render as an empty state: "no conflicts
// waiting" over an unreadable directory tells the user their preserved bytes
// are resolved when the window never managed to look.

/// Heading over a session whose detail could not be loaded.
pub const SESSION_LOAD_FAILED: &str = "This session could not be read";

/// Heading when the file list could not be read.
pub const FILES_FAILED: &str = "The file list could not be read";

/// Heading when the version history could not be read.
pub const HISTORY_FAILED: &str = "The version history could not be read";

/// Heading when the conflicts directory could not be read.
pub const CONFLICTS_FAILED: &str = "The preserved copies could not be read";

/// Heading when the audit ledger could not be read.
pub const AUDIT_FAILED: &str = "The audit ledger could not be read";

/// Heading when the peer list could not be read.
pub const PEERS_FAILED: &str = "The peer list could not be read";

/// Hint under the files register when a filter matches nothing.
pub const FILES_FILTER_EMPTY_HINT: &str = "clear the filter to see every tracked path";

/// Explains why an edit verb is disabled on a role that cannot take a lease.
pub fn role_cannot_edit(role: &str) -> String {
    format!(
        "this folder is joined as {role}, which cannot take a lease — \
         rejoin with an editor invite to make changes here"
    )
}

/// Says plainly that the ledger shown is a prefix of the whole.
pub fn history_truncated(shown: usize, total: usize) -> String {
    format!(
        "showing the {shown} most recent of {total} versions — \
         use `tazamun log` for the full ledger"
    )
}

/// The same, for the audit ledger.
pub fn audit_truncated(shown: usize, total: usize) -> String {
    format!(
        "showing the {shown} most recent of {total} entries — \
         use `tazamun log` for the full ledger"
    )
}

// ─── conflict resolution (P35) ───────────────────────────────────────────────
// Ordered by what each choice does to the user's bytes. `keep both` deletes
// nothing and is therefore the default offer; the two that destroy bytes are
// both styled as destructive and both ask first.

/// The safe resolution: restore the preserved copy beside the synced file.
pub const CONFLICT_KEEP_BOTH: &str = "Keep both";

/// Overwrite the synced file with the preserved copy, then delete the copy.
pub const CONFLICT_KEEP_MINE: &str = "Keep mine";

/// Delete the preserved copy and leave the synced file alone.
pub const CONFLICT_KEEP_THEIRS: &str = "Keep theirs";

/// Explains what `keep both` will do, shown beside the button.
pub const CONFLICT_KEEP_BOTH_NOTE: &str =
    "restores the preserved copy under a new name — nothing is overwritten and nothing is deleted";

/// Title of the confirm shown before `keep mine`.
pub const CONFLICT_MINE_TITLE: &str = "Overwrite the synced file?";

/// Verb on the `keep mine` confirm.
pub const CONFLICT_MINE_VERB: &str = "Overwrite and discard";

/// Says exactly which bytes move and which are destroyed.
pub fn conflict_mine_body(target: &str, kept: &str) -> String {
    format!(
        "The preserved copy is written over {target} and published to every peer, \
         replacing what they hold. The preserved copy ({kept}) is then deleted. \
         The version being replaced is pushed to history first, so it can be restored — \
         but the preserved copy cannot. Keep both instead if you are unsure."
    )
}

/// Heading over the conflict detail pane.
pub const CONFLICT_WEIGH_TITLE: &str = "Weigh the two copies";

/// Shown when the preserved copy has no recorded original path.
pub const CONFLICT_ORPHAN_NOTE: &str =
    "this copy's original path was never recorded, so it can only be restored under a new name";

/// Verb on the `keep theirs` confirm.
pub const CONFLICT_DISCARD_VERB: &str = "Discard the copy";

/// Tag on a peer reached over the local network.
pub const PEER_LAN: &str = "LAN";

/// Tag on a peer reached through a relay rather than directly.
pub const PEER_RELAY: &str = "relay";

/// Hover detail for a peer entry: the totals that do not earn a column.
pub fn peer_lifetime(up: &str, down: &str, ttd_ms: Option<u64>) -> String {
    let mut s = format!("lifetime — sent {up}, received {down}");
    if let Some(ms) = ttd_ms {
        s.push_str(&format!(
            "\ndirect path found in {:.1}s",
            ms as f64 / 1000.0
        ));
    }
    s
}

// ─── appearance (P35) ────────────────────────────────────────────────────────

/// Name of the palette setting.
pub const THEME_TITLE: &str = "Palette";

/// What the palette setting does.
pub const THEME_HINT: &str = "ink on a dark desk, ink on paper, or maximum separation";

/// Name of the register-density setting.
pub const DENSITY_TITLE: &str = "Density";

/// What the density setting does.
pub const DENSITY_HINT: &str = "how many entries a register fits on one screen";

/// Name of the motion setting.
pub const MOTION_TITLE: &str = "Motion";

/// What the motion setting does.
pub const MOTION_HINT: &str = "reveals, sweeps and the breathing seal can be turned off";

/// The option that keeps animation.
pub const MOTION_FULL: &str = "Full";

/// The option that removes it.
pub const MOTION_REDUCED: &str = "Reduced";

/// The verb that backs out of a dialog without doing anything.
pub const ACTION_CANCEL: &str = "Cancel";

// ─── the three lease preconditions (P35) ─────────────────────────────────────
// Every synced file is read-only. A lease is granted only when all three hold.
// The daemon names which one failed; the window must say what that means and
// what clears it, because this is the interaction the product exists for.

/// Confirms the file is now writable, which is the fact the user needs.
pub const TOAST_LOCKED: &str = "locked — the file is writable until you unlock it";

/// Confirms the lease was given back and the bytes went out.
pub const TOAST_UNLOCKED: &str = "unlocked — your changes are published to every peer";

/// Dismisses the refusal panel.
pub const REFUSED_DISMISS: &str = "Dismiss";

/// Names the precondition in the user's terms.
pub fn precondition_title(p: &str) -> &'static str {
    match p {
        "REACHABILITY" => "No peer is reachable",
        "FRESHNESS" => "This copy is not up to date yet",
        "LEASE" => "Someone else holds the pen",
        _ => "This folder does not allow edits",
    }
}

/// Explains why the rule exists, so the refusal reads as a design rather than
/// as a fault.
pub fn precondition_why(p: &str) -> &'static str {
    match p {
        "REACHABILITY" => {
            "Strict checkout hands the pen to one device at a time. With no peer connected there \
             is nobody to agree the handover, so edits are refused rather than risked."
        }
        "FRESHNESS" => {
            "Taking the pen on a copy that is behind would publish old bytes over newer ones. \
             The lease waits until this device holds the current version."
        }
        "LEASE" => {
            "Exactly one device may hold a file at a time. The lease is released on unlock, or \
             when it expires on its own."
        }
        _ => {
            "This folder was joined with a role that cannot take a lease, so every edit path is \
             refused here."
        }
    }
}

/// Introduces the device currently holding the lease.
pub const REFUSED_HELD_BY: &str = "held by";

/// Introduces the peers the daemon consulted before refusing.
pub const REFUSED_PEERS: &str = "peers consulted";

/// Sends the user to the tab that answers a reachability refusal.
pub const REFUSED_SEE_PEERS: &str = "See peers";

/// Clears the refusal so the user can try the edit again.
pub const REFUSED_RETRY: &str = "Try again";

/// Says how far the blocking pull has got.
pub fn refused_pull_progress(percent: u64) -> String {
    format!("this path is {percent}% pulled")
}

/// The enabled side of a two-state setting.
pub const TOGGLE_ON: &str = "on";

/// The disabled side of a two-state setting.
pub const TOGGLE_OFF: &str = "off";

// ─── the standing statement (P36) ────────────────────────────────────────────
// The Overview is the one view that is narrative rather than tabular. It opens
// with a sentence answering the question the window exists to answer — who
// holds what here, right now — instead of a strip of numbers that makes the
// reader assemble the answer themselves.

/// Everything the standing statement is derived from.
pub struct Standing {
    pub running: bool,
    pub strict: bool,
    pub files: usize,
    pub held_by_you: usize,
    pub held_by_peers: usize,
    pub conflicts: usize,
    pub peers_online: usize,
    pub peers_total: usize,
}

/// English has no rule that makes "copy" into "copys", so callers name both
/// forms rather than relying on a naive trailing `s`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The sentence at the head of the Overview.
pub fn standing(s: &Standing) -> String {
    if !s.running {
        return "This session is stopped. Nothing is syncing, and no file here can be edited."
            .into();
    }
    let files = plural(s.files, "file", "files");
    let custody = match (s.held_by_you, s.held_by_peers) {
        (0, 0) => "nothing is held".to_string(),
        (y, 0) => format!("you hold {}", plural(y, "file", "files")),
        (0, p) => format!("{} held elsewhere", plural(p, "file is", "files are")),
        (y, p) => format!(
            "you hold {}, {} held elsewhere",
            plural(y, "file", "files"),
            plural(p, "file is", "files are")
        ),
    };
    let reach = if s.peers_total == 0 {
        "no peer has joined yet".to_string()
    } else if s.peers_online == 0 {
        "no peer is reachable".to_string()
    } else if s.peers_online == s.peers_total {
        format!("in step with {}", plural(s.peers_online, "peer", "peers"))
    } else {
        format!("{} of {} peers reachable", s.peers_online, s.peers_total)
    };
    format!("{files}, {custody}, {reach}.")
}

/// One thing the session needs the reader to deal with. The Overview shows
/// these only when they apply, so an untroubled session is short.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attention {
    /// Preserved copies are waiting on a decision.
    Conflicts(usize),
    /// Strict mode plus no reachable peer means every edit is refused.
    NoPeers,
    /// Files are held by this device and their changes are unpublished.
    Unpublished(usize),
    /// The session is stopped.
    Stopped,
}

/// What this session needs, most pressing first. Empty when all is well.
pub fn attention(s: &Standing) -> Vec<Attention> {
    let mut out = Vec::new();
    if !s.running {
        out.push(Attention::Stopped);
        return out;
    }
    if s.conflicts > 0 {
        out.push(Attention::Conflicts(s.conflicts));
    }
    if s.strict && s.peers_online == 0 && s.peers_total > 0 {
        out.push(Attention::NoPeers);
    }
    if s.held_by_you > 0 {
        out.push(Attention::Unpublished(s.held_by_you));
    }
    out
}

impl Attention {
    /// The line shown in the attention band.
    pub fn line(self) -> String {
        match self {
            Self::Conflicts(n) => format!(
                "{} waiting on your decision — nothing is lost until you choose",
                plural(n, "preserved copy is", "preserved copies are")
            ),
            Self::NoPeers => {
                "No peer is reachable, so every edit is refused until one returns.".into()
            }
            Self::Unpublished(n) => format!(
                "{} still held by you — unlock to publish your changes",
                plural(n, "file is", "files are")
            ),
            Self::Stopped => "Start this session to sync and to edit.".into(),
        }
    }

    /// The verb that answers it, and where it leads.
    pub fn verb(self) -> Option<&'static str> {
        match self {
            Self::Conflicts(_) => Some("Resolve"),
            Self::NoPeers => Some("See peers"),
            Self::Unpublished(_) => Some("See files"),
            Self::Stopped => Some("Start"),
        }
    }
}

#[cfg(test)]
mod standing_tests {
    use super::*;

    fn base() -> Standing {
        Standing {
            running: true,
            strict: true,
            files: 5,
            held_by_you: 0,
            held_by_peers: 0,
            conflicts: 0,
            peers_online: 1,
            peers_total: 1,
        }
    }

    #[test]
    fn a_stopped_session_says_so_first() {
        let s = Standing {
            running: false,
            ..base()
        };
        assert!(standing(&s).starts_with("This session is stopped"));
        assert_eq!(attention(&s), vec![Attention::Stopped]);
    }

    #[test]
    fn a_quiet_session_needs_nothing() {
        assert!(attention(&base()).is_empty());
        assert_eq!(
            standing(&base()),
            "5 files, nothing is held, in step with 1 peer."
        );
    }

    #[test]
    fn one_file_is_not_pluralised() {
        let s = Standing { files: 1, ..base() };
        assert!(standing(&s).starts_with("1 file,"), "{}", standing(&s));
    }

    #[test]
    fn custody_names_both_sides() {
        let s = Standing {
            held_by_you: 2,
            held_by_peers: 1,
            ..base()
        };
        let t = standing(&s);
        assert!(t.contains("you hold 2 files"), "{t}");
        assert!(t.contains("1 file is held elsewhere"), "{t}");
    }

    #[test]
    fn an_unreachable_mesh_is_stated_and_flagged() {
        let s = Standing {
            peers_online: 0,
            peers_total: 2,
            ..base()
        };
        assert!(
            standing(&s).contains("no peer is reachable"),
            "{}",
            standing(&s)
        );
        assert!(attention(&s).contains(&Attention::NoPeers));
    }

    /// Easy mode still syncs without a peer, so the refusal warning must not
    /// appear there — it would be false.
    #[test]
    fn easy_mode_does_not_warn_about_reachability() {
        let s = Standing {
            strict: false,
            peers_online: 0,
            peers_total: 2,
            ..base()
        };
        assert!(!attention(&s).contains(&Attention::NoPeers));
    }

    /// A session nobody has joined is not the same as one whose peers are down.
    #[test]
    fn a_solo_session_is_not_reported_as_unreachable() {
        let s = Standing {
            peers_online: 0,
            peers_total: 0,
            ..base()
        };
        assert!(
            standing(&s).contains("no peer has joined yet"),
            "{}",
            standing(&s)
        );
        assert!(!attention(&s).contains(&Attention::NoPeers));
    }

    #[test]
    fn conflicts_outrank_everything_else_present() {
        let s = Standing {
            conflicts: 2,
            held_by_you: 1,
            peers_online: 0,
            peers_total: 1,
            ..base()
        };
        assert_eq!(attention(&s).first(), Some(&Attention::Conflicts(2)));
    }

    #[test]
    fn every_attention_carries_a_line_and_a_verb() {
        for a in [
            Attention::Conflicts(1),
            Attention::NoPeers,
            Attention::Unpublished(1),
            Attention::Stopped,
        ] {
            assert!(!a.line().trim().is_empty());
            assert!(a.verb().is_some_and(|v| !v.trim().is_empty()));
        }
    }

    #[test]
    fn a_partially_reachable_mesh_reports_the_fraction() {
        let s = Standing {
            peers_online: 1,
            peers_total: 3,
            ..base()
        };
        assert!(
            standing(&s).contains("1 of 3 peers reachable"),
            "{}",
            standing(&s)
        );
    }
}

// ─── the Overview's own vocabulary (P36) ─────────────────────────────────────

/// Heading over the session's fixed facts.
pub const OVERVIEW_STATE: &str = "This session";

/// Heading over transfers and events.
pub const OVERVIEW_MOVEMENT: &str = "Movement";

/// Heading over the invite disclosure.
pub const OVERVIEW_INVITE: &str = "Bring someone in";

/// Row label: strict or easy.
pub const STATE_MODE: &str = "Mode";

/// Row label: the role this device joined as.
pub const STATE_ROLE: &str = "Your role";

/// Row label: how many paths are tracked.
pub const STATE_FILES: &str = "Tracked";

/// Row label: reachable peers over known peers.
pub const STATE_PEERS: &str = "Peers reachable";

/// Row label: preserved copies awaiting a decision.
pub const STATE_CONFLICTS: &str = "Preserved copies";

/// Strict checkout: one pen, handed over deliberately.
pub const MODE_STRICT: &str = "strict — one holder at a time";

/// Easy mode: local edits auto-publish.
pub const MODE_EASY: &str = "easy — local edits publish themselves";

/// Stand-in when the daemon has not reported a value.
pub const UNKNOWN: &str = "unknown";

/// Label before the invite role choice.
pub const INVITE_ROLE: &str = "Joins as";

/// Label before the invite expiry choice.
pub const INVITE_EXPIRY: &str = "Expires";

/// The role an unscoped invite grants.
pub const INVITE_ROLE_DEFAULT: &str = "editor";

/// The roles an invite can be scoped to.
pub const INVITE_ROLES: &[&str] = &["editor", "viewer", "archive"];

/// The expiries offered, and what each means in milliseconds.
pub const INVITE_TTLS: &[(&str, Option<u64>)] = &[
    ("1 hour", Some(60 * 60 * 1000)),
    ("1 day", Some(24 * 60 * 60 * 1000)),
    ("7 days", Some(7 * 24 * 60 * 60 * 1000)),
    ("never", None),
];

/// Mints a ticket with the chosen scope.
pub const INVITE_MINT: &str = "Mint a ticket";

/// Copies the current ticket.
pub const INVITE_COPY: &str = "Copy ticket";

/// Summarises the transfer queue.
pub fn transfer_meta(backlog: usize, resuming: usize, cap: Option<String>) -> String {
    let mut s = format!("{backlog} queued · {resuming} resuming");
    if let Some(c) = cap {
        s.push_str(&format!(" · capped at {c}/s"));
    }
    s
}

// ─── work in flight (P36) ────────────────────────────────────────────────────

/// Abandons an in-flight command at its next safe boundary.
pub const WORKING_CANCEL: &str = "Cancel";

/// Shown once a command has been running long enough to be worth remarking on.
pub const WORKING_SLOW: &str = "this is taking longer than usual";

/// Says which step of a guided sequence is running.
pub fn working_step(what: &str, subject: &str, step: Option<(u8, u8)>) -> String {
    match step {
        Some((n, of)) => format!("{what} {subject} — step {n} of {of}"),
        None => format!("{what} {subject}"),
    }
}

/// Says how old the data on screen is when a session stops answering.
pub fn stale_for(seconds: u64) -> String {
    if seconds < 60 {
        format!("this session last answered {seconds}s ago — showing the last good read")
    } else {
        format!(
            "this session last answered {}m ago — showing the last good read",
            seconds / 60
        )
    }
}

/// Shown while a poll is outstanding but not yet late.
pub const REACH_SLOW: &str = "waiting for this session to answer";

/// Closes a report.
pub const REPORT_CLOSE: &str = "Close";

/// Copies a report's body.
pub const REPORT_COPY: &str = "Copy";

/// Opens the address a report names.
pub const REPORT_OPEN: &str = "Open";

/// Suspends syncing without stopping the daemon.
pub const ACTION_PAUSE: &str = "Pause";

/// Resumes a paused session.
pub const ACTION_RESUME: &str = "Resume";

// ─── the session menu (P36) ──────────────────────────────────────────────────

/// Opens the session's less-used operations.
pub const MENU_MORE: &str = "More";

/// Runs the daemon's connectivity self-check.
pub const MENU_DOCTOR: &str = "Run diagnostics";

/// Starts the web dashboard and reports its address.
pub const MENU_DASHBOARD: &str = "Open the dashboard";

/// Drops unreferenced blobs.
pub const MENU_GC: &str = "Reclaim disk space";

/// Rotates the session secret.
pub const MENU_REKEY: &str = "Rotate the session key";

/// Title of the rekey confirm.
pub const REKEY_TITLE: &str = "Rotate this session's key?";

/// Verb on the rekey confirm.
pub const REKEY_VERB: &str = "Rotate the key";

/// Says exactly what rotating costs, because it is not reversible and it
/// invalidates what other people are holding.
pub const REKEY_BODY: &str = "Every existing invite stops working immediately, and every peer must be re-invited with a \
     fresh ticket before it can sync again. This is the only way to revoke access from someone \
     who already has a ticket. It cannot be undone.";

// ─── per-file operations (P36) ───────────────────────────────────────────────

/// Joins the daemon's waitlist for a path someone else holds.
pub const ACTION_WAIT: &str = "Wait";

/// Explains what waiting does, since it is not obvious from one word.
pub const ACTION_WAIT_HOVER: &str =
    "join the queue for this file — it is taken for you as soon as the current holder releases it";

/// Renames a synced path under a lease.
pub const MENU_RENAME: &str = "Rename…";

/// Shows what changed against the newest kept version.
pub const MENU_DIFF: &str = "What changed";

/// Copies the relative path.
pub const MENU_COPY_PATH: &str = "Copy path";

/// Shown when a diff is asked for on a path with no history to compare against.
pub const DIFF_NO_VERSIONS: &str = "no kept version to compare against yet";

/// Title of the rename dialog.
pub const RENAME_TITLE: &str = "Rename this file";

/// Explains why renaming goes through the app rather than the file manager.
pub const RENAME_BODY: &str = "A rename in your file manager is half a delete, and the delete half is reverted by design. \
     Renaming here takes the lease, moves the file and publishes the move as one act.";

/// Verb on the rename dialog.
pub const RENAME_VERB: &str = "Rename and publish";

// ─── pruning preserved copies (P36) ──────────────────────────────────────────

/// Label before the prune age choice.
pub const PRUNE_OLDER_THAN: &str = "Older than";

/// The default cutoff: a month.
pub const PRUNE_AGE_DEFAULT: u64 = 30 * 24 * 60 * 60 * 1000;

/// The cutoffs offered.
pub const PRUNE_AGES: &[(&str, u64)] = &[
    ("7 days", 7 * 24 * 60 * 60 * 1000),
    ("30 days", 30 * 24 * 60 * 60 * 1000),
    ("90 days", 90 * 24 * 60 * 60 * 1000),
];

/// Verb on the prune control.
pub const PRUNE_VERB: &str = "Delete them";

/// Title of the prune confirm.
pub const PRUNE_TITLE: &str = "Delete these preserved copies?";

/// Says what is about to go, before it goes.
pub fn prune_body(count: usize, bytes: &str) -> String {
    format!(
        "{count} preserved copies ({bytes}) are older than the cutoff and will be deleted from \
         disk. Each one is a version of a file that was preserved because two histories diverged; \
         once deleted they cannot be restored. Nothing currently synced is touched."
    )
}

/// The standing count beside the prune control.
pub fn prune_preview(count: usize, bytes: &str) -> String {
    if count == 0 {
        "nothing is older than the cutoff".into()
    } else {
        format!("{count} copies · {bytes}")
    }
}

// ─── the audit filters (P36) ─────────────────────────────────────────────────

/// Placeholder in the audit filter field.
pub const AUDIT_FILTER: &str = "filter by path, peer or detail";

/// The option that clears the kind filter.
pub const AUDIT_ALL_KINDS: &str = "all";

/// Shown when a filter matches no ledger entry.
pub const AUDIT_FILTER_EMPTY: &str = "No entry matches this filter";

/// What to do about it.
pub const AUDIT_FILTER_EMPTY_HINT: &str = "clear the filter to see the whole ledger";

// ─── machine upkeep (P36) ────────────────────────────────────────────────────

/// Heading over the machine-level section of Settings.
pub const MAINTENANCE_TITLE: &str = "This machine";

/// What the supervisor is for.
pub const SUPERVISOR_NOTE: &str = "The supervisor starts your sessions again after a reboot or a sign-out. Without it, syncing \
     stops when this machine does and does not come back on its own.";

/// Installs the OS service.
pub const SUPERVISOR_INSTALL: &str = "Start sessions at login";

/// Removes it.
pub const SUPERVISOR_REMOVE: &str = "Stop doing that";

/// Title of the removal confirm.
pub const SUPERVISOR_REMOVE_TITLE: &str = "Remove the supervisor?";

/// Says what stops happening.
pub const SUPERVISOR_REMOVE_BODY: &str = "Your sessions will no longer start on their own after a reboot — you will have to open this \
     window and start them by hand. Nothing syncing right now is interrupted.";

// ─── paging a large index (P36) ──────────────────────────────────────────────

/// The previous page of a file query.
pub const PAGE_PREV: &str = "Previous";

/// The next page of a file query.
pub const PAGE_NEXT: &str = "Next";

/// Says which slice of the whole index is on screen. The daemon caps what it
/// will send in one answer, so the window must be honest that it is showing a
/// page and not the folder.
pub fn files_page(offset: usize, shown: usize, matched: usize) -> String {
    if matched == 0 {
        return "no file matches".into();
    }
    // An offset past the end returns nothing; describing it as a range would
    // print a backwards one.
    if shown == 0 {
        return format!("{matched} matching — past the last page");
    }
    if shown >= matched {
        return format!("{matched} matching");
    }
    let last = (offset + shown).min(matched);
    format!("{}–{} of {} matching", offset + 1, last, matched)
}

#[cfg(test)]
mod paging_tests {
    use super::*;

    #[test]
    fn a_full_result_set_is_not_described_as_a_page() {
        assert_eq!(files_page(0, 12, 12), "12 matching");
    }

    #[test]
    fn a_partial_page_names_its_slice_one_based() {
        assert_eq!(files_page(0, 200, 4812), "1–200 of 4812 matching");
        assert_eq!(files_page(200, 200, 4812), "201–400 of 4812 matching");
    }

    /// The last page is short; its upper bound must be the total, not the
    /// arithmetic end of a full page.
    #[test]
    fn the_last_page_stops_at_the_total() {
        assert_eq!(files_page(4800, 12, 4812), "4801–4812 of 4812 matching");
    }

    #[test]
    fn no_matches_says_so_plainly() {
        assert_eq!(files_page(0, 0, 0), "no file matches");
    }

    /// An offset past the end must not produce a backwards range.
    #[test]
    fn an_overshot_offset_does_not_invert_the_range() {
        let s = files_page(500, 0, 10);
        assert!(!s.contains("501–"), "{s}");
    }
}

/// A folder heading's share of the session, without repeating the byte figure
/// the register's own bytes column already carries.
pub fn group_share(files: usize, share: f32) -> String {
    let pct = if share.is_finite() {
        (share * 100.0).round().clamp(0.0, 100.0) as u32
    } else {
        0
    };
    format!("{} · {pct}%", plural(files, "file", "files"))
}

#[cfg(test)]
mod group_share_tests {
    use super::*;

    #[test]
    fn a_share_is_a_whole_percent_beside_a_file_count() {
        assert_eq!(group_share(1, 0.5), "1 file · 50%");
        assert_eq!(group_share(3, 0.41), "3 files · 41%");
    }

    /// "copy"/"copies" is not the only irregular case this file has to survive,
    /// but a file count is the regular one and must stay regular.
    #[test]
    fn zero_files_is_plural() {
        assert_eq!(group_share(0, 0.0), "0 files · 0%");
    }

    #[test]
    fn the_percentage_rounds_half_away_from_zero() {
        assert_eq!(group_share(2, 0.414), "2 files · 41%");
        assert_eq!(group_share(2, 0.416), "2 files · 42%");
        assert_eq!(group_share(2, 0.125), "2 files · 13%");
        assert_eq!(group_share(2, 0.375), "2 files · 38%");
    }

    /// A folder holding a sliver of the session still reads as present, not as
    /// absent — but a genuinely empty one reads as zero.
    #[test]
    fn a_small_share_still_rounds_to_something_honest() {
        assert_eq!(group_share(2, 0.006), "2 files · 1%");
        assert_eq!(group_share(2, 0.004), "2 files · 0%");
    }

    #[test]
    fn a_share_is_clamped_to_its_range() {
        assert_eq!(group_share(2, 1.0), "2 files · 100%");
        assert_eq!(group_share(2, 3.7), "2 files · 100%");
        assert_eq!(group_share(2, -0.5), "2 files · 0%");
    }

    /// A zero-byte session divides by zero somewhere upstream; the caption must
    /// still be a caption rather than "NaN%".
    #[test]
    fn a_non_finite_share_reads_as_zero() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(group_share(1, bad), "1 file · 0%");
        }
    }
}
