//! `tazamun setup` — the interactive configuration panel — plus the
//! three-question first-run wizard offered by `tazamun init` on a terminal.
//!
//! Invariant: this module never parses a config value itself. Every edit goes
//! through [`SessionConfig::set_value`] — the same single parser used by
//! `tazamun config set` — so the panel, the wizard, and the CLI cannot drift.
//! The model (items, presets, filtering, pending-diff) is pure and
//! unit-tested; only the thin terminal loop touches I/O.

use std::io::Write as _;
use std::path::Path;

use console::{Key, Term, style};

use crate::state::{AppState, SessionConfig};

// ─── the model (pure) ───────────────────────────────────────────────────────

/// One editable setting in the panel.
pub struct Item {
    /// The config key, exactly as `tazamun config set` spells it.
    pub key: &'static str,
    /// Display group; items are rendered in `ITEMS` order under group headers.
    pub group: &'static str,
    /// `Some(values)` cycle in place on Enter; `None` opens a typed prompt.
    pub choices: Option<&'static [&'static str]>,
    /// One line shown for the selected item.
    pub help: &'static str,
}

/// Every panel setting, grouped. Order is display order.
pub const ITEMS: &[Item] = &[
    Item {
        key: "role",
        group: "Role & editing",
        choices: Some(&["editor", "viewer", "archive"]),
        help: "What this folder may do: editor locks and publishes; viewer only syncs and reads; archive is receive-only with deep history.",
    },
    Item {
        key: "strict",
        group: "Role & editing",
        choices: Some(&["on", "off"]),
        help: "on = exclusive checkout (files read-only, lock to edit). off = easy mode (files writable, edits auto-publish).",
    },
    Item {
        key: "autolock",
        group: "Role & editing",
        choices: Some(&["on", "off"]),
        help: "In strict mode, auto-take the lease on your first write to a free path instead of refusing the edit.",
    },
    Item {
        key: "relay",
        group: "Network",
        choices: None,
        help: "Relay policy: 'default' (public relays), 'none', or your own https:// relay URL.",
    },
    Item {
        key: "lan",
        group: "Network",
        choices: Some(&["on", "off"]),
        help: "Find same-LAN members over mDNS with no external lookup.",
    },
    Item {
        key: "airgap",
        group: "Network",
        choices: Some(&["on", "off"]),
        help: "Closed network: no relays, no external discovery of any kind — LAN only.",
    },
    Item {
        key: "junk-filter",
        group: "Sync scope",
        choices: Some(&["on", "off"]),
        help: "Hold editor swap/backup files and OS metadata (.DS_Store, Thumbs.db, *.swp, Zone.Identifier) out of the sync.",
    },
    Item {
        key: "sync-only",
        group: "Sync scope",
        choices: None,
        help: "Carry ONLY this subtree on this node (e.g. docs); empty carries everything. Held paths are listed, never deleted.",
    },
    Item {
        key: "sync-skip",
        group: "Sync scope",
        choices: None,
        help: "Comma-separated subtrees this node does not carry (e.g. renders,tmp).",
    },
    Item {
        key: "max-file-size",
        group: "Sync scope",
        choices: None,
        help: "Hold new files larger than this (0 = unlimited; e.g. 500KB, 100MB, 2GB). Already-synced files are unaffected.",
    },
    Item {
        key: "history-depth",
        group: "History",
        choices: None,
        help: "Versions kept per file (0/auto = role default: 5, or deeper for archive). Pinned versions are always kept.",
    },
    Item {
        key: "max-down",
        group: "Transfer",
        choices: None,
        help: "Cap the download rate for incoming file chunks (0 = unlimited; e.g. 2MB, 500KB). Applies live, aggregate across all pulls.",
    },
    Item {
        key: "audit",
        group: "Observability",
        choices: Some(&["on", "off"]),
        help: "Write the append-only audit log (.tazamun/audit.jsonl), readable with `tazamun log`. Capped; cheap.",
    },
    Item {
        key: "hooks",
        group: "Observability",
        choices: Some(&["on", "off"]),
        help: "Run executables under .tazamun/hooks/ on events (on-sync/on-conflict/on-lock-denied/on-peer-offline). No cost unless a hook exists.",
    },
    Item {
        key: "notify",
        group: "Observability",
        choices: Some(&["on", "off"]),
        help: "Desktop notifications for events worth a human (conflict preserved, peer offline mid-lease). Off by default.",
    },
    Item {
        key: "lease-ttl",
        group: "Leases",
        choices: None,
        help: "How long a lease you take lasts before it must renew (10s-24h, e.g. 90s, 15m).",
    },
    Item {
        key: "acquire-timeout",
        group: "Leases",
        choices: None,
        help: "How long a lock request waits for every peer to answer (2s-60s).",
    },
    Item {
        key: "wait-timeout",
        group: "Leases",
        choices: None,
        help: "How long `lock --wait` keeps waiting for a busy path (e.g. 10m).",
    },
    Item {
        key: "dashboard-port",
        group: "Interface",
        choices: None,
        help: "Loopback port for the web dashboard (1024-65535).",
    },
    Item {
        key: "update-channel",
        group: "Interface",
        choices: Some(&["stable", "beta"]),
        help: "Which releases `tazamun update` follows: stable skips prereleases; beta takes the newest.",
    },
];

