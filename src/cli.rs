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
    #[command(flatten)]
    pub net: NetFlags,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// Network preference flags shared by `init`, `join`, and `start`.
/// At `init`/`join` these are persisted into `state.json`; at `start` they are
/// per-run overrides on top of the persisted config.
#[derive(Debug, Default, clap::Args)]
pub struct NetFlags {
    /// Use a self-hosted relay instead of the public ones.
    #[arg(long, conflicts_with = "no_relay", global = true)]
    pub relay: Option<String>,
    /// Disable relays entirely (LAN / manually routed setups).
    #[arg(long, global = true)]
    pub no_relay: bool,
    /// Disable LAN mDNS discovery (enabled by default).
    #[arg(long, global = true)]
    pub no_lan: bool,
    /// Closed-network mode: no relays and no external discovery of any kind.
    #[arg(long, global = true)]
    pub airgap: bool,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Initialize this folder as a new sync session and print an invite.
    Init,
    /// Join an existing session from an invite ticket.
    Join { ticket: String },
    /// Run the sync daemon in the foreground. Network preferences come from
    /// `state.json`; the global `--relay/--no-relay/--no-lan/--airgap` flags
    /// override them for this run only.
    Start,
    /// Show or change persisted per-session network preferences.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
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
    /// List active leases: holder, age, and expiry countdown.
    Locks,
    /// Acquire an exclusive lease and make the file writable.
    Lock {
        path: String,
        /// If the path is already held, wait for it to free and auto-acquire.
        #[arg(long)]
        wait: bool,
    },
    /// Publish pending edits and release the lease.
    Unlock { path: String },
    /// List kept historical versions of a path.
    Versions { path: String },
    /// Restore version N of a path (requires a held lease).
    Restore { path: String, n: usize },
    /// Delete unreferenced blobs from the local store.
    Gc,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print the persisted network preferences.
    Show,
    /// Change a persisted preference. Keys: `relay <default|none|URL>`,
    /// `lan <on|off>`, `airgap <on|off>`, `lease-ttl <90s|15m|2h>`,
    /// `acquire-timeout <8s>`, `autolock <on|off>`, `wait-timeout <10m>`.
    /// Takes effect on next `start`.
    Set { key: String, value: String },
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
    let net = cli.net;
    match cli.cmd {
        Cmd::Init => {
            init(&dir)?;
            persist_net_flags(&dir, &net)?;
            Ok(())
        }
        Cmd::Join { ticket } => {
            join(&dir, &ticket)?;
            persist_net_flags(&dir, &net)?;
            Ok(())
        }
        Cmd::Start => start(&dir, &net, ui).await,
        Cmd::Config { cmd } => handle_config_cli(&dir, cmd),
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
        Cmd::Locks => handle_locks_cli(&dir).await,
        Cmd::Lock { path, wait } => handle_lock_cli(&dir, &path, wait, verbose).await,
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
/// network-terms diagnosis block on failure. With `wait`, a `held` failure
/// registers interest and re-attempts the full acquire until the path frees or
/// the configured `wait-timeout` elapses.
async fn handle_lock_cli(
    dir: &Path,
    path: &str,
    wait: bool,
    verbose: bool,
) -> Result<(), CliError> {
    lock_advisory(dir).await;

    let deadline = if wait {
        let wt = AppState::load(dir)?.config.wait_timeout();
        Some(std::time::Instant::now() + wt)
    } else {
        None
    };
    let mut announced = false;
    loop {
        let response = ipc::request(dir, &IpcRequest::Lock { path: path.into() }).await?;
        if response.ok {
            let data = response.data.unwrap_or(serde_json::Value::Null);
            let ttl = data.get("ttl_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            if data.get("already").and_then(|v| v.as_bool()) == Some(true) {
                println!("✔ {path} — you already hold this lease");
            } else {
                if announced {
                    print!("\x07"); // bell: the wait resolved
                }
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

        // Waitlist: on a `held` refusal, register interest with the daemon and
        // retry the full acquire (preconditions re-checked fresh each round)
        // until the deadline. The daemon fast-wakes us on a LockFreed; the poll
        // interval is only a fallback ceiling.
        if let Some(deadline) = deadline
            && err.code == "lease_held"
            && std::time::Instant::now() < deadline
        {
            if !announced {
                let by = response
                    .data
                    .as_ref()
                    .and_then(|d| d["diagnosis"]["held_by"].as_str())
                    .map(short)
                    .unwrap_or_else(|| "another peer".into());
                // Register interest once; the daemon tells the holder and shows
                // us in `status`/`locks`. Subsequent rounds just re-attempt.
                let _ = ipc::request(dir, &IpcRequest::LockWait { path: path.into() }).await;
                eprintln!(
                    "… {path} is held by {by}; waiting (auto-acquires when free, Ctrl-C to stop)"
                );
                announced = true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        }

        if announced {
            eprintln!("✗ gave up waiting for {path} after the wait-timeout");
        }
        print_lock_diagnosis(path, &err, response.data.as_ref(), verbose);
        return Err(CliError::DaemonRefused {
            code: err.code,
            message: err.message,
        });
    }
}

/// Best-effort pre-acquire advisory: warn if a peer that would be consulted
/// grades Poor. A status hiccup never blocks the lock.
async fn lock_advisory(dir: &Path) {
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
}

/// `locks`: list active leases (holder, age, expiry) from the same IPC snapshot
/// that `status` uses, so the two never disagree.
async fn handle_locks_cli(dir: &Path) -> Result<(), CliError> {
    let data = request(dir, IpcRequest::Status).await?;
    let leases = data["leases"].as_array().cloned().unwrap_or_default();
    let waiting = data["waiting"].as_array().cloned().unwrap_or_default();
    if leases.is_empty() {
        println!("no active leases");
    } else {
        println!("active leases ({}):", leases.len());
        for l in &leases {
            let path = l["path"].as_str().unwrap_or("?");
            let holder = short(l["holder"].as_str().unwrap_or("?"));
            let mine = l["mine"].as_bool() == Some(true);
            let age = fmt_ms(l["age_ms"].as_u64().unwrap_or(0));
            let left = fmt_ms(l["expires_in_ms"].as_u64().unwrap_or(0));
            let who = if mine { "you".to_string() } else { holder };
            println!("  {path}  held by {who}  age {age}  expires in {left}");
        }
    }
    if !waiting.is_empty() {
        println!("\nwaiting:");
        for w in &waiting {
            let path = w["path"].as_str().unwrap_or("?");
            let behind = short(w["behind"].as_str().unwrap_or("?"));
            println!("  {path}  (behind {behind})");
        }
    }
    Ok(())
}

/// Formats a millisecond duration compactly (e.g. `1m 30s`, `4s`).
fn fmt_ms(ms: u64) -> String {
    // Round to whole seconds so the countdown reads cleanly.
    let secs = ms / 1000;
    fmt_dur(std::time::Duration::from_secs(secs))
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

/// Folds per-run [`NetFlags`] onto the persisted [`SessionConfig`] (flag >
/// config > default) into the runtime [`NetConfig`].
fn resolve_net_config(
    saved: &crate::state::SessionConfig,
    flags: &NetFlags,
) -> Result<NetConfig, CliError> {
    let airgap = flags.airgap || saved.airgap;
    let relay = if airgap {
        RelayChoice::Disabled
    } else {
        match (&flags.relay, flags.no_relay) {
            (Some(url), _) => RelayChoice::parse(url).map_err(CliError::Refused)?,
            (None, true) => RelayChoice::Disabled,
            (None, false) => RelayChoice::parse(&saved.relay).map_err(CliError::Refused)?,
        }
    };
    // LAN is on by default and always on in airgap; --no-lan turns it off.
    let lan = airgap || (saved.lan && !flags.no_lan);
    Ok(NetConfig {
        relay,
        lan,
        airgap,
        test_bind: None,
        test_relay: None,
    })
}

/// Persists explicit network flags into the session config (used by
/// `init`/`join`). Absent flags leave the existing/default value untouched.
fn persist_net_flags(dir: &Path, flags: &NetFlags) -> Result<(), CliError> {
    if flags.relay.is_none() && !flags.no_relay && !flags.no_lan && !flags.airgap {
        return Ok(());
    }
    let mut state = AppState::load(dir)?;
    if let Some(url) = &flags.relay {
        let choice = RelayChoice::parse(url).map_err(CliError::Refused)?;
        state.config.relay = choice.to_config_string();
    } else if flags.no_relay {
        state.config.relay = "none".to_string();
    }
    if flags.no_lan {
        state.config.lan = false;
    }
    if flags.airgap {
        state.config.airgap = true;
    }
    state.save(dir)?;
    Ok(())
}

async fn start(dir: &Path, flags: &NetFlags, ui: Ui) -> Result<(), CliError> {
    let saved = AppState::load(dir)?.config;
    let net = resolve_net_config(&saved, flags)?;
    let airgap = net.airgap;
    // Lease timings come from the persisted session config (clamped). TTL is
    // lease-scoped on the wire, so peers may run different values safely.
    let timings = LockTimings {
        ttl: saved.lease_ttl(),
        renew: saved.lease_renew(),
        acquire_timeout: saved.acquire_timeout(),
    };
    let cfg = DaemonConfig {
        dir: dir.to_path_buf(),
        net,
        timings,
        ui,
    };
    let handle = crate::daemon::spawn(cfg).await?;
    println!("tazamun daemon running");
    println!("  folder : {}", dir.display());
    println!("  peer id: {}", handle.id());
    if airgap {
        println!("  mode   : AIRGAP — no relays, no external discovery, LAN only");
    }
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

/// `config show|set` — reads/writes the persisted per-session preferences.
fn handle_config_cli(dir: &Path, cmd: ConfigCmd) -> Result<(), CliError> {
    let mut state = AppState::load(dir)?;
    match cmd {
        ConfigCmd::Show => {
            let c = &state.config;
            println!("session preferences (folder: {})", dir.display());
            println!("  relay           : {}", c.relay);
            println!("  lan             : {}", on_off(c.lan));
            println!("  airgap          : {}", on_off(c.airgap));
            println!(
                "  lease-ttl       : {} (renew every {})",
                fmt_dur(c.lease_ttl()),
                fmt_dur(c.lease_renew())
            );
            println!("  acquire-timeout : {}", fmt_dur(c.acquire_timeout()));
            println!("  autolock        : {}", on_off(c.autolock));
            println!("  wait-timeout    : {}", fmt_dur(c.wait_timeout()));
            println!(
                "\n(values shown are effective/clamped; per-run flags override network keys; changes apply on next `tazamun start`)"
            );
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            match key.as_str() {
                "relay" => {
                    // Validate before persisting.
                    let choice = RelayChoice::parse(&value).map_err(CliError::Refused)?;
                    state.config.relay = choice.to_config_string();
                }
                "lan" => {
                    state.config.lan = parse_on_off(&value)?;
                }
                "airgap" => {
                    state.config.airgap = parse_on_off(&value)?;
                }
                "lease-ttl" => {
                    let d = parse_clamped_dur(
                        &value,
                        crate::consts::MIN_LEASE_TTL,
                        crate::consts::MAX_LEASE_TTL,
                    )?;
                    state.config.lease_ttl_ms = d.as_millis() as u64;
                }
                "acquire-timeout" => {
                    let d = parse_clamped_dur(
                        &value,
                        crate::consts::MIN_ACQUIRE_TIMEOUT,
                        crate::consts::MAX_ACQUIRE_TIMEOUT,
                    )?;
                    state.config.acquire_timeout_ms = d.as_millis() as u64;
                }
                "autolock" => {
                    state.config.autolock = parse_on_off(&value)?;
                }
                "wait-timeout" => {
                    let d = parse_clamped_dur(
                        &value,
                        std::time::Duration::from_secs(10),
                        crate::consts::MAX_LEASE_TTL,
                    )?;
                    state.config.wait_timeout_ms = d.as_millis() as u64;
                }
                other => {
                    return Err(CliError::Refused(format!(
                        "unknown config key {other:?} (valid: relay, lan, airgap, \
                         lease-ttl, acquire-timeout, autolock, wait-timeout)"
                    )));
                }
            }
            state.save(dir)?;
            println!("✔ config set {key} = {value}");
            println!("(applies on next `tazamun start`)");
            Ok(())
        }
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn fmt_dur(d: std::time::Duration) -> String {
    humantime::format_duration(d).to_string()
}

/// Parses a humantime duration (e.g. `90s`, `15m`, `2h`) and clamps it to
/// `[min, max]`, printing a note when the input fell outside the range.
fn parse_clamped_dur(
    value: &str,
    min: std::time::Duration,
    max: std::time::Duration,
) -> Result<std::time::Duration, CliError> {
    let raw = humantime::parse_duration(value.trim()).map_err(|e| {
        CliError::Refused(format!(
            "invalid duration {value:?}: {e} (use forms like 90s, 15m, 2h)"
        ))
    })?;
    let clamped = raw.clamp(min, max);
    if clamped != raw {
        println!(
            "note: {} is outside [{}, {}]; clamped to {}",
            fmt_dur(raw),
            fmt_dur(min),
            fmt_dur(max),
            fmt_dur(clamped)
        );
    }
    Ok(clamped)
}

fn parse_on_off(value: &str) -> Result<bool, CliError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => Err(CliError::Refused(format!("expected on/off, got {other:?}"))),
    }
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
        // Path origin: relay hostname when relayed, "via LAN" for a direct
        // link to a local-network peer (mDNS discovery).
        let relay = if let Some(h) = m["relay_url"].as_str().and_then(relay_host) {
            format!(" via {h}")
        } else if m["via_lan"].as_bool() == Some(true) {
            " via LAN".to_string()
        } else {
            String::new()
        };
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
    let unapplied = data["unapplied"].as_array().cloned().unwrap_or_default();
    if !unapplied.is_empty() {
        let _ = writeln!(
            out,
            "\nunapplied — non-portable paths ({}):",
            unapplied.len()
        );
        for u in &unapplied {
            let _ = writeln!(out, "  ⚠ {}  ({})", s(&u["path"]), s(&u["reason"]));
        }
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

    // Portability (from daemon): count of remote records held unapplied
    // because their paths cannot exist on this filesystem.
    if let Some(n) = daemon
        .as_ref()
        .and_then(|d| d["unapplied_count"].as_u64())
        .filter(|n| *n > 0)
    {
        let mut s = Section::from_warn("portability  [from daemon]");
        s.lines.push(format!(
            "unapplied paths    : {n} remote file(s) held back (non-portable names)"
        ));
        s.action = Some(
            "run `tazamun status` for the list; rename on the origin node to sync them".into(),
        );
        sections.push(s);
    }

    // Airgap banner (from daemon): list the closed-network guarantees.
    if daemon.as_ref().and_then(|d| d["mode"].as_str()) == Some("airgap") {
        let mut s = Section::from_ok("mode  [from daemon]");
        s.lines
            .push("mode               : AIRGAP (closed network)".into());
        s.lines
            .push("guarantees         : no relays · no DNS/pkarr discovery · LAN mDNS only".into());
        s.lines
            .push("egress             : nothing is contacted outside the local network".into());
        sections.push(s);
    }

    // (b) relay: policy from daemon; disabled-by-flag is OK. When a custom
    // relay is configured, probe IT via the daemon's live relay handshake
    // (relay_status), not the defaults.
    let policy = daemon
        .as_ref()
        .and_then(|d| d["relay_policy"].as_str())
        .unwrap_or("unknown (daemon not running)")
        .to_string();
    let home = daemon.as_ref().and_then(|d| d["home_relay"].as_str());
    // The daemon's actual relay handshake result is the reachability probe:
    // Ok when the configured relay is connected, Err when it is not.
    let relay_probe: Option<Result<u128, String>> = daemon
        .as_ref()
        .and_then(|d| d["relay_status"].as_array())
        .and_then(|st| st.first())
        .map(|r| {
            if r["connected"].as_bool() == Some(true) {
                Ok(0u128)
            } else {
                Err(format!(
                    "relay {} did not complete a connection",
                    r["url"].as_str().unwrap_or("?")
                ))
            }
        })
        .or_else(|| home.map(|_| Ok(0u128)));
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
            let lan = if p["via_lan"].as_bool() == Some(true) {
                " via LAN"
            } else {
                ""
            };
            s.lines.push(format!(
                "peer {id}      : {conn}{lan} ({grade}, {rtt:.0}ms{ttd})"
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
    if let Some(s) = crate::doctor::long_paths_section(dir) {
        sections.push(s);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::endpoint::RelayChoice;
    use crate::state::SessionConfig;

    fn flags(relay: Option<&str>, no_relay: bool, no_lan: bool, airgap: bool) -> NetFlags {
        NetFlags {
            relay: relay.map(String::from),
            no_relay,
            no_lan,
            airgap,
        }
    }

    #[test]
    fn precedence_flag_beats_config_beats_default() {
        // Default config, no flags → n0 default relay, LAN on, not airgap.
        let saved = SessionConfig::default();
        let n = resolve_net_config(&saved, &flags(None, false, false, false)).unwrap();
        assert!(matches!(n.relay, RelayChoice::Default));
        assert!(n.lan && !n.airgap);

        // Persisted custom relay + LAN off is honored when no flags override.
        let saved = SessionConfig {
            relay: "https://saved.example./".into(),
            lan: false,
            airgap: false,
            ..SessionConfig::default()
        };
        let n = resolve_net_config(&saved, &flags(None, false, false, false)).unwrap();
        assert!(matches!(n.relay, RelayChoice::Custom(_)));
        assert!(!n.lan);

        // A per-run --no-relay flag overrides the persisted custom relay.
        let n = resolve_net_config(&saved, &flags(None, true, false, false)).unwrap();
        assert!(matches!(n.relay, RelayChoice::Disabled));

        // A per-run --relay URL overrides both.
        let n = resolve_net_config(
            &saved,
            &flags(Some("https://run.example./"), false, false, false),
        )
        .unwrap();
        match n.relay {
            RelayChoice::Custom(u) => assert!(u.to_string().contains("run.example")),
            other => panic!("expected run.example, got {other:?}"),
        }
    }

    #[test]
    fn airgap_forces_relay_off_and_lan_on() {
        // Airgap via flag: relay disabled, LAN on, even if config said otherwise.
        let saved = SessionConfig {
            relay: "https://x.example./".into(),
            lan: false,
            airgap: false,
            ..SessionConfig::default()
        };
        let n = resolve_net_config(&saved, &flags(None, false, true, true)).unwrap();
        assert!(matches!(n.relay, RelayChoice::Disabled));
        assert!(n.lan, "airgap keeps LAN on even with --no-lan");
        assert!(n.airgap);

        // Airgap via persisted config, no flags.
        let saved = SessionConfig {
            relay: "default".into(),
            lan: true,
            airgap: true,
            ..SessionConfig::default()
        };
        let n = resolve_net_config(&saved, &flags(None, false, false, false)).unwrap();
        assert!(matches!(n.relay, RelayChoice::Disabled));
        assert!(n.airgap && n.lan);
    }

    #[test]
    fn no_lan_flag_disables_lan_when_not_airgap() {
        let saved = SessionConfig::default();
        let n = resolve_net_config(&saved, &flags(None, false, true, false)).unwrap();
        assert!(!n.lan);
        assert!(!n.airgap);
    }

    #[test]
    fn invalid_relay_flag_is_rejected() {
        let saved = SessionConfig::default();
        assert!(resolve_net_config(&saved, &flags(Some("garbage"), false, false, false)).is_err());
    }

    #[test]
    fn humantime_durations_parse_and_clamp() {
        use crate::consts::{MAX_LEASE_TTL, MIN_LEASE_TTL};
        use std::time::Duration;

        // In-band values parse verbatim.
        assert_eq!(
            parse_clamped_dur("15m", MIN_LEASE_TTL, MAX_LEASE_TTL).unwrap(),
            Duration::from_secs(15 * 60)
        );
        assert_eq!(
            parse_clamped_dur("2h", MIN_LEASE_TTL, MAX_LEASE_TTL).unwrap(),
            Duration::from_secs(2 * 60 * 60)
        );
        // Out-of-band values clamp to the nearest bound.
        assert_eq!(
            parse_clamped_dur("1s", MIN_LEASE_TTL, MAX_LEASE_TTL).unwrap(),
            MIN_LEASE_TTL
        );
        assert_eq!(
            parse_clamped_dur("72h", MIN_LEASE_TTL, MAX_LEASE_TTL).unwrap(),
            MAX_LEASE_TTL
        );
        // Garbage is a clear error, not a silent default.
        assert!(parse_clamped_dur("soon", MIN_LEASE_TTL, MAX_LEASE_TTL).is_err());
    }

    #[test]
    fn on_off_parsing_is_liberal_but_strict() {
        assert!(parse_on_off("on").unwrap());
        assert!(parse_on_off("YES").unwrap());
        assert!(!parse_on_off("off").unwrap());
        assert!(!parse_on_off("0").unwrap());
        assert!(parse_on_off("maybe").is_err());
    }
}
