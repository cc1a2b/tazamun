//! The Home screen and the device session manager (P13).
//!
//! Bare `tazamun` (no subcommand) opens [`show_home`]: a time-of-day greeting
//! by the OS user, then a live overview of every registered session.
//! `tazamun sessions` opens [`run_manager`]: an interactive list of every
//! session on the device with per-item actions (copy invite, open dashboard,
//! per-folder setup, remove). Everything works on Linux, macOS, and Windows;
//! the pure bits (greeting, display name) are unit-tested and the interactive
//! loop is the only part that touches a terminal.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{Local, Timelike};
use console::{Key, Term, style};

use crate::ipc;
use crate::registry::{Registry, SessionKind, SessionRef};
use crate::session::{AddrWire, SessionSecret, Ticket};
use crate::state::{AppState, decode_hex32};

// ─── greeting (pure) ─────────────────────────────────────────────────────────

/// The time-of-day greeting for a 24-hour `hour` and a display `name`.
pub fn greeting(hour: u32, name: &str) -> String {
    let part = match hour {
        5..=11 => "Good morning",
        12..=16 => "Good afternoon",
        17..=21 => "Good evening",
        _ => "Good night",
    };
    format!("{part}, {name}")
}

/// The OS user's display name: `$USER` / `$USERNAME` / `$LOGNAME`, first letter
/// upper-cased, or a friendly fallback. Cross-platform (Windows sets
/// `USERNAME`, Unix sets `USER`).
pub fn display_name() -> String {
    let raw = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "there".to_string());
    capitalize(raw.trim())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The current greeting from the local wall clock.
fn now_greeting() -> String {
    greeting(Local::now().hour(), &display_name())
}

// ─── session info (async) ────────────────────────────────────────────────────

/// A session as shown in Home / the manager: registry path plus live details
/// read from its own `state.json` and a daemon liveness ping.
pub struct SessionInfo {
    pub path: PathBuf,
    pub kind: SessionKind,
    pub running: bool,
    pub files: usize,
    pub id_short: String,
    /// `None` when the folder no longer has a readable `state.json`.
    pub readable: bool,
}

/// Loads live info for every registered session (pinging each daemon).
async fn gather(refs: &[SessionRef]) -> Vec<SessionInfo> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let path = PathBuf::from(&r.path);
        let (files, id_short, readable) = match AppState::load(&path) {
            Ok(st) => (
                st.files.values().filter(|f| !f.deleted).count(),
                // The node's PUBLIC id short form — never a prefix of the private
                // `iroh_secret_key` (that field is the node's secret key).
                st.node_id_short().unwrap_or_else(|| "?".to_string()),
                true,
            ),
            Err(_) => (0, "-".to_string(), false),
        };
        let running = ipc::daemon_alive(&path).await;
        out.push(SessionInfo {
            path,
            kind: r.kind,
            running,
            files,
            id_short,
            readable,
        });
    }
    out
}

// ─── Home screen ─────────────────────────────────────────────────────────────

/// Prints the Home screen: greeting + a live overview of every session. Plain
/// and non-interactive, so it is identical over SSH, a pipe, or any OS.
pub async fn show_home() -> Result<(), String> {
    let mut reg = Registry::load();
    let pruned = reg.prune(AppState::exists);
    if !pruned.is_empty() {
        let _ = reg.save();
    }
    let infos = gather(&reg.sessions).await;

    println!("\n  {}", style(now_greeting()).bold().cyan());
    println!(
        "  {}\n",
        style(format!("tazamun {}", env!("TAZAMUN_VERSION"))).dim()
    );

    if infos.is_empty() {
        println!("  No sessions yet.\n");
        println!("  {}  create or join one:", style("get started").bold());
        println!("    tazamun init              # share a folder");
        println!("    tazamun join tzm1…        # join an invite");
        println!("    tazamun send <path>       # one-shot transfer, no session\n");
        return Ok(());
    }

    let running = infos.iter().filter(|i| i.running).count();
    println!(
        "  {} session(s) on this device · {} running",
        infos.len(),
        running
    );
    for i in &infos {
        let dot = if i.running {
            style("●").green()
        } else if i.readable {
            style("○").dim()
        } else {
            style("⚠").red()
        };
        let state = if !i.readable {
            "unreadable".to_string()
        } else if i.running {
            format!("running · {} files", i.files)
        } else {
            format!("stopped · {} files", i.files)
        };
        println!("    {dot} {:<40} {}", i.path.display(), style(state).dim());
    }
    println!(
        "\n  {} manage them:  {}",
        style("→").cyan(),
        style("tazamun sessions").bold()
    );
    println!(
        "  {} in a session:  tazamun --dir <path> status | setup | dashboard\n",
        style("→").cyan()
    );
    Ok(())
}

