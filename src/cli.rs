//! Command-line interface and command handlers.
//!
//! Invariant: mutating commands go through the running daemon's IPC socket;
//! this module never touches synced files directly — the daemon is the only
//! writer, so the strict-mode guarantees cannot be bypassed from the CLI.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand};

use crate::daemon::{DaemonConfig, DaemonError};
use crate::ipc::{self, IpcRequest, IpcResponse};
use crate::locks::LockTimings;
use crate::net::endpoint::{NetConfig, RelayChoice};
use crate::session::{AddrWire, SessionSecret, Ticket};
use crate::state::{AppState, encode_hex32};
use crate::ui::progress::Ui;

#[derive(Debug, Parser)]
#[command(
    name = "tazamun",
    version,
    about = "تزامُن — strict-checkout P2P folder sync. No server ever reads your files."
)]
pub struct Cli {
    /// Session folder (defaults to the current directory).
    #[arg(long, global = true, default_value = ".")]
    pub dir: PathBuf,
    /// Verbose logging (-v: debug).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Initialize this folder as a new sync session and print an invite.
    Init,
    /// Join an existing session from an invite ticket.
    Join { ticket: String },
    /// Run the sync daemon in the foreground.
    Start {
        /// Use a self-hosted relay instead of the public ones.
        #[arg(long, conflicts_with = "no_relay")]
        relay: Option<String>,
        /// Disable relays entirely (LAN / manually routed setups).
        #[arg(long)]
        no_relay: bool,
        /// Enable local mDNS discovery.
        #[arg(long)]
        lan: bool,
    },
    /// Show members, connection health, leases and transfers.
    Status {
        /// Live-refreshing panel (1s); press q or Ctrl-C to exit.
        #[arg(long, conflicts_with = "json")]
        watch: bool,
        /// Emit the full telemetry snapshot as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print a fresh invite ticket for this session.
    Invite {
        /// Also render the ticket as a scannable QR code.
        #[arg(long)]
        qr: bool,
    },
    /// One-shot NAT & environment health report.
    Doctor {
        /// Emit the report as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Acquire an exclusive lease and make the file writable.
    Lock { path: String },
    /// Publish pending edits and release the lease.
    Unlock { path: String },
    /// List kept historical versions of a path.
    Versions { path: String },
    /// Restore version N of a path (requires a held lease).
    Restore { path: String, n: usize },
    /// Delete unreferenced blobs from the local store.
    Gc,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    #[error(transparent)]
    Ipc(#[from] ipc::IpcError),
    #[error(transparent)]
    Daemon(#[from] DaemonError),
    #[error("invalid ticket: {0}")]
    Ticket(#[from] crate::session::TicketError),
    #[error("{0}")]
    Refused(String),
    #[error("daemon error [{code}]: {message}")]
    DaemonRefused { code: String, message: String },
}

/// Runs one parsed command to completion.
pub async fn run(cli: Cli, ui: Ui) -> Result<(), CliError> {
    let dir = std::path::absolute(&cli.dir).map_err(crate::state::StateError::Io)?;
    let verbose = cli.verbose > 0;
    match cli.cmd {
        Cmd::Init => init(&dir),
        Cmd::Join { ticket } => join(&dir, &ticket),
        Cmd::Start {
            relay,
            no_relay,
            lan,
        } => start(&dir, relay, no_relay, lan, ui).await,
        Cmd::Status { watch, json } => handle_status_cli(&dir, watch, json).await,
        Cmd::Doctor { json } => handle_doctor_cli(&dir, json).await,
        Cmd::Invite { qr } => {
            let data = request(&dir, IpcRequest::Invite).await?;
            let ticket = data
                .get("ticket")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if qr {
                print_qr_invite(ticket);
            } else {
                println!("Share this ticket to invite someone:\n\n  {ticket}\n");
            }
            Ok(())
        }
        Cmd::Lock { path } => handle_lock_cli(&dir, &path, verbose).await,
        Cmd::Unlock { path } => {
            request(&dir, IpcRequest::Unlock { path: path.clone() }).await?;
            println!("✔ {path} synced and read-only again");
            Ok(())
        }
        Cmd::Versions { path } => {
            let data = request(&dir, IpcRequest::Versions { path }).await?;
            print_versions(&data);
            Ok(())
        }
        Cmd::Restore { path, n } => {
            let data = request(
                &dir,
                IpcRequest::Restore {
                    path: path.clone(),
                    n,
                },
            )
            .await?;
            let size = data.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("✔ restored version {n} of {path} ({size} bytes) and broadcast it");
            Ok(())
        }
        Cmd::Gc => {
            request(&dir, IpcRequest::Gc).await?;
            println!("✔ garbage collection finished");
            Ok(())
        }
    }
}

/// Acquires a lease, with a pre-acquire advisory for degraded links and a
/// network-terms diagnosis block on failure.
async fn handle_lock_cli(dir: &Path, path: &str, verbose: bool) -> Result<(), CliError> {
    // Pre-acquire advisory (non-blocking): warn if any connected peer that
    // would be consulted grades Poor. Best-effort — a status hiccup never
    // stops the lock.
    if let Ok(status) = ipc::request(dir, &IpcRequest::Status).await
        && status.ok
        && let Some(members) = status.data.as_ref().and_then(|d| d["members"].as_array())
    {
        for m in members {
            let conn = m["conn"].as_str().unwrap_or("None");
            if conn != "None" && m["grade"].as_str() == Some("Poor") {
                let id = short(m["id"].as_str().unwrap_or("?"));
                let rtt = m["rtt_ms"].as_f64().unwrap_or(0.0);
                eprintln!(
                    "⚠ acquiring via a degraded link to {id} ({conn}, {rtt:.0}ms) — sync may lag behind edits"
                );
            }
        }
    }

    let response = ipc::request(dir, &IpcRequest::Lock { path: path.into() }).await?;
    if response.ok {
        let data = response.data.unwrap_or(serde_json::Value::Null);
        let ttl = data.get("ttl_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        if data.get("already").and_then(|v| v.as_bool()) == Some(true) {
            println!("✔ {path} — you already hold this lease");
        } else {
            println!(
                "✔ {path} is now writable (lease TTL {}s, auto-renewed)",
                ttl / 1000
            );
        }
        return Ok(());
    }

    let err = response.error.clone().unwrap_or(crate::ipc::IpcErrorBody {
        code: "unknown".into(),
        message: "daemon returned an empty error".into(),
    });
    print_lock_diagnosis(path, &err, response.data.as_ref(), verbose);
    Err(CliError::DaemonRefused {
        code: err.code,
        message: err.message,
    })
}

/// Prints the compact lock-failure diagnosis; with `verbose`, a per-peer table.
fn print_lock_diagnosis(
    path: &str,
    err: &crate::ipc::IpcErrorBody,
    data: Option<&serde_json::Value>,
    verbose: bool,
) {
    eprintln!("✗ could not lock {path}: {}", err.message);
    let Some(diag) = data.and_then(|d| d.get("diagnosis")) else {
        return;
    };
    let precondition = diag["precondition"].as_str().unwrap_or("?");
    eprintln!("  blocked precondition : {precondition}");
    if let Some(hint) = diag["hint"].as_str() {
        eprintln!("  what to do           : {hint}");
    }
    let peers = diag["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        return;
    }
    if verbose {
        eprintln!("  peers consulted:");
        eprintln!(
            "    {:<12} {:<8} {:<8} {:>8}  answered",
            "id", "grade", "conn", "rtt"
        );
        for p in &peers {
            let answered = match p["answered"].as_bool() {
                Some(true) => "yes",
                Some(false) => "NO",
                None => "-",
            };
            eprintln!(
                "    {:<12} {:<8} {:<8} {:>6.0}ms  {answered}",
                short(p["id"].as_str().unwrap_or("?")),
                p["grade"].as_str().unwrap_or("?"),
                p["conn"].as_str().unwrap_or("?"),
                p["rtt_ms"].as_f64().unwrap_or(0.0),
            );
        }
    } else {
        let names: Vec<String> = peers
            .iter()
            .map(|p| {
                format!(
                    "{} ({}, {})",
                    short(p["id"].as_str().unwrap_or("?")),
                    p["grade"].as_str().unwrap_or("?"),
                    p["conn"].as_str().unwrap_or("?"),
                )
            })
            .collect();
        eprintln!("  peers consulted      : {}", names.join(", "));
        eprintln!("  (use -v for the full per-peer table)");
    }
}

fn short(id: &str) -> String {
    id.chars().take(10).collect()
}

async fn request(dir: &Path, req: IpcRequest) -> Result<serde_json::Value, CliError> {
    let response: IpcResponse = ipc::request(dir, &req).await?;
    if response.ok {
        Ok(response.data.unwrap_or(serde_json::Value::Null))
    } else {
        let err = response.error.unwrap_or(crate::ipc::IpcErrorBody {
            code: "unknown".into(),
            message: "daemon returned an empty error".into(),
        });
        Err(CliError::DaemonRefused {
            code: err.code,
            message: err.message,
        })
    }
}

/// Initializes a folder as a new session (shared by the CLI and tests).
pub fn init(dir: &Path) -> Result<(), CliError> {
    if AppState::exists(dir) {
        return Err(CliError::Refused(
            "this folder is already a tazamun session".into(),
        ));
    }
    std::fs::create_dir_all(dir).map_err(crate::state::StateError::Io)?;
    let secret_key = iroh::SecretKey::generate();
    let session_secret: [u8; 32] = rand::random();
    let state = AppState::new(
        encode_hex32(&secret_key.to_bytes()),
        encode_hex32(&session_secret),
    );
    state.save(dir)?;
    let me = secret_key.public();
    let ticket = Ticket::new(
        SessionSecret(session_secret),
        vec![AddrWire {
            id: *me.as_bytes(),
            relay: None,
            direct: vec![],
        }],
    );
    println!("Initialized tazamun session");
    println!("  folder : {}", dir.display());
    println!("  peer id: {me}");
    println!(
        "\nShare this ticket to invite someone:\n\n  {}\n",
        ticket.encode()
    );
    println!("Tip: after `tazamun start`, run `tazamun invite` for a ticket");
    println!("that also carries your live addresses (fastest connection).");
    Ok(())
}

/// Prepares a folder to join an existing session (shared by the CLI and tests).
pub fn join(dir: &Path, ticket: &str) -> Result<(), CliError> {
    if AppState::exists(dir) {
        return Err(CliError::Refused(
            "this folder is already a tazamun session".into(),
        ));
    }
    std::fs::create_dir_all(dir).map_err(crate::state::StateError::Io)?;
    let non_meta = std::fs::read_dir(dir)
        .map_err(crate::state::StateError::Io)?
        .flatten()
        .any(|e| {
            !e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".tazamun")
        });
    if non_meta {
        return Err(CliError::Refused(
            "join requires an empty folder (existing files would be ambiguous; \
             move them away and lock them in after joining)"
                .into(),
        ));
    }
    let ticket = Ticket::decode(ticket)?;
    let secret_key = iroh::SecretKey::generate();
    let me = secret_key.public();
    let mut state = AppState::new(
        encode_hex32(&secret_key.to_bytes()),
        encode_hex32(&ticket.secret.0),
    );
    for addr in &ticket.bootstrap {
        if addr.id != *me.as_bytes()
            && let Some(id) = addr.endpoint_id()
        {
            state.known_members.insert(id.to_string(), addr.clone());
        }
    }
    state.save(dir)?;
    println!("Joined tazamun session");
    println!("  folder : {}", dir.display());
    println!("  peer id: {me}");
    println!("\nRun `tazamun start` to begin syncing.");
    Ok(())
}

async fn start(
    dir: &Path,
    relay: Option<String>,
    no_relay: bool,
    lan: bool,
    ui: Ui,
) -> Result<(), CliError> {
    let relay_choice = match (relay, no_relay) {
        (Some(url), _) => {
            RelayChoice::Custom(url.parse().map_err(|e: iroh::RelayUrlParseError| {
                CliError::Refused(format!("invalid relay url: {e}"))
            })?)
        }
        (None, true) => RelayChoice::Disabled,
        (None, false) => RelayChoice::Default,
    };
    let cfg = DaemonConfig {
        dir: dir.to_path_buf(),
        net: NetConfig {
            relay: relay_choice,
            lan,
            test_bind: None,
        },
        timings: LockTimings::default(),
        ui,
    };
    let handle = crate::daemon::spawn(cfg).await?;
    println!("tazamun daemon running");
    println!("  folder : {}", dir.display());
    println!("  peer id: {}", handle.id());
    println!("\nPress Ctrl-C to stop. Use `tazamun status` from another shell.");
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            println!("\nStopping: releasing leases and saying goodbye…");
        }
        Err(e) => {
            eprintln!("signal handler failed ({e}); shutting down");
        }
    }
    handle.shutdown().await;
    println!("Stopped cleanly.");
    Ok(())
}

/// `status`: one-shot table, live `--watch` panel, or `--json` snapshot.
async fn handle_status_cli(dir: &Path, watch: bool, json: bool) -> Result<(), CliError> {
    if json {
        let data = request(dir, IpcRequest::Status).await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&data).unwrap_or_default()
        );
        return Ok(());
    }
    if watch && std::io::stdout().is_terminal() {
        return watch_status(dir).await;
    }
    let data = request(dir, IpcRequest::Status).await?;
    let mut out = String::new();
    render_status(&data, &mut out);
    print!("{out}");
    Ok(())
}