/// Renders the current value of `key` for display. Panics on an unknown key —
/// `ITEMS` and this function are maintained together (tested below).
pub fn get_value(c: &SessionConfig, key: &str) -> String {
    fn on_off(b: bool) -> String {
        (if b { "on" } else { "off" }).to_string()
    }
    fn dur(ms: u64) -> String {
        humantime::format_duration(std::time::Duration::from_millis(ms)).to_string()
    }
    match key {
        "role" => c.role.as_str().to_string(),
        "strict" => on_off(c.strict),
        "autolock" => on_off(c.autolock),
        "relay" => c.relay.clone(),
        "lan" => on_off(c.lan),
        "airgap" => on_off(c.airgap),
        "lease-ttl" => dur(c.lease_ttl_ms),
        "acquire-timeout" => dur(c.acquire_timeout_ms),
        "wait-timeout" => dur(c.wait_timeout_ms),
        "dashboard-port" => c.dashboard_port.to_string(),
        "update-channel" => c.update_channel.clone(),
        "junk-filter" => on_off(c.junk_filter),
        "audit" => on_off(c.audit),
        "hooks" => on_off(c.hooks),
        "notify" => on_off(c.notify),
        "sync-only" => {
            if c.sync_only.is_empty() {
                "(everything)".to_string()
            } else {
                c.sync_only.clone()
            }
        }
        "sync-skip" => {
            if c.sync_skip.is_empty() {
                "(none)".to_string()
            } else {
                c.sync_skip.clone()
            }
        }
        "max-file-size" => crate::state::fmt_size(c.max_file_size),
        "history-depth" => {
            if c.history_depth == 0 {
                "auto".to_string()
            } else {
                c.history_depth.to_string()
            }
        }
        "max-down" => {
            if c.max_down == 0 {
                "unlimited".to_string()
            } else {
                format!("{}/s", crate::state::fmt_size(c.max_down))
            }
        }
        other => unreachable!("get_value: unknown panel key {other}"),
    }
}

/// A named bundle of settings applied in one keystroke.
pub struct Preset {
    pub name: &'static str,
    pub blurb: &'static str,
    pub sets: &'static [(&'static str, &'static str)],
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "team-strict",
        blurb: "Full exclusive checkout for a team: lock deliberately, never clobber.",
        sets: &[("role", "editor"), ("strict", "on"), ("autolock", "off")],
    },
    Preset {
        name: "solo-easy",
        blurb: "Edit in place and let changes publish themselves; quarantine still guards.",
        sets: &[("role", "editor"), ("strict", "off"), ("autolock", "on")],
    },
    Preset {
        name: "viewer-kiosk",
        blurb: "This folder only receives and displays; it can never lock or publish.",
        sets: &[("role", "viewer"), ("strict", "on"), ("autolock", "off")],
    },
    Preset {
        name: "lan-only",
        blurb: "No relays: members meet over local mDNS only (role unchanged).",
        sets: &[("relay", "none"), ("lan", "on"), ("airgap", "off")],
    },
];

/// The working state of one panel session: the config as saved on disk and
/// the edited copy, plus cursor/filter/status. Pure — the terminal loop drives
/// it and renders it.
pub struct Panel {
    pub saved: SessionConfig,
    pub work: SessionConfig,
    pub cursor: usize,
    pub filter: String,
    pub status: String,
}