// ─── session manager (interactive) ───────────────────────────────────────────

/// Runs the interactive device session manager. Blocking terminal work runs
/// off the async runtime; re-gathers live state after each round so the list
/// stays fresh.
pub async fn run_manager() -> Result<(), String> {
    let term = Term::stdout();
    if !term.is_term() {
        // Non-interactive: print the same overview `show_home` would, then exit.
        return show_home().await;
    }
    loop {
        let mut reg = Registry::load();
        reg.prune(AppState::exists);
        let _ = reg.save();
        let infos = gather(&reg.sessions).await;
        let term2 = term.clone();
        let again = tokio::task::spawn_blocking(move || manager_loop(&term2, infos))
            .await
            .map_err(|e| format!("manager task failed: {e}"))??;
        if !again {
            let _ = term.show_cursor();
            let _ = term.clear_screen();
            return Ok(());
        }
    }
}

/// The blocking manager loop over a snapshot of `infos`. Returns `true` to
/// re-gather (state changed / refresh) or `false` to quit. Removal and
/// invite-copy are pure-sync and handled in place; live actions (dashboard)
/// print the command to run.
fn manager_loop(term: &Term, mut infos: Vec<SessionInfo>) -> Result<bool, String> {
    let _ = term.hide_cursor();
    let mut cursor = 0usize;
    let mut status = String::new();
    loop {
        render_manager(term, &infos, cursor, &status);
        let key = term.read_key().map_err(|e| e.to_string())?;
        match key {
            Key::ArrowUp | Key::Char('k') => cursor = cursor.saturating_sub(1),
            Key::ArrowDown | Key::Char('j') => {
                if cursor + 1 < infos.len() {
                    cursor += 1;
                }
            }
            Key::Char('r') => return Ok(true),
            Key::Char('q') | Key::Escape => return Ok(false),
            Key::Char('i') => {
                if let Some(info) = infos.get(cursor) {
                    status = copy_invite(term, info);
                }
            }
            Key::Char('d') => {
                if let Some(info) = infos.get(cursor) {
                    status = if info.running {
                        format!(
                            "open it with:  tazamun --dir {} dashboard",
                            info.path.display()
                        )
                    } else {
                        "start it first:  tazamun --dir <path> start".to_string()
                    };
                }
            }
            Key::Char('s') => {
                if let Some(info) = infos.get(cursor) {
                    status = format!("edit it with:  tazamun --dir {} setup", info.path.display());
                }
            }
            Key::Char('x') => {
                if let Some(info) = infos.get(cursor) {
                    if let Some(msg) = remove_flow(term, info)? {
                        status = msg;
                        infos.remove(cursor);
                        cursor = cursor.min(infos.len().saturating_sub(1));
                    } else {
                        status = "removal cancelled".to_string();
                    }
                }
            }
            Key::Char('X') => {
                if remove_all_flow(term, &infos)? {
                    return Ok(true);
                }
                status = "remove-all cancelled".to_string();
            }
            _ => {}
        }
        if infos.is_empty() {
            render_manager(term, &infos, 0, "no sessions left — press q to exit");
            // Fall through so the empty state is visible; keep looping for q.
        }
    }
}