/// Live panel: clear + redraw once a second until `q` or Ctrl-C.
async fn watch_status(dir: &Path) -> Result<(), CliError> {
    use console::Term;
    let term = Term::stdout();
    let _ = term.hide_cursor();
    let result = watch_status_loop(dir, &term).await;
    let _ = term.show_cursor();
    let _ = term.clear_last_lines(0);
    result
}

async fn watch_status_loop(dir: &Path, term: &console::Term) -> Result<(), CliError> {
    // Poll stdin for 'q' on a blocking thread; Ctrl-C is handled by tokio.
    let (quit_tx, mut quit_rx) = tokio::sync::mpsc::channel::<()>(1);
    let key_term = term.clone();
    std::thread::spawn(move || {
        while let Ok(ch) = key_term.read_char() {
            if ch == 'q' || ch == 'Q' {
                let _ = quit_tx.blocking_send(());
                break;
            }
        }
    });
    loop {
        let data = request(dir, IpcRequest::Status).await?;
        let mut out = String::new();
        render_status(&data, &mut out);
        out.push_str("\n(refreshing every 1s — press q or Ctrl-C to exit)\n");
        let _ = term.clear_screen();
        print!("{out}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = quit_rx.recv() => return Ok(()),
        }
    }
}

/// A colored grade dot, honoring NO_COLOR and non-TTY output.
fn grade_dot(grade: &str) -> String {
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let (glyph, color) = match grade {
        "Good" => ("●", "\x1b[32m"),
        "Fair" => ("●", "\x1b[33m"),
        "Poor" => ("●", "\x1b[31m"),
        _ => ("○", "\x1b[90m"),
    };
    if colored {
        format!("{color}{glyph}\x1b[0m")
    } else {
        glyph.to_string()
    }
}