impl Panel {
    pub fn new(saved: SessionConfig) -> Self {
        Self {
            work: saved.clone(),
            saved,
            cursor: 0,
            filter: String::new(),
            status: String::new(),
        }
    }

    /// Indices into `ITEMS` that match the current filter (key, group, or
    /// help substring, case-insensitive). Empty filter = everything.
    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_ascii_lowercase();
        ITEMS
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                f.is_empty()
                    || it.key.contains(&f)
                    || it.group.to_ascii_lowercase().contains(&f)
                    || it.help.to_ascii_lowercase().contains(&f)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The preview: every key whose edited value differs from the saved one,
    /// as `(key, saved, edited)`.
    pub fn pending(&self) -> Vec<(&'static str, String, String)> {
        ITEMS
            .iter()
            .filter_map(|it| {
                let from = get_value(&self.saved, it.key);
                let to = get_value(&self.work, it.key);
                (from != to).then_some((it.key, from, to))
            })
            .collect()
    }

    /// Applies one edit through the shared parser; errors land in `status`
    /// and leave the working copy untouched.
    pub fn set(&mut self, key: &str, value: &str) {
        match self.work.set_value(key, value) {
            Ok(note) => self.status = note,
            Err(e) => self.status = format!("✗ {e}"),
        }
    }

    /// For a choice item: step to the next (or previous) allowed value.
    pub fn cycle(&mut self, item: usize, forward: bool) {
        let it = &ITEMS[item];
        let Some(choices) = it.choices else { return };
        let current = get_value(&self.work, it.key);
        let pos = choices.iter().position(|c| **c == current).unwrap_or(0);
        let next = if forward {
            (pos + 1) % choices.len()
        } else {
            (pos + choices.len() - 1) % choices.len()
        };
        self.set(it.key, choices[next]);
    }

    /// Applies a preset bundle to the working copy (still previewed, not saved).
    pub fn apply_preset(&mut self, preset: &Preset) {
        for (k, v) in preset.sets {
            // Preset values are static and valid by construction; a failure
            // here is a programming error and the status line will show it.
            self.set(k, v);
        }
        self.status = format!(
            "preset {} applied — review and press s to save",
            preset.name
        );
    }
}

// ─── the terminal loop (thin I/O) ────────────────────────────────────────────

/// Runs the full-screen panel for the session at `dir`. Blocking; call from
/// `spawn_blocking`. On save it persists `state.json` and returns the changed
/// `(key, value)` pairs so the async caller can live-apply what it can over
/// IPC. `Ok(None)` = quit without saving.
pub fn run_panel(dir: &Path) -> Result<Option<Vec<(String, String)>>, String> {
    let state = AppState::load(dir).map_err(|e| e.to_string())?;
    let term = Term::stdout();
    if !term.is_term() {
        return Err(
            "setup needs an interactive terminal; use `tazamun config set <key> <value>` instead"
                .to_string(),
        );
    }
    let mut panel = Panel::new(state.config.clone());
    let _ = term.hide_cursor();
    let result = panel_loop(&term, &mut panel, dir);
    let _ = term.show_cursor();
    let _ = term.clear_screen();
    result
}