fn render_manager(term: &Term, infos: &[SessionInfo], cursor: usize, status: &str) {
    let _ = term.clear_screen();
    let mut out = term.clone();
    let _ = writeln!(
        out,
        "{}  {}\n",
        style(" tazamun sessions ").bold().reverse(),
        style("every session on this device").dim()
    );
    if infos.is_empty() {
        let _ = writeln!(out, "  (no registered sessions)\n");
    }
    for (i, info) in infos.iter().enumerate() {
        let marker = if i == cursor { ">" } else { " " };
        let dot = if info.running {
            style("●").green()
        } else if info.readable {
            style("○").dim()
        } else {
            style("⚠").red()
        };
        let detail = if !info.readable {
            "unreadable".to_string()
        } else {
            format!(
                "{} · {} files · {}",
                if info.running { "running" } else { "stopped" },
                info.files,
                info.kind.as_str()
            )
        };
        let line = format!("  {marker} {dot} {:<38} {}", info.path.display(), detail);
        let _ = if i == cursor {
            writeln!(out, "{}", style(line).reverse())
        } else {
            writeln!(out, "{line}")
        };
    }
    if !status.is_empty() {
        let _ = writeln!(out, "\n  {}", style(status).yellow());
    }
    let _ = writeln!(
        out,
        "\n  {}",
        style("↑↓ move · i invite · d dashboard · s setup · x remove · X remove-all · r refresh · q quit")
            .dim()
    );
}

/// Copies/prints the session's invite: the offline invite reconstructed from
/// `state.json` (secret + peer id + known members), best-effort to the OS
/// clipboard, always printed so it can be copied by hand.
fn copy_invite(term: &Term, info: &SessionInfo) -> String {
    let Some(ticket) = offline_invite(&info.path) else {
        return "could not read this session's invite (state unreadable)".to_string();
    };
    let _ = term.clear_screen();
    let mut out = term.clone();
    let _ = writeln!(out, "{}\n", style("Invite for this session").bold());
    let _ = writeln!(out, "  {ticket}\n");
    let clip = clipboard_copy(&ticket);
    let _ = writeln!(
        out,
        "  {}",
        style(if clip {
            "copied to the clipboard — share it to invite someone"
        } else {
            "select and copy the ticket above to invite someone"
        })
        .dim()
    );
    let _ =
        writeln!(
        out,
        "  {}",
        style("(for a ticket with live addresses, run `tazamun --dir <path> invite` while it runs)")
            .dim()
    );
    let _ = writeln!(out, "\n  press any key to go back");
    let _ = term.read_key();
    if clip {
        "invite copied to the clipboard".to_string()
    } else {
        "invite printed above".to_string()
    }
}

/// Confirms and removes one session: forgets it from the device registry, then
/// on an explicit second confirm deletes only the `.tazamun` metadata — user
/// files are never touched. Returns `Some(message)` on removal, `None` on
/// cancel.
fn remove_flow(term: &Term, info: &SessionInfo) -> Result<Option<String>, String> {
    let _ = term.clear_screen();
    let mut out = term.clone();
    let _ = writeln!(
        out,
        "{}\n",
        style(format!("Remove {} ?", info.path.display())).bold()
    );
    let _ = writeln!(
        out,
        "  This forgets the session from this device's list.\n  Your files are NOT deleted.\n"
    );
    let _ = write!(out, "  remove it? [y/N] ");
    if !yes(term)? {
        return Ok(None);
    }
    crate::registry::forget_session(&info.path);
    // Offer the metadata purge as a distinct, explicit second step.
    let _ = writeln!(
        out,
        "\n  Also delete tazamun's sync metadata (.tazamun: history, blobs, keys)?"
    );
    let _ = writeln!(
        out,
        "  {} — your files stay; only tazamun stops tracking this folder.",
        style("this cannot be undone").red()
    );
    let _ = write!(out, "  delete .tazamun? [y/N] ");
    if yes(term)? {
        let meta = AppState::meta_dir(&info.path);
        match std::fs::remove_dir_all(&meta) {
            Ok(()) => Ok(Some(format!(
                "removed session and its metadata: {}",
                info.path.display()
            ))),
            Err(e) => Ok(Some(format!(
                "forgot the session, but could not delete {}: {e}",
                meta.display()
            ))),
        }
    } else {
        Ok(Some(format!(
            "forgot {} (metadata kept — re-add by running tazamun in it)",
            info.path.display()
        )))
    }
}

/// Confirms and forgets every session (metadata kept — a bulk purge would be
/// too blunt for a destructive op).
fn remove_all_flow(term: &Term, infos: &[SessionInfo]) -> Result<bool, String> {
    if infos.is_empty() {
        return Ok(false);
    }
    let _ = term.clear_screen();
    let mut out = term.clone();
    let _ = writeln!(
        out,
        "{}\n",
        style(format!("Forget all {} session(s)?", infos.len())).bold()
    );
    let _ = writeln!(
        out,
        "  Clears this device's session list. No files and no .tazamun metadata\n  are deleted — every folder can be re-added by running tazamun in it.\n"
    );
    let _ = write!(out, "  forget all? [y/N] ");
    if !yes(term)? {
        return Ok(false);
    }
    let mut reg = Registry::load();
    for info in infos {
        reg.forget(&info.path);
    }
    let _ = reg.save();
    Ok(true)
}