/// Renders the human status table into `out`.
fn render_status(data: &serde_json::Value, out: &mut String) {
    use std::fmt::Write;
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("-").to_string();
    let _ = writeln!(out, "peer id : {}", s(&data["id"]));
    let _ = writeln!(out, "folder  : {}", s(&data["dir"]));
    let _ = writeln!(
        out,
        "files   : {} ({} bytes)",
        data["file_count"].as_u64().unwrap_or(0),
        data["total_bytes"].as_u64().unwrap_or(0)
    );
    let members = data["members"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(out, "\nmembers ({}):", members.len());
    if members.is_empty() {
        let _ = writeln!(out, "  (none known yet — share `tazamun invite`)");
    }
    for m in members {
        let grade = m["grade"].as_str().unwrap_or("Offline");
        let conn = s(&m["conn"]);
        let rtt = m["rtt_ms"].as_f64().unwrap_or(0.0);
        let jitter = m["rtt_jitter_ms"].as_f64().unwrap_or(0.0);
        let paths = m["path_changes"].as_u64().unwrap_or(0);
        let rtt_col = if conn == "None" {
            "-".to_string()
        } else {
            format!("{rtt:.0}±{jitter:.0}ms")
        };
        let relay = m["relay_url"]
            .as_str()
            .and_then(relay_host)
            .map(|h| format!(" via {h}"))
            .unwrap_or_default();
        let rate_rx = m["rate_rx_bps"].as_f64().unwrap_or(0.0) / 1_000_000.0;
        let rate_tx = m["rate_tx_bps"].as_f64().unwrap_or(0.0) / 1_000_000.0;
        let rates = if rate_rx > 0.05 || rate_tx > 0.05 {
            format!("  ↓{rate_rx:.1} ↑{rate_tx:.1} MB/s")
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  {} {:<6} {:<10} {:<7} {:<12} Δ{}{}{}",
            grade_dot(grade),
            grade,
            short(&s(&m["id"])),
            conn,
            rtt_col,
            paths,
            relay,
            rates,
        );
    }
    let leases = data["leases"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(out, "\nactive leases ({}):", leases.len());
    for l in leases {
        let holder = if l["mine"].as_bool().unwrap_or(false) {
            "you".to_string()
        } else {
            short(&s(&l["holder"]))
        };
        let _ = writeln!(
            out,
            "  {}  held by {}  expires in {}s",
            s(&l["path"]),
            holder,
            l["expires_in_ms"].as_u64().unwrap_or(0) / 1000
        );
    }
    let pulls = data["pending_pulls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !pulls.is_empty() {
        let _ = writeln!(out, "\ntransfers ({}):", pulls.len());
        for p in pulls {
            let path = p["path"].as_str().unwrap_or("-");
            let percent = p["percent"].as_u64().unwrap_or(0);
            let rate = p["rate_bytes_per_sec"].as_u64().unwrap_or(0) as f64 / 1_000_000.0;
            let _ = writeln!(out, "  ⇣ {path}  {percent:>3}%  {rate:.1} MB/s");
        }
    }
    let events = data["events"].as_array().cloned().unwrap_or_default();
    if !events.is_empty() {
        let _ = writeln!(out, "\nrecent events:");
        for e in events {
            let _ = writeln!(out, "  • {}", e["text"].as_str().unwrap_or("-"));
        }
    }
}

/// `doctor`: builds the report from the daemon's live view (when running)
/// plus local probes, prints it, and exits 0/1/2 on OK/WARN/FAIL.
async fn handle_doctor_cli(dir: &Path, json: bool) -> Result<(), CliError> {
    use crate::doctor::{
        Report, Section, Verdict, classify_mount, filesystem_section, ipc_section, relay_section,
    };

    let mut sections: Vec<Section> = Vec::new();

    // Query the running daemon (best-effort).
    let daemon = ipc::request(dir, &IpcRequest::Doctor)
        .await
        .ok()
        .filter(|r| r.ok)
        .and_then(|r| r.data);
    let alive = daemon.is_some();

    // (a) identity + bound sockets (from daemon).
    match &daemon {
        Some(d) => {
            let mut s = Section::from_ok("identity  [from daemon]");
            s.lines.push(format!(
                "peer id            : {}",
                d["id"].as_str().unwrap_or("-")
            ));
            let socks = d["bound_sockets"].as_array().cloned().unwrap_or_default();
            if socks.is_empty() {
                s.lines.push("bound sockets      : (none reported)".into());
            }
            for so in socks {
                s.lines.push(format!(
                    "bound socket       : {}",
                    so.as_str().unwrap_or("-")
                ));
            }
            sections.push(s);
        }
        None => {
            let mut s = Section::from_warn("identity");
            s.lines
                .push("daemon not running — identity and live network probes unavailable".into());
            s.action = Some("run `tazamun start` and re-run `tazamun doctor`".into());
            sections.push(s);
        }
    }

    // (b) relay: policy/home relay from daemon; disabled-by-flag is OK.
    let policy = daemon
        .as_ref()
        .and_then(|d| d["relay_policy"].as_str())
        .unwrap_or("unknown (daemon not running)")
        .to_string();
    let home = daemon.as_ref().and_then(|d| d["home_relay"].as_str());
    // We do not open a fresh endpoint here; the daemon's home-relay presence is
    // the reachability signal, so no separate probe unless disabled.
    let relay_probe = home.map(|_| Ok(0u128));
    sections.push(relay_section(&policy, home, relay_probe));

    // (c) NAT / hole-punch (from daemon telemetry).
    if let Some(d) = &daemon {
        let peers = d["peers"].as_array().cloned().unwrap_or_default();
        let mut s = Section::from_ok("connectivity  [from daemon]");
        if peers.is_empty() {
            s.lines
                .push("no connected peers — nothing to hole-punch yet".into());
        }
        for p in &peers {
            let id = short(p["id"].as_str().unwrap_or("?"));
            let conn = p["conn"].as_str().unwrap_or("None");
            let grade = p["grade"].as_str().unwrap_or("Offline");
            let rtt = p["rtt_ms"].as_f64().unwrap_or(0.0);
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
            s.lines.push(format!(
                "peer {id}      : {conn} ({grade}, {rtt:.0}ms{ttd})"
            ));
            if conn == "Relayed" {
                s.verdict = s.verdict.max_with(Verdict::Warn);
                s.action.get_or_insert_with(|| {
                    "a peer is reachable only via relay — direct hole-punching has not \
                     succeeded; run `tazamun doctor` on both ends and check NAT/firewalls"
                        .to_string()
                });
            }
        }
        sections.push(s);
    }

    // (d) filesystem sanity (local probe).
    sections.push(filesystem_section(dir, classify_mount(dir)));

    // (e) IPC health (local).
    sections.push(ipc_section(dir, alive));

    let report = Report { sections };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print_doctor(&report);
    }
    if report.exit_code() != 0 {
        std::process::exit(report.exit_code());
    }
    Ok(())
}

fn print_doctor(report: &crate::doctor::Report) {
    use crate::doctor::Verdict;
    let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let tag = |v: Verdict| -> String {
        let (label, color) = match v {
            Verdict::Ok => ("OK  ", "\x1b[32m"),
            Verdict::Warn => ("WARN", "\x1b[33m"),
            Verdict::Fail => ("FAIL", "\x1b[31m"),
        };
        if colored {
            format!("{color}{label}\x1b[0m")
        } else {
            label.to_string()
        }
    };
    println!("tazamun doctor\n");
    for sec in &report.sections {
        println!("[{}] {}", tag(sec.verdict), sec.name);
        for line in &sec.lines {
            println!("     {line}");
        }
        if let Some(action) = &sec.action {
            println!("     → {action}");
        }
        println!();
    }
    println!("summary: {}", tag(report.worst()));
}

/// Extracts a bare hostname from a relay URL for compact display.
fn relay_host(url: &str) -> Option<String> {
    let after = url.split("://").nth(1).unwrap_or(url);
    let host = after.split(['/', ':']).next().unwrap_or(after);
    if host.is_empty() {
        None
    } else {
        Some(host.trim_end_matches('.').to_string())
    }
}

/// Renders the exact ticket string as a terminal QR code (unicode
/// half-blocks, inverted for dark terminals — phone scanners read both
/// polarities). Falls back to the plain ticket when the terminal is too
/// narrow for a scannable code.
fn print_qr_invite(ticket: &str) {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    let code = match QrCode::new(ticket.as_bytes()) {
        Ok(code) => code,
        Err(e) => {
            println!("(QR encoding failed: {e}; showing the plain ticket)\n");
            println!("Share this ticket to invite someone:\n\n  {ticket}\n");
            return;
        }
    };
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    let qr_width = image.lines().next().map(|l| l.chars().count()).unwrap_or(0);
    let term_cols = console::Term::stdout().size().1 as usize;
    // Unknown width (pipes) counts as wide enough — the QR was asked for.
    if term_cols != 0 && qr_width > term_cols {
        println!(
            "(terminal is {term_cols} columns; the QR needs {qr_width} — showing the plain ticket)\n"
        );
        println!("Share this ticket to invite someone:\n\n  {ticket}\n");
        return;
    }
    println!("Scan to join this session:\n");
    println!("{image}");
    println!("\nSame ticket as text:\n\n  {ticket}\n");
}

fn print_versions(data: &serde_json::Value) {
    let path = data["path"].as_str().unwrap_or("-");
    let versions = data["versions"].as_array().cloned().unwrap_or_default();
    if versions.is_empty() {
        println!("no kept versions for {path}");
        return;
    }
    println!("versions of {path} (newest first):");
    for v in versions {
        println!(
            "  [{}] {}  {} bytes",
            v["n"].as_u64().unwrap_or(0),
            v["ts"].as_str().unwrap_or("-"),
            v["size"].as_u64().unwrap_or(0)
        );
    }
    println!("\nrestore with: tazamun restore {path} <N> (requires a held lease)");
}