fn panel_loop(
    term: &Term,
    panel: &mut Panel,
    dir: &Path,
) -> Result<Option<Vec<(String, String)>>, String> {
    let mut filtering = false;
    loop {
        render(term, panel, filtering);
        let key = term.read_key().map_err(|e| e.to_string())?;
        if filtering {
            match key {
                Key::Char(c) => panel.filter.push(c),
                Key::Backspace => {
                    panel.filter.pop();
                }
                Key::Escape => {
                    panel.filter.clear();
                    filtering = false;
                }
                Key::Enter => filtering = false,
                _ => {}
            }
            panel.cursor = 0;
            continue;
        }
        let visible = panel.visible();
        match key {
            Key::ArrowUp | Key::Char('k') => {
                panel.cursor = panel.cursor.saturating_sub(1);
            }
            Key::ArrowDown | Key::Char('j') => {
                if panel.cursor + 1 < visible.len() {
                    panel.cursor += 1;
                }
            }
            Key::ArrowRight | Key::Enter => {
                if let Some(&i) = visible.get(panel.cursor) {
                    if ITEMS[i].choices.is_some() {
                        panel.cycle(i, true);
                    } else {
                        prompt_edit(term, panel, i)?;
                    }
                }
            }
            Key::ArrowLeft => {
                if let Some(&i) = visible.get(panel.cursor)
                    && ITEMS[i].choices.is_some()
                {
                    panel.cycle(i, false);
                }
            }
            Key::Char('/') => {
                filtering = true;
                panel.filter.clear();
                panel.cursor = 0;
            }
            Key::Char('p') => {
                if let Some(preset) = pick_preset(term)? {
                    panel.apply_preset(preset);
                }
            }
            Key::Char('s') => {
                let pending = panel.pending();
                if pending.is_empty() {
                    panel.status = "nothing to save".to_string();
                    continue;
                }
                let mut state = AppState::load(dir).map_err(|e| e.to_string())?;
                state.config = panel.work.clone();
                state.save(dir).map_err(|e| e.to_string())?;
                return Ok(Some(
                    pending
                        .into_iter()
                        .map(|(k, _, to)| (k.to_string(), to))
                        .collect(),
                ));
            }
            Key::Char('q') | Key::Escape => {
                if panel.pending().is_empty() {
                    return Ok(None);
                }
                panel.status =
                    "unsaved changes — press s to save them or Q (shift) to discard".to_string();
            }
            Key::Char('Q') => return Ok(None),
            _ => {}
        }
    }
}

/// Inline typed edit for free-form items (durations, port, relay URL).
fn prompt_edit(term: &Term, panel: &mut Panel, item: usize) -> Result<(), String> {
    let it = &ITEMS[item];
    let _ = term.show_cursor();
    let mut out = term.clone();
    let _ = write!(
        out,
        "\n  {} {} = ",
        style("new value:").bold(),
        style(it.key).cyan()
    );
    let line = term.read_line().map_err(|e| e.to_string())?;
    let _ = term.hide_cursor();
    let value = line.trim();
    if !value.is_empty() {
        panel.set(it.key, value);
    }
    Ok(())
}