fn yes(term: &Term) -> Result<bool, String> {
    match term.read_key().map_err(|e| e.to_string())? {
        Key::Char('y') | Key::Char('Y') => Ok(true),
        _ => Ok(false),
    }
}

// ─── offline invite + clipboard ──────────────────────────────────────────────

/// Rebuilds a session's invite from its `state.json` alone (no daemon): the
/// shared secret, this node's peer id, and any known members as bootstrap.
/// It lacks live addresses, but it is enough to join over discovery/LAN.
pub fn offline_invite(dir: &Path) -> Option<String> {
    let state = AppState::load(dir).ok()?;
    let secret = decode_hex32(&state.session_secret)?;
    let sk = decode_hex32(&state.iroh_secret_key)?;
    let me = iroh::SecretKey::from_bytes(&sk).public();
    let mut bootstrap = vec![AddrWire {
        id: *me.as_bytes(),
        relay: None,
        direct: vec![],
    }];
    for member in state.known_members.values() {
        bootstrap.push(member.clone());
    }
    // P17: on a v2 session, an offline invite from this (editor) node is a
    // signed editor invite, so the joinee gets a real grant — consistent with a
    // live `tazamun invite`. A legacy session, or a node without the admin
    // secret, falls back to the plain v1 ticket.
    let ticket = match (state.admin_secret_key(), state.admin_public_bytes()) {
        (Some(admin), Some(admin_pub)) => crate::session::mint_ticket(
            secret,
            Some((&admin, admin_pub)),
            crate::session::ROLE_EDITOR,
            rand::random(),
            crate::now_ms(),
            0,
            bootstrap,
        ),
        _ => Ticket::new(SessionSecret(secret), bootstrap),
    };
    Some(ticket.encode())
}

/// Best-effort copy to the OS clipboard by shelling out to the platform tool
/// (`clip` / `pbcopy` / `wl-copy` / `xclip`). No dependency; a missing tool is
/// simply reported as "not copied" and the ticket is printed regardless.
fn clipboard_copy(text: &str) -> bool {
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("clip", &[])];
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[(&str, &[&str])] =
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];

    for (cmd, args) in candidates {
        let child = std::process::Command::new(cmd)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_tracks_the_clock() {
        assert_eq!(greeting(6, "Hussain"), "Good morning, Hussain");
        assert_eq!(greeting(13, "Hussain"), "Good afternoon, Hussain");
        assert_eq!(greeting(19, "Hussain"), "Good evening, Hussain");
        assert_eq!(greeting(23, "Hussain"), "Good night, Hussain");
        assert_eq!(greeting(3, "Hussain"), "Good night, Hussain");
        // Boundaries.
        assert!(greeting(5, "x").starts_with("Good morning"));
        assert!(greeting(11, "x").starts_with("Good morning"));
        assert!(greeting(12, "x").starts_with("Good afternoon"));
        assert!(greeting(17, "x").starts_with("Good evening"));
        assert!(greeting(22, "x").starts_with("Good night"));
    }

    #[test]
    fn display_name_capitalizes_and_falls_back() {
        assert_eq!(capitalize("hussain"), "Hussain");
        assert_eq!(capitalize("cc1a2b"), "Cc1a2b");
        assert_eq!(capitalize(""), "");
        // display_name never panics and is non-empty regardless of environment.
        assert!(!display_name().is_empty());
    }

    #[test]
    fn offline_invite_reconstructs_a_valid_ticket() {
        let dir = tempfile::tempdir().unwrap();
        // A real session so state.json has valid keys.
        crate::cli::init(dir.path()).unwrap();
        let ticket = offline_invite(dir.path()).expect("invite from state");
        assert!(ticket.starts_with("tzm1"));
        // It decodes back to the same session secret.
        let decoded = Ticket::decode(&ticket).unwrap();
        let st = AppState::load(dir.path()).unwrap();
        assert_eq!(
            crate::state::encode_hex32(&decoded.secret.0),
            st.session_secret
        );
    }
}