/// The preset picker overlay: a small select list, Esc to cancel.
fn pick_preset(term: &Term) -> Result<Option<&'static Preset>, String> {
    let mut cursor = 0usize;
    loop {
        let _ = term.clear_screen();
        let mut out = term.clone();
        let _ = writeln!(out, "{}\n", style("Apply a preset").bold());
        for (i, p) in PRESETS.iter().enumerate() {
            let marker = if i == cursor { ">" } else { " " };
            let name = if i == cursor {
                style(p.name).reverse()
            } else {
                style(p.name).cyan()
            };
            let _ = writeln!(out, " {marker} {name:<14} {}", style(p.blurb).dim());
        }
        let _ = writeln!(
            out,
            "\n {}",
            style("↑↓ move · Enter apply · Esc cancel").dim()
        );
        match term.read_key().map_err(|e| e.to_string())? {
            Key::ArrowUp | Key::Char('k') => cursor = cursor.saturating_sub(1),
            Key::ArrowDown | Key::Char('j') => {
                if cursor + 1 < PRESETS.len() {
                    cursor += 1;
                }
            }
            Key::Enter => return Ok(Some(&PRESETS[cursor])),
            Key::Escape | Key::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

fn render(term: &Term, panel: &Panel, filtering: bool) {
    let _ = term.clear_screen();
    let mut out = term.clone();
    let _ = writeln!(
        out,
        "{}  {}\n",
        style(" tazamun setup ").bold().reverse(),
        style("per-folder policy — changes preview until saved").dim()
    );
    let visible = panel.visible();
    let mut last_group = "";
    for (row, &i) in visible.iter().enumerate() {
        let it = &ITEMS[i];
        if it.group != last_group {
            let _ = writeln!(out, " {}", style(it.group).bold().underlined());
            last_group = it.group;
        }
        let saved = get_value(&panel.saved, it.key);
        let value = get_value(&panel.work, it.key);
        let changed = saved != value;
        let marker = if row == panel.cursor { ">" } else { " " };
        let shown = if changed {
            format!("{saved} → {value} *")
        } else {
            value
        };
        let line = format!("  {marker} {:<16} {shown}", it.key);
        let _ = if row == panel.cursor {
            writeln!(out, "{}", style(line).reverse())
        } else if changed {
            writeln!(out, "{}", style(line).yellow())
        } else {
            writeln!(out, "{line}")
        };
    }
    if visible.is_empty() {
        let _ = writeln!(out, "  (no setting matches {:?})", panel.filter);
    }
    // Selected item's help, the pending count, and the key legend.
    let _ = writeln!(out);
    if let Some(&i) = visible.get(panel.cursor) {
        let _ = writeln!(out, " {}", style(ITEMS[i].help).dim());
    }
    let pending = panel.pending();
    if !pending.is_empty() {
        let _ = writeln!(
            out,
            " {}",
            style(format!("{} pending change(s) — s saves", pending.len())).yellow()
        );
    }
    if !panel.status.is_empty() {
        let _ = writeln!(out, " {}", panel.status);
    }
    let legend = if filtering {
        format!("filter: {}▌  (Enter keep · Esc clear)", panel.filter)
    } else {
        "↑↓ move · Enter/←→ change · / search · p presets · s save · q quit".to_string()
    };
    let _ = writeln!(out, "\n {}", style(legend).dim());
}

// ─── the first-run wizard ────────────────────────────────────────────────────

/// The three-question short path offered by `tazamun init` on a terminal:
/// role → editing → network. Every answer routes through the same
/// `set_value` parser; Enter accepts the default on each question, so
/// Enter-Enter-Enter is exactly the pre-P10 behavior. Returns the notes of
/// what was set (empty when the user aborted with Esc — defaults stand).
pub fn run_init_wizard(dir: &Path) -> Result<Vec<String>, String> {
    let term = Term::stdout();
    if !term.is_term() {
        return Ok(Vec::new());
    }
    // One wizard option: (name, blurb, key/value pairs it applies).
    type WizardOption = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
    );
    let questions: &[(&str, &[WizardOption])] = &[
        (
            "What is this folder's role?",
            &[
                (
                    "editor",
                    "lock, edit, publish (default)",
                    &[("role", "editor")],
                ),
                (
                    "viewer",
                    "sync + read only — never locks",
                    &[("role", "viewer")],
                ),
                (
                    "archive",
                    "receive-only, deep history",
                    &[("role", "archive")],
                ),
            ],
        ),
        (
            "How should editing work?",
            &[
                (
                    "strict",
                    "files read-only; lock → edit → unlock (default)",
                    &[("strict", "on")],
                ),
                (
                    "easy",
                    "files writable; edits auto-publish",
                    &[("strict", "off"), ("autolock", "on")],
                ),
            ],
        ),
        (
            "How should peers meet?",
            &[
                (
                    "internet",
                    "direct + public relay fallback (default)",
                    &[("relay", "default"), ("lan", "on"), ("airgap", "off")],
                ),
                (
                    "lan-only",
                    "no relays; local mDNS only",
                    &[("relay", "none"), ("lan", "on"), ("airgap", "off")],
                ),
                (
                    "airgap",
                    "closed network: nothing external at all",
                    &[("airgap", "on")],
                ),
            ],
        ),
    ];

    let mut state = AppState::load(dir).map_err(|e| e.to_string())?;
    let mut notes = Vec::new();
    println!("\nQuick setup — Enter keeps the default, Esc skips the rest:\n");
    for (title, options) in questions {
        let labels: Vec<(String, String)> = options
            .iter()
            .map(|(name, blurb, _)| (name.to_string(), blurb.to_string()))
            .collect();
        match select(&term, title, &labels).map_err(|e| e.to_string())? {
            None => break, // Esc: keep defaults for the remaining questions
            Some(choice) => {
                for (k, v) in options[choice].2 {
                    let note = state.config.set_value(k, v)?;
                    notes.push(note);
                }
            }
        }
    }
    if !notes.is_empty() {
        state.save(dir).map_err(|e| e.to_string())?;
        for n in &notes {
            println!("  ✔ {n}");
        }
        println!("  (change any of this later with `tazamun setup`)\n");
    }
    Ok(notes)
}

/// A minimal arrow-key select: returns the chosen index, or `None` on Esc.
fn select(
    term: &Term,
    title: &str,
    options: &[(String, String)],
) -> std::io::Result<Option<usize>> {
    let mut cursor = 0usize;
    let mut out = term.clone();
    writeln!(out, " {}", style(title).bold())?;
    let rows = options.len();
    // First draw, then redraw in place on every keypress.
    loop {
        for (i, (name, blurb)) in options.iter().enumerate() {
            let marker = if i == cursor { ">" } else { " " };
            let line = format!("  {marker} {name:<10} {blurb}");
            if i == cursor {
                writeln!(out, "{}", style(line).reverse())?;
            } else {
                writeln!(out, "{line}")?;
            }
        }
        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') => cursor = cursor.saturating_sub(1),
            Key::ArrowDown | Key::Char('j') => {
                if cursor + 1 < rows {
                    cursor += 1;
                }
            }
            Key::Enter => {
                writeln!(out)?;
                return Ok(Some(cursor));
            }
            Key::Escape | Key::Char('q') => {
                writeln!(out)?;
                return Ok(None);
            }
            _ => {}
        }
        term.clear_last_lines(rows)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::NodeRole;

    #[test]
    fn every_item_key_renders_and_round_trips_through_set_value() {
        // ITEMS and get_value are maintained together: every key must render
        // from a default config, and every choice value must be accepted by
        // the shared parser. This is the no-drift guarantee, executable.
        let mut c = SessionConfig::default();
        for it in ITEMS {
            let _ = get_value(&c, it.key); // must not panic
            if let Some(choices) = it.choices {
                for v in choices {
                    c.set_value(it.key, v)
                        .unwrap_or_else(|e| panic!("choice {v:?} for {} rejected: {e}", it.key));
                    assert_eq!(&get_value(&c, it.key), v, "render mismatch for {}", it.key);
                }
            }
        }
    }

    #[test]
    fn presets_apply_cleanly_and_preview_as_pending() {
        for preset in PRESETS {
            let mut panel = Panel::new(SessionConfig::default());
            panel.apply_preset(preset);
            assert!(
                !panel.status.starts_with('✗'),
                "preset {} errored: {}",
                preset.name,
                panel.status
            );
            // The saved copy is untouched until an explicit save.
            assert_eq!(panel.saved, SessionConfig::default());
        }
        // viewer-kiosk really flips the role in the working copy.
        let mut panel = Panel::new(SessionConfig::default());
        panel.apply_preset(&PRESETS[2]);
        assert_eq!(panel.work.role, NodeRole::Viewer);
        assert!(
            panel
                .pending()
                .iter()
                .any(|(k, _, to)| *k == "role" && to == "viewer")
        );
    }

    #[test]
    fn cycle_steps_through_choices_and_wraps() {
        let mut panel = Panel::new(SessionConfig::default());
        let role_idx = ITEMS.iter().position(|i| i.key == "role").unwrap();
        assert_eq!(get_value(&panel.work, "role"), "editor");
        panel.cycle(role_idx, true);
        assert_eq!(get_value(&panel.work, "role"), "viewer");
        panel.cycle(role_idx, true);
        assert_eq!(get_value(&panel.work, "role"), "archive");
        panel.cycle(role_idx, true);
        assert_eq!(get_value(&panel.work, "role"), "editor", "wraps around");
        panel.cycle(role_idx, false);
        assert_eq!(get_value(&panel.work, "role"), "archive", "backwards wraps");
    }

    #[test]
    fn filter_narrows_and_bad_values_do_not_mutate() {
        let mut panel = Panel::new(SessionConfig::default());
        // "relay" matches the relay item itself AND items whose help mentions
        // relays (airgap) — searching help text is the point. Every hit must
        // actually mention it somewhere.
        panel.filter = "relay".to_string();
        let vis = panel.visible();
        assert!(vis.iter().any(|&i| ITEMS[i].key == "relay"));
        assert!(vis.iter().all(|&i| {
            let it = &ITEMS[i];
            it.key.contains("relay") || it.help.to_ascii_lowercase().contains("relay")
        }));
        // An exact key is a unique hit.
        panel.filter = "wait-timeout".to_string();
        let vis = panel.visible();
        assert_eq!(vis.len(), 1);
        assert_eq!(ITEMS[vis[0]].key, "wait-timeout");
        // A rejected value reports in status and leaves the value untouched.
        panel.set("dashboard-port", "80");
        assert!(panel.status.starts_with('✗'));
        assert_eq!(get_value(&panel.work, "dashboard-port"), "8787");
        assert!(panel.pending().is_empty());
    }
}
