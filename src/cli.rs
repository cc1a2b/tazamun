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
use crate::registry::{Registry, SessionKind};
use crate::session::{self, AddrWire, Ticket};
use crate::state::{AppState, NodeRole, encode_hex32};
use crate::supervisor::{self, ControlRequest};
use crate::ui::progress::Ui;

#[derive(Debug, Parser)]
#[command(
    name = "tazamun",
    version = env!("TAZAMUN_VERSION"),
    about = "Tazamun — strict-checkout P2P folder sync. No server ever reads your files."
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
    /// With no subcommand, `tazamun` opens the Home screen (greeting + every
    /// session on this device).
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

/// Network preference flags shared by `init`, `join`, and `start`.
/// At `init`/`join` these are persisted into `state.json`; at `start` they are
/// per-run overrides on top of the persisted config.
#[derive(Debug, Default, Clone, clap::Args)]
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
    /// Turn this folder into a new session and print an invite
    Init,
    /// Join an existing session from an invite ticket
    Join { ticket: String },
    /// Rotate the session key to revoke a member
    ///
    /// The only honest revocation in a shared-secret mesh. Run it (daemon
    /// stopped) to mint a new key + invite, hand that invite to every member you
    /// keep, and have each run `tazamun rekey --accept <ticket>`. Whoever is not
    /// given the new invite is locked out. See also the per-machine accept form.
    Rekey {
        /// Accept a rekey: adopt the new session key from `<ticket>` in place,
        /// keeping this folder's files and history. Run on each kept machine.
        #[arg(long, value_name = "TICKET")]
        accept: Option<String>,
    },
    /// Run the sync daemon in the foreground
    ///
    /// Network preferences come from `state.json`; the global
    /// `--relay/--no-relay/--no-lan/--airgap` flags override them for this run
    /// only.
    Start {
        /// Also write the rotated `.tazamun/logs/daemon.log` regardless of
        /// whether stdout is a terminal. Set by `service install` so
        /// unattended daemons log deterministically (a Windows Scheduled Task's
        /// hidden host still hands the child a console, so TTY detection alone
        /// is not enough). Hidden from the normal help.
        #[arg(long, hide = true)]
        log_file: bool,
        /// Host every registered session in this one process (the multi-session
        /// supervisor). Ignores `--dir`; paused folders are skipped. This is
        /// what the device-wide service runs.
        #[arg(long)]
        all: bool,
    },
    /// Show or change this session's saved network preferences
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Show members, connection health, leases and transfers
    Status {
        /// Live-refreshing panel (1s); press q or Ctrl-C to exit.
        #[arg(long, conflicts_with = "json")]
        watch: bool,
        /// Emit the full telemetry snapshot as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print a fresh invite ticket
    ///
    /// On a v2 session you can mint role-scoped, expiring invites.
    Invite {
        /// Also render the ticket as a scannable QR code.
        #[arg(long)]
        qr: bool,
        /// Role this invite confers: `editor` (default) · `viewer` · `archive`.
        /// A viewer/archive invite cannot lock or publish, and — because it
        /// omits the admin key — cannot be forged into an editor invite.
        #[arg(long)]
        role: Option<String>,
        /// Make the invite expire after this long unused (e.g. `1h`, `30m`,
        /// `7d`). Default: never expires.
        #[arg(long, value_name = "DURATION")]
        ttl: Option<String>,
    },
    /// One-shot NAT and environment health report
    Doctor {
        /// Emit the report as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install or manage the background service
    ///
    /// A systemd user unit on Linux, a LaunchAgent on macOS, a logon Scheduled
    /// Task on Windows.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// List active leases: holder, age, expiry countdown
    Locks,
    /// Take an exclusive lease and make the file writable
    Lock {
        path: String,
        /// If the path is already held, wait for it to free and auto-acquire.
        #[arg(long)]
        wait: bool,
    },
    /// Publish your edits and release the lease
    Unlock { path: String },
    /// List kept versions of a path, with tags, pins and disk usage
    Versions { path: String },
    /// Restore version N of a path (needs a held lease)
    Restore { path: String, n: usize },
    /// Name a kept version so you can find it later
    Tag {
        path: String,
        n: usize,
        name: String,
    },
    /// Remove the name from version N of a path
    Untag { path: String, n: usize },
    /// Pin version N so it is never pruned
    Pin { path: String, n: usize },
    /// Unpin version N, making it prunable again
    Unpin { path: String, n: usize },
    /// Diff a file against a kept version
    ///
    /// Chunk-aware and honest about binaries: percent changed, bytes that would
    /// transfer, chunk counts. Compares against version 0 by default.
    Diff {
        path: String,
        #[arg(default_value_t = 0)]
        n: usize,
    },
    /// List and resolve conflicts
    ///
    /// The conflict center: list quarantined copies with why and when, resolve
    /// one (keep mine / keep theirs / keep both), or prune old copies — always
    /// explicitly, never automatically.
    Conflicts {
        #[command(subcommand)]
        cmd: Option<ConflictsCmd>,
    },
    /// Read this folder's append-only audit log
    ///
    /// Who locked, unlocked, published or restored what, when, and from which
    /// peer. Works offline.
    Log {
        /// Only events for this exact relative path.
        #[arg(long)]
        path: Option<String>,
        /// Only events involving a peer whose id starts with this.
        #[arg(long)]
        peer: Option<String>,
        /// Only events within this recent window (e.g. `2h`, `7d`).
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
        /// Only these event kinds (repeatable), e.g. `--kind lock --kind unlock`.
        #[arg(long)]
        kind: Vec<String>,
        /// Follow the log, printing new events as they happen (Ctrl-C to stop).
        #[arg(long)]
        follow: bool,
        /// Machine-readable JSON, one event per line.
        #[arg(long)]
        json: bool,
    },
    /// Delete unreferenced blobs from the local store
    Gc,
    /// Open the local web dashboard in your browser
    ///
    /// Loopback only, served by the daemon — nothing binds until you run this.
    Dashboard {
        /// Port to open in the URL (defaults to the daemon's dashboard port).
        #[arg(long)]
        port: Option<u16>,
        /// Print the URL but do not launch a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Open the desktop app
    ///
    /// A real window on Windows, macOS and Linux that manages every session on
    /// this machine — overview, files and locks, conflicts, audit, peers,
    /// settings, invites, and start/stop/pause/resume/init/join. Built into
    /// this binary: no browser, no webview, no extra install.
    Gui,
    /// Open the interactive settings panel for this folder
    ///
    /// Role (editor/viewer/archive), strict or easy editing, network, leases,
    /// dashboard — arrow keys, presets, preview-before-save.
    Setup,
    /// Manage every session on this device
    ///
    /// List, open, copy invite, remove. Interactive on a terminal; a plain
    /// overview otherwise.
    Sessions,
    /// List every session with live state
    ///
    /// Running, paused, files, peers and pending transfers. `--json` for
    /// scripts.
    Ls {
        /// Emit the device-wide session table as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show this session's peers, or give them friendly names
    ///
    /// Bare `peers` lists them; `peers name <id> <label>` labels one (the id
    /// may be a short prefix); `peers rm <id>` clears the label.
    Peers {
        #[command(subcommand)]
        cmd: Option<PeersCmd>,
    },
    /// Pause this session without unregistering it
    ///
    /// It stops syncing and, under the supervisor, is shut down live — but
    /// stays registered. Nothing is deleted; `resume` re-hosts it.
    Pause,
    /// Resume a paused session
    ///
    /// Clears the pause and, under a running supervisor, brings the session
    /// back up immediately.
    Resume,
    /// Print a shell completion script
    ///
    /// Supports bash, zsh, fish, powershell and elvish.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
    /// Print the man page to stdout
    Man,
    /// Send a file or folder to one person, no session needed
    ///
    /// No session, no daemon. Prints a single-use, expiring `tzs1…` ticket to
    /// run `tazamun receive` on.
    Send {
        /// The file or folder to send.
        path: PathBuf,
        /// Also render the ticket as a scannable QR code.
        #[arg(long)]
        qr: bool,
        /// How long the ticket stays valid unused (e.g. 30m, 2h; default 10m).
        #[arg(long, value_name = "DURATION")]
        ttl: Option<String>,
    },
    /// Receive a one-shot transfer from a `tzs1…` ticket
    ///
    /// Lands in `--dir`, the current directory by default.
    Receive {
        /// The `tzs1…` ticket printed by `tazamun send`.
        ticket: String,
    },
    /// Update tazamun to the latest release
    ///
    /// Downloads the build for this platform and self-replaces the running
    /// binary.
    Update {
        /// Only report whether a newer version exists; do not install anything.
        #[arg(long)]
        check: bool,
        /// Install this exact release tag (e.g. `0.1.1`) instead of the latest.
        #[arg(long, value_name = "VERSION")]
        tag: Option<String>,
        /// Replace the binary without the interactive confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
        /// GitHub token for a private repo or higher rate limits. Falls back to
        /// the GITHUB_TOKEN or GH_TOKEN environment variable.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConflictsCmd {
    /// List every quarantined copy: when, why, size, original path.
    List {
        /// Machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resolve one quarantined copy (id = the listed name or a unique prefix).
    /// Interactive when no `--keep` is given on a terminal.
    Resolve {
        id: String,
        /// `mine` = the quarantined bytes win (lock, republish, discard copy);
        /// `theirs` = the current synced version wins (discard copy);
        /// `both` = restore the copy as a new `…conflict-<ts>` file.
        #[arg(long, value_name = "mine|theirs|both")]
        keep: Option<String>,
        /// For `--keep both`: restore into this relative path instead of the
        /// suggested `…conflict-<ts>` name.
        #[arg(long, value_name = "RELPATH")]
        into: Option<String>,
    },
    /// Delete quarantined copies older than a cutoff. Interactive only, with
    /// a typed confirmation — the Golden Invariant does not get a cron job.
    Prune {
        /// Age cutoff, e.g. `90d`, `6m`, `24h` (humantime).
        #[arg(long, value_name = "DURATION")]
        older_than: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PeersCmd {
    /// Give a peer a friendly name (id may be a short prefix).
    Name { id: String, name: String },
    /// Clear a peer's name (id may be a short prefix).
    Rm { id: String },
}

#[derive(Debug, Subcommand)]
pub enum ServiceCmd {
    /// Install and start the background service. Per-folder by default; `--all`
    /// installs one device-wide supervisor service that hosts every folder.
    Install {
        /// Install the single device-wide supervisor service (`start --all`)
        /// instead of a unit for this one folder.
        #[arg(long)]
        all: bool,
    },
    /// Stop and remove the background service (per-folder, or `--all` for the
    /// device-wide supervisor service).
    Uninstall {
        #[arg(long)]
        all: bool,
    },
    /// Show the service state and the last daemon log lines (per-folder, or
    /// `--all` for the device-wide supervisor service).
    Status {
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print the persisted network preferences.
    Show,
    /// Change a persisted preference. Keys: `relay <default|none|URL>`,
    /// `lan <on|off>`, `airgap <on|off>`, `lease-ttl <90s|15m|2h>`,
    /// `acquire-timeout <8s>`, `autolock <on|off>`, `strict <on|off>`,
    /// `role <editor|viewer|archive>`, `wait-timeout <10m>`,
    /// `dashboard-port <8787>`, `update-channel <stable|beta>`. Takes effect
    /// on next `start` (live keys can also change from the web dashboard).
    /// `strict off` is "easy mode"; `role viewer` makes this folder
    /// sync-and-read-only. `tazamun setup` edits all of this interactively.
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
    // Bare `tazamun` → the Home screen.
    let Some(cmd) = cli.cmd else {
        return crate::home::show_home().await.map_err(CliError::Refused);
    };
    match cmd {
        Cmd::Init => {
            init(&dir)?;
            persist_net_flags(&dir, &net)?;
            // First-run wizard: on an interactive terminal with no explicit
            // network flags, offer the three-question short path (role →
            // editing → network). Enter-Enter-Enter keeps every default, so
            // scripted/non-TTY init is byte-identical to before.
            let no_net_flags = net.relay.is_none() && !net.no_relay && !net.no_lan && !net.airgap;
            if no_net_flags
                && std::io::stdout().is_terminal()
                && std::io::stdin().is_terminal()
                && std::env::var_os("TAZAMUN_NO_WIZARD").is_none()
            {
                let d = dir.clone();
                tokio::task::spawn_blocking(move || crate::setup::run_init_wizard(&d))
                    .await
                    .map_err(|e| CliError::Refused(format!("wizard task failed: {e}")))?
                    .map_err(CliError::Refused)?;
            }
            Ok(())
        }
        Cmd::Join { ticket } => {
            join(&dir, &ticket)?;
            persist_net_flags(&dir, &net)?;
            Ok(())
        }
        Cmd::Rekey { accept } => handle_rekey_cli(&dir, accept).await,
        Cmd::Start { all, .. } => {
            if all {
                supervisor::run(net).await
            } else {
                start(&dir, &net, ui).await
            }
        }
        Cmd::Config { cmd } => handle_config_cli(&dir, cmd),
        Cmd::Status { watch, json } => handle_status_cli(&dir, watch, json).await,
        Cmd::Doctor { json } => handle_doctor_cli(&dir, json).await,
        Cmd::Service { cmd } => handle_service_cli(&dir, cmd).await,
        Cmd::Invite { qr, role, ttl } => {
            // Validate the role early for a friendly error before touching IPC.
            if let Some(r) = &role {
                NodeRole::parse(r).map_err(CliError::Refused)?;
            }
            let ttl_ms = match &ttl {
                Some(s) => Some(
                    humantime::parse_duration(s)
                        .map_err(|e| CliError::Refused(format!("bad --ttl {s:?}: {e}")))?
                        .as_millis() as u64,
                ),
                None => None,
            };
            let data = request(&dir, IpcRequest::Invite { role, ttl_ms }).await?;
            let ticket = data
                .get("ticket")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if qr {
                print_qr_invite(ticket);
            } else {
                let note = data.get("note").and_then(|v| v.as_str()).unwrap_or("");
                println!("Share this ticket to invite someone:\n\n  {ticket}\n");
                if !note.is_empty() {
                    println!("{note}\n");
                }
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
        Cmd::Tag { path, n, name } => {
            request(
                &dir,
                IpcRequest::Tag {
                    path: path.clone(),
                    n,
                    name: Some(name.clone()),
                },
            )
            .await?;
            println!("✔ tagged version {n} of {path} as \"{name}\"");
            Ok(())
        }
        Cmd::Untag { path, n } => {
            request(
                &dir,
                IpcRequest::Tag {
                    path: path.clone(),
                    n,
                    name: None,
                },
            )
            .await?;
            println!("✔ removed the tag from version {n} of {path}");
            Ok(())
        }
        Cmd::Pin { path, n } => {
            request(
                &dir,
                IpcRequest::Pin {
                    path: path.clone(),
                    n,
                    pinned: true,
                },
            )
            .await?;
            println!("✔ pinned version {n} of {path} — it will never be pruned");
            Ok(())
        }
        Cmd::Unpin { path, n } => {
            request(
                &dir,
                IpcRequest::Pin {
                    path: path.clone(),
                    n,
                    pinned: false,
                },
            )
            .await?;
            println!("✔ unpinned version {n} of {path}");
            Ok(())
        }
        Cmd::Diff { path, n } => {
            let data = request(
                &dir,
                IpcRequest::Diff {
                    path: path.clone(),
                    n,
                },
            )
            .await?;
            print_diff(&path, n, &data);
            Ok(())
        }
        Cmd::Gc => {
            request(&dir, IpcRequest::Gc).await?;
            println!("✔ garbage collection finished");
            Ok(())
        }
        Cmd::Dashboard { port, no_open } => handle_dashboard_cli(&dir, port, no_open).await,
        // The native GUI owns the true main thread and builds its own async
        // runtime, so it is intercepted in `main()` before this outer runtime is
        // entered (nesting Tokio runtimes would panic). Reaching here is a no-op.
        Cmd::Gui => Ok(()),
        Cmd::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "tazamun", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Man => {
            use clap::CommandFactory;
            clap_mangen::Man::new(Cli::command())
                .render(&mut std::io::stdout())
                .map_err(|e| CliError::Refused(format!("man page render failed: {e}")))?;
            Ok(())
        }
        Cmd::Setup => handle_setup_cli(&dir).await,
        Cmd::Sessions => crate::home::run_manager().await.map_err(CliError::Refused),
        Cmd::Ls { json } => handle_ls_cli(json).await,
        Cmd::Conflicts { cmd } => handle_conflicts_cli(&dir, cmd).await,
        Cmd::Log {
            path,
            peer,
            since,
            kind,
            follow,
            json,
        } => handle_log_cli(&dir, path, peer, since, kind, follow, json).await,
        Cmd::Peers { cmd } => handle_peers_cli(&dir, cmd).await,
        Cmd::Pause => handle_pause_cli(&dir, true).await,
        Cmd::Resume => handle_pause_cli(&dir, false).await,
        Cmd::Send { path, qr, ttl } => handle_send_cli(path, &net, ttl, qr).await,
        Cmd::Receive { ticket } => handle_receive_cli(&dir, &ticket, &net).await,
        Cmd::Update {
            check,
            tag,
            yes,
            token,
        } => handle_update_cli(&dir, check, tag, yes, token).await,
    }
}

/// Builds a session-less [`NetConfig`] from the global network flags, for the
/// one-shot `send`/`receive` (which have no persisted config to fold onto).
fn net_config_from_flags(flags: &NetFlags) -> Result<NetConfig, CliError> {
    let airgap = flags.airgap;
    let relay = if airgap {
        RelayChoice::Disabled
    } else {
        match (&flags.relay, flags.no_relay) {
            (Some(url), _) => RelayChoice::parse(url).map_err(CliError::Refused)?,
            (None, true) => RelayChoice::Disabled,
            (None, false) => RelayChoice::Default,
        }
    };
    Ok(NetConfig {
        relay,
        lan: airgap || !flags.no_lan,
        airgap,
        test_bind: None,
        test_relay: None,
    })
}

/// `send`: serve one file/folder over an ephemeral endpoint until a receiver
/// completes or the TTL expires. The ticket is printed the moment the
/// endpoint has addresses.
async fn handle_send_cli(
    path: PathBuf,
    flags: &NetFlags,
    ttl: Option<String>,
    qr: bool,
) -> Result<(), CliError> {
    let net = net_config_from_flags(flags)?;
    let ttl = match ttl {
        Some(s) => humantime::parse_duration(s.trim())
            .map_err(|e| CliError::Refused(format!("invalid --ttl {s:?}: {e}")))?,
        None => crate::consts::SEND_TICKET_TTL,
    };
    let abs = std::path::absolute(&path).map_err(crate::state::StateError::Io)?;
    println!(
        "Sharing {} — waiting for a receiver (ticket valid {}, Ctrl-C to cancel):",
        abs.display(),
        humantime::format_duration(ttl)
    );
    let outcome = crate::oneshot::send(&abs, &net, ttl, |ticket| {
        println!("\n  {ticket}\n");
        println!("On the other machine, run:\n  tazamun receive {ticket}\n");
        if qr {
            print_qr_invite(ticket);
        }
    })
    .await
    .map_err(CliError::Refused)?;
    println!(
        "✔ sent {} file(s) ({}) — the ticket is now spent.",
        outcome.files,
        indicatif::HumanBytes(outcome.bytes)
    );
    Ok(())
}

/// `receive`: pull a one-shot transfer named by the ticket into `dir`.
async fn handle_receive_cli(dir: &Path, ticket: &str, flags: &NetFlags) -> Result<(), CliError> {
    let net = net_config_from_flags(flags)?;
    println!("Connecting to the sender…");
    let outcome = crate::oneshot::receive(ticket, dir, &net)
        .await
        .map_err(CliError::Refused)?;
    println!(
        "✔ received {} file(s) ({}) into {}",
        outcome.files,
        indicatif::HumanBytes(outcome.bytes),
        outcome.dest.display()
    );
    Ok(())
}

/// `setup`: run the blocking panel off the runtime; after a save, live-apply
/// whatever the running daemon accepts over IPC and say what needs a restart.
async fn handle_setup_cli(dir: &Path) -> Result<(), CliError> {
    let d = dir.to_path_buf();
    let changed = tokio::task::spawn_blocking(move || crate::setup::run_panel(&d))
        .await
        .map_err(|e| CliError::Refused(format!("setup task failed: {e}")))?
        .map_err(CliError::Refused)?;
    let Some(changed) = changed else {
        println!("setup closed — nothing changed");
        return Ok(());
    };
    println!("✔ saved {} change(s) to state.json", changed.len());
    if !ipc::daemon_alive(dir).await {
        println!("(no daemon running — everything applies on next `tazamun start`)");
        return Ok(());
    }
    let mut restart_needed = Vec::new();
    for (key, value) in &changed {
        match ipc::request(
            dir,
            &IpcRequest::ConfigSet {
                key: key.clone(),
                value: value.clone(),
            },
        )
        .await
        {
            Ok(resp) if resp.ok => println!("  applied live: {key} = {value}"),
            _ => restart_needed.push(key.clone()),
        }
    }
    if !restart_needed.is_empty() {
        println!(
            "  needs a daemon restart: {} (Ctrl-C the running `tazamun start` and start again)",
            restart_needed.join(", ")
        );
    }
    Ok(())
}

/// `dashboard`: fetch the running daemon's loopback port + session token and
/// print (optionally open) the URL. The token rides the URL *fragment*, so it
/// never reaches the server in a request. Requires a running daemon.
async fn handle_dashboard_cli(
    dir: &Path,
    port: Option<u16>,
    no_open: bool,
) -> Result<(), CliError> {
    // The dashboard is started on demand: ask the daemon to bind it (idempotent),
    // then wait for the real bound port before opening. `request` maps a missing
    // daemon to the clean "start the daemon" error.
    let started = request(dir, IpcRequest::DashboardStart).await?;
    let token = started
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    // Poll until the server reports a bound port (it binds within milliseconds);
    // fall back to the configured port after ~2s if the bind is slow or failed.
    let mut daemon_port = started
        .get("port")
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::from(crate::consts::DASHBOARD_PORT)) as u16;
    for _ in 0..40 {
        let info = request(dir, IpcRequest::DashboardInfo).await?;
        let bound = info.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        if bound != 0 {
            daemon_port = bound;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let port = port.unwrap_or(daemon_port);
    let url = format!("http://127.0.0.1:{port}/#{token}");
    println!("Dashboard: {url}");
    println!("(loopback only — the token in the URL authorizes changes; do not share the URL)");
    if no_open {
        println!("(--no-open given; open the URL above manually)");
    } else if let Err(e) = open_browser(&url) {
        eprintln!("could not launch a browser ({e}); open the URL above manually");
    }
    Ok(())
}

/// Launches the platform's default browser at `url`. No dependency: shells out
/// to the OS opener.
pub(crate) fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// `update`: query GitHub releases and, unless `--check`, download the build
/// for this target and self-replace the running binary. self_update's client
/// is blocking, so the work runs off the async runtime via `spawn_blocking`.
async fn handle_update_cli(
    dir: &Path,
    check: bool,
    tag: Option<String>,
    yes: bool,
    token: Option<String>,
) -> Result<(), CliError> {
    // The release channel is a per-folder preference (set in `tazamun setup`);
    // read it best-effort — `update` also works outside any session folder.
    let channel = AppState::load(dir)
        .map(|s| s.config.update_channel)
        .unwrap_or_else(|_| "stable".to_string());
    tokio::task::spawn_blocking(move || self_update_run(check, tag, yes, token, channel))
        .await
        .map_err(|e| CliError::Refused(format!("update task panicked: {e}")))?
        .map_err(CliError::Refused)
}

/// The blocking half of `update`. `--check` reports the latest release without
/// touching the binary; otherwise the matching asset is downloaded and swapped
/// in atomically (self_update handles the Windows "can't replace a running exe"
/// case). Never runs on a tokio worker thread — reqwest's blocking client would
/// panic trying to start a runtime inside one.
fn self_update_run(
    check: bool,
    tag: Option<String>,
    yes: bool,
    token: Option<String>,
    channel: String,
) -> Result<(), String> {
    use self_update::backends::github;
    let current = self_update::cargo_crate_version!();
    // A token lets `update` see a private repo and lifts the anonymous API rate
    // limit; fall back to the usual GitHub CLI / Actions environment variables.
    let token = token
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .filter(|t| !t.is_empty());
    let beta = channel == "beta";

    // The releases list is needed by --check always, and by a beta-channel
    // install (GitHub's "latest" excludes prereleases, so beta resolves the
    // newest release — prerelease or not — from the full list instead).
    let fetch_releases = || -> Result<Vec<self_update::update::Release>, String> {
        let mut rl = github::ReleaseList::configure();
        rl.repo_owner("cc1a2b").repo_name("tazamun");
        if let Some(t) = &token {
            rl.auth_token(t);
        }
        rl.build()
            .map_err(|e| format!("could not configure the release query: {e}"))?
            .fetch()
            .map_err(|e| update_hint(&e.to_string(), token.is_some()))
    };

    if check {
        let releases = fetch_releases()?;
        match releases.first() {
            None => println!("No published releases yet (installed: {current})."),
            Some(latest) => {
                let latest_v = latest.version.trim_start_matches('v');
                if self_update::version::bump_is_greater(current, latest_v).unwrap_or(false) {
                    println!("Update available: {current} → {latest_v} (channel: {channel})");
                    println!("Run `tazamun update` to install it.");
                } else {
                    println!(
                        "Up to date (installed: {current}, latest: {latest_v}, channel: {channel})."
                    );
                }
            }
        }
        return Ok(());
    }

    let _ = yes; // kept for compatibility: updates no longer prompt
    let mut builder = github::Update::configure();
    builder
        .repo_owner("cc1a2b")
        .repo_name("tazamun")
        .bin_name("tazamun")
        // The two archive formats have two different layouts, verified against
        // live release assets: the unix tar.gz nests the binary one directory
        // down in `tazamun-<target>/`, while the Windows zip is flat with
        // `tazamun.exe` at the root. Guessing one shape for both is exactly
        // the bug that shipped in v0.1.0 (the zip lookup died with "specified
        // file not found in archive"). `{{ bin }}` carries the exe suffix.
        .bin_path_in_archive(update_bin_path_in_archive())
        .target(update_target())
        .current_version(current)
        // No questions, no chatter: the surrounding command already says what
        // it found and what it did, and a confirm prompt in the middle of an
        // update is friction with no safety — the swap is atomic and the old
        // binary is what you already had.
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(true);
    if let Some(t) = &token {
        builder.auth_token(t);
    }
    if let Some(v) = tag {
        builder.target_version_tag(&format!("v{}", v.trim_start_matches('v')));
    } else if beta {
        let releases = fetch_releases()?;
        let Some(newest) = releases.first() else {
            println!("No published releases yet (installed: {current}).");
            return Ok(());
        };
        builder.target_version_tag(&format!("v{}", newest.version.trim_start_matches('v')));
    }
    let status = builder
        .build()
        .map_err(|e| format!("update setup failed: {e}"))?
        .update()
        .map_err(|e| update_hint(&e.to_string(), token.is_some()))?;
    if status.updated() {
        println!("\u{2714} updated to {}", status.version());
        println!("Restart any running `tazamun` daemons to pick up the new binary.");
        // A package manager that owns this install keeps its own books; the
        // binary is now newer than the manager believes, and its next
        // operation may quietly roll this file back.
        if let Some((mgr, cmd)) = managed_by() {
            println!(
                "note: this install is managed by {mgr}, whose records still name the old \
                 version — prefer `{cmd}` next time, or a future {mgr} operation may revert it."
            );
        }
    } else {
        println!("Already up to date ({}).", status.version());
    }
    Ok(())
}

/// Which package manager owns the running executable, judged from its path,
/// with the command that updates it properly. `None` for a plain install —
/// the self-updater's home turf.
fn managed_by() -> Option<(&'static str, &'static str)> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    managed_by_path(&exe)
}

/// Split on both separators rather than `components()`: `components()` cannot
/// see the segments of a Windows path on a Unix host, which would make this
/// untestable for the exact layouts users report. A Unix filename containing a
/// literal backslash could over-split, but the cost is at worst one advisory
/// line.
fn managed_by_path(exe: &std::path::Path) -> Option<(&'static str, &'static str)> {
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

/// Where the binary lives inside a dist `.tar.gz`: nested one directory down.
/// Verified against the live v0.1.1 linux asset, and pinned by a test.
const UPDATE_BIN_PATH_TAR: &str = "tazamun-{{ target }}/{{ bin }}";

/// Where the binary lives inside a dist `.zip`: at the root. Verified against
/// the live v0.1.1 windows asset (`LICENSE`, `README.md`, `tazamun.exe`), and
/// pinned by a test.
const UPDATE_BIN_PATH_ZIP: &str = "{{ bin }}";

/// Windows releases ship as flat zips; everything else as nested tarballs.
/// `cfg!` rather than `#[cfg]` so both constants stay compiled (and testable)
/// on every platform.
fn update_bin_path_in_archive() -> &'static str {
    if cfg!(windows) {
        UPDATE_BIN_PATH_ZIP
    } else {
        UPDATE_BIN_PATH_TAR
    }
}

/// The release-asset target this build should update from. Releases ship MSVC
/// on Windows, so a locally cross-built GNU binary must look for the MSVC
/// asset — it graduates to the released ABI on its first update. Every other
/// triple is its own asset name.
fn update_target() -> &'static str {
    normalize_update_target(self_update::get_target())
}

fn normalize_update_target(target: &'static str) -> &'static str {
    match target {
        "x86_64-pc-windows-gnu" => "x86_64-pc-windows-msvc",
        t => t,
    }
}

/// Maps a self_update error string to a message that names the likely cause —
/// a 404 with no token means the repo is private (or has no releases yet).
fn update_hint(err: &str, had_token: bool) -> String {
    if err.contains("404") && !had_token {
        format!(
            "GitHub returned 404 — the repo is private or has no releases yet. \
             Pass a token: `tazamun update --token <TOKEN>` or set GITHUB_TOKEN / GH_TOKEN. ({err})"
        )
    } else {
        format!("could not reach GitHub releases: {err}")
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
    let names = &data["names"];
    let leases = data["leases"].as_array().cloned().unwrap_or_default();
    let waiting = data["waiting"].as_array().cloned().unwrap_or_default();
    if leases.is_empty() {
        println!("no active leases");
    } else {
        println!("active leases ({}):", leases.len());
        for l in &leases {
            let path = l["path"].as_str().unwrap_or("?");
            let mine = l["mine"].as_bool() == Some(true);
            let age = fmt_ms(l["age_ms"].as_u64().unwrap_or(0));
            let left = fmt_ms(l["expires_in_ms"].as_u64().unwrap_or(0));
            let who = if mine {
                "you".to_string()
            } else {
                name_or_short(names, l["holder"].as_str().unwrap_or("?"))
            };
            println!("  {path}  held by {who}  age {age}  expires in {left}");
        }
    }
    if !waiting.is_empty() {
        println!("\nwaiting:");
        for w in &waiting {
            let path = w["path"].as_str().unwrap_or("?");
            let behind = name_or_short(names, w["behind"].as_str().unwrap_or("?"));
            println!("  {path}  (behind {behind})");
        }
    }
    Ok(())
}

/// A peer id resolved to its friendly name (from the status `names` map) or a
/// short hex prefix as a fallback.
fn name_or_short(names: &serde_json::Value, id: &str) -> String {
    names
        .get(id)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| short(id))
}

/// `tazamun log` — reads (and optionally follows) the append-only audit log.
/// Fully offline: it reads `.tazamun/audit.jsonl` directly, no daemon needed.
#[allow(clippy::too_many_arguments)]
async fn handle_log_cli(
    dir: &Path,
    path: Option<String>,
    peer: Option<String>,
    since: Option<String>,
    kind: Vec<String>,
    follow: bool,
    json: bool,
) -> Result<(), CliError> {
    let _ = AppState::load(dir)?;
    let since_ms = match &since {
        Some(s) => {
            let d = humantime::parse_duration(s)
                .map_err(|e| CliError::Refused(format!("bad --since {s:?}: {e}")))?;
            Some(crate::now_ms().saturating_sub(d.as_millis() as u64))
        }
        None => None,
    };
    let filter = crate::audit::Filter {
        path,
        peer,
        since_ms,
        kinds: kind,
    };

    let print = |e: &crate::audit::AuditEvent| {
        if json {
            println!("{}", serde_json::to_string(e).unwrap_or_default());
        } else {
            let when = crate::guard::utc_timestamp(e.ts_ms);
            let who = e.peer.as_deref().map(short).unwrap_or_default();
            let what = e.path.as_deref().unwrap_or("");
            let extra = e
                .detail
                .as_deref()
                .map(|d| format!("  ({d})"))
                .unwrap_or_default();
            println!("{when}  {:<14} {:<28} {}{extra}", e.kind, what, who);
        }
    };

    let initial = crate::audit::read(dir, &filter);
    if initial.is_empty() && !follow {
        println!("no audit events match (the log is on by default; try widening the filters)");
        return Ok(());
    }
    for e in &initial {
        print(e);
    }
    if !follow {
        return Ok(());
    }

    // Follow: poll the file from the current end, printing new complete lines.
    let mut offset = crate::audit::end_offset(dir);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                let (events, new_off) = crate::audit::read_since_offset(dir, offset, &filter);
                offset = new_off;
                for e in &events {
                    print(e);
                }
            }
        }
    }
}

/// `tazamun conflicts` — the conflict center. Listing works offline (it reads
/// the quarantine directly); resolving drives the daemon over IPC so the
/// single-writer discipline holds; pruning is interactive-only with a typed
/// confirmation.
async fn handle_conflicts_cli(dir: &Path, cmd: Option<ConflictsCmd>) -> Result<(), CliError> {
    let _ = AppState::load(dir)?; // must be a session folder
    match cmd.unwrap_or(ConflictsCmd::List { json: false }) {
        ConflictsCmd::List { json } => {
            let entries = crate::conflicts::list(dir);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entries).unwrap_or_default()
                );
                return Ok(());
            }
            if entries.is_empty() {
                println!("no quarantined copies — the conflict center is empty");
                return Ok(());
            }
            let total: u64 = entries.iter().map(|e| e.size).sum();
            println!(
                "{} quarantined cop{} · {} preserved",
                entries.len(),
                if entries.len() == 1 { "y" } else { "ies" },
                crate::state::fmt_size(total)
            );
            let now = crate::now_ms();
            for e in &entries {
                println!(
                    "  {}\n    path {}  ·  why {}  ·  {} ago  ·  {}",
                    e.name,
                    e.path.as_deref().unwrap_or("(unknown — see the log)"),
                    e.reason.as_deref().unwrap_or("unknown"),
                    fmt_ms(now.saturating_sub(e.ts_ms)),
                    crate::state::fmt_size(e.size),
                );
            }
            println!(
                "\nresolve one:  tazamun conflicts resolve <id> --keep mine|theirs|both\n\
                 (id may be a unique prefix of the name above)"
            );
            Ok(())
        }
        ConflictsCmd::Resolve { id, keep, into } => {
            handle_conflict_resolve(dir, &id, keep.as_deref(), into.as_deref()).await
        }
        ConflictsCmd::Prune { older_than } => {
            let dur = humantime::parse_duration(&older_than)
                .map_err(|e| CliError::Refused(format!("bad --older-than {older_than:?}: {e}")))?;
            let entries = crate::conflicts::list(dir);
            let prunable = crate::conflicts::select_prunable(
                &entries,
                crate::now_ms(),
                dur.as_millis() as u64,
            );
            if prunable.is_empty() {
                println!("nothing older than {older_than} — nothing to prune");
                return Ok(());
            }
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                return Err(CliError::Refused(
                    "conflicts prune is interactive only (typed confirmation) — \
                     the Golden Invariant does not get a cron job"
                        .into(),
                ));
            }
            let bytes: u64 = prunable.iter().map(|e| e.size).sum();
            println!(
                "about to DELETE {} quarantined cop{} ({}) older than {older_than}:",
                prunable.len(),
                if prunable.len() == 1 { "y" } else { "ies" },
                crate::state::fmt_size(bytes)
            );
            for e in &prunable {
                println!(
                    "  {}  ({}, {})",
                    e.name,
                    e.path.as_deref().unwrap_or("?"),
                    crate::state::fmt_size(e.size)
                );
            }
            println!("\nThese bytes exist NOWHERE else. This cannot be undone.");
            print!("type `delete` to confirm: ");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(crate::state::StateError::Io)?;
            if line.trim() != "delete" {
                println!("not confirmed — nothing was deleted");
                return Ok(());
            }
            let names: Vec<String> = prunable.iter().map(|e| e.name.clone()).collect();
            let (removed, freed, errors) = crate::conflicts::prune(dir, &names);
            println!(
                "✔ pruned {} cop{} · {} freed",
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" },
                crate::state::fmt_size(freed)
            );
            for e in errors {
                eprintln!("  ! {e}");
            }
            Ok(())
        }
    }
}

/// The guided resolve: fetches the live entry from the daemon, asks (or takes
/// `--keep`), then drives the battle-tested verbs — Lock → ConflictApply →
/// Unlock → ConflictDiscard — so a failure at any step leaves the quarantined
/// copy untouched.
async fn handle_conflict_resolve(
    dir: &Path,
    id: &str,
    keep: Option<&str>,
    into: Option<&str>,
) -> Result<(), CliError> {
    // Resolve the id against the UNCAPPED local quarantine (the daemon's status
    // snapshot caps at CONFLICTS_LIST_MAX, so an old conflict past the cap must
    // still be resolvable — a reviewer caught this trap).
    let entries = crate::conflicts::list(dir);
    let entry = crate::conflicts::resolve_id(&entries, id).map_err(CliError::Refused)?;
    let name = entry.name.clone();
    let path = entry.path.clone();
    let reason = entry.reason.clone().unwrap_or_else(|| "unknown".into());
    let size = entry.size;
    // Suggest a collision-free keep-both name, checking live indexed files.
    let both_default = path.as_deref().map(|p| {
        let files = AppState::load(dir).ok();
        crate::conflicts::both_name(p, crate::now_ms(), |c| {
            files
                .as_ref()
                .and_then(|s| {
                    crate::sync::index::sanitize_rel_path(c)
                        .ok()
                        .map(|r| s.files.get(&r).is_some_and(|f| !f.deleted))
                })
                .unwrap_or(false)
        })
    });

    println!("conflict  {name}");
    println!(
        "  path {}  ·  why {reason}  ·  {}",
        path.as_deref().unwrap_or("(unknown)"),
        crate::state::fmt_size(size)
    );

    let choice = match keep {
        Some(k @ ("mine" | "theirs" | "both")) => k.to_string(),
        Some(other) => {
            return Err(CliError::Refused(format!(
                "--keep must be mine, theirs, or both (got {other:?})"
            )));
        }
        None => {
            if !std::io::stdin().is_terminal() {
                return Err(CliError::Refused(
                    "no --keep given and no terminal to ask on (use --keep mine|theirs|both)"
                        .into(),
                ));
            }
            println!("\n  [m]ine   — these quarantined bytes win: lock, republish, discard copy");
            println!("  [t]heirs — the current synced version wins: discard this copy");
            println!("  [b]oth   — restore this copy as a new conflict-named file");
            print!("keep which? [m/t/b, anything else cancels] ");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(crate::state::StateError::Io)?;
            match line.trim().to_ascii_lowercase().as_str() {
                "m" | "mine" => "mine".into(),
                "t" | "theirs" => "theirs".into(),
                "b" | "both" => "both".into(),
                _ => {
                    println!("cancelled — nothing changed");
                    return Ok(());
                }
            }
        }
    };

    match choice.as_str() {
        "theirs" => {
            let d = request(dir, IpcRequest::ConflictDiscard { id: name.clone() }).await?;
            println!(
                "✔ kept the synced version; discarded the quarantined copy ({} freed)",
                crate::state::fmt_size(d["size"].as_u64().unwrap_or(0))
            );
            Ok(())
        }
        "mine" | "both" => {
            let target = if choice == "mine" {
                path.clone().ok_or_else(|| {
                    CliError::Refused(
                        "this copy's original path is unknown (legacy entry) — \
                         use `--keep both --into <path>` to restore it under a name you choose"
                            .into(),
                    )
                })?
            } else {
                match into.map(str::to_string).or(both_default) {
                    Some(t) => t,
                    None => {
                        return Err(CliError::Refused(
                            "no target name available — pass `--into <relpath>`".into(),
                        ));
                    }
                }
            };
            println!("→ locking {target} …");
            request(
                dir,
                IpcRequest::Lock {
                    path: target.clone(),
                },
            )
            .await?;
            println!("→ writing the quarantined bytes …");
            if let Err(e) = request(
                dir,
                IpcRequest::ConflictApply {
                    id: name.clone(),
                    target: target.clone(),
                },
            )
            .await
            {
                // Leave nothing half-done: release the lease, keep the copy.
                let _ = request(
                    dir,
                    IpcRequest::Unlock {
                        path: target.clone(),
                    },
                )
                .await;
                eprintln!("✗ apply failed; the quarantined copy is untouched");
                return Err(e);
            }
            println!("→ publishing (unlock) …");
            request(
                dir,
                IpcRequest::Unlock {
                    path: target.clone(),
                },
            )
            .await?;
            let d = request(dir, IpcRequest::ConflictDiscard { id: name }).await?;
            println!(
                "✔ resolved: {target} now carries the quarantined bytes (copy discarded, {} freed)",
                crate::state::fmt_size(d["size"].as_u64().unwrap_or(0))
            );
            Ok(())
        }
        _ => unreachable!("choice is validated above"),
    }
}

/// `tazamun peers` — list peers, or name/clear-name one via the daemon.
async fn handle_peers_cli(dir: &Path, cmd: Option<PeersCmd>) -> Result<(), CliError> {
    match cmd {
        Some(PeersCmd::Name { id, name }) => {
            let data = request(
                dir,
                IpcRequest::PeerName {
                    id,
                    name: Some(name),
                },
            )
            .await?;
            println!(
                "✔ named {} → {}",
                short(data["id"].as_str().unwrap_or("?")),
                data["name"].as_str().unwrap_or("?")
            );
            Ok(())
        }
        Some(PeersCmd::Rm { id }) => {
            let data = request(dir, IpcRequest::PeerName { id, name: None }).await?;
            let id = data["id"].as_str().unwrap_or("?");
            if data["cleared"].as_bool() == Some(true) {
                println!("✔ cleared the name for {}", short(id));
            } else {
                println!("{} had no name", short(id));
            }
            Ok(())
        }
        None => {
            let data = request(dir, IpcRequest::Status).await?;
            let members = data["members"].as_array().cloned().unwrap_or_default();
            if members.is_empty() {
                println!("no peers known yet — share an invite to add one");
                return Ok(());
            }
            use console::style;
            println!("peers ({}):", members.len());
            for m in &members {
                let id = m["id"].as_str().unwrap_or("?");
                let online = m["online"].as_bool() == Some(true);
                let dot = if online {
                    style("●").green()
                } else {
                    style("○").dim()
                };
                let label = match m["name"].as_str() {
                    Some(n) => format!("{n}  ({})", short(id)),
                    None => short(id),
                };
                let conn = m["conn"].as_str().unwrap_or("None");
                let grade = m["grade"].as_str().unwrap_or("Offline");
                println!("  {dot} {label:<28} {conn}  {}", style(grade).dim());
            }
            println!(
                "\nname one:  tazamun peers name {} \"render-box\"",
                members[0]["id"]
                    .as_str()
                    .map(short)
                    .unwrap_or_else(|| "<id>".into())
            );
            Ok(())
        }
    }
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
    // Refuse before writing any session state if the daemon could never run
    // here. Otherwise `init` succeeds, mints a real invite, and only `start`
    // discovers the folder is unusable — the confusing sequence a WSL user on
    // a /mnt drive hits every time.
    if let Err(e) = crate::ipc::probe_can_host(dir) {
        return Err(CliError::Refused(e.to_string()));
    }
    let secret_key = iroh::SecretKey::generate();
    let session_secret: [u8; 32] = rand::random();
    // P17: the founder is the session admin/editor. Generate the admin Ed25519
    // keypair (the root of role enforcement) and a self-signed, never-expiring
    // editor grant this node presents to peers.
    let admin = iroh::SecretKey::generate();
    let admin_pub = *admin.public().as_bytes();
    let now = crate::now_ms();
    let mut state = AppState::new(
        encode_hex32(&secret_key.to_bytes()),
        encode_hex32(&session_secret),
    );
    state.admin_public_key = Some(encode_hex32(&admin_pub));
    state.admin_secret_key = Some(encode_hex32(&admin.to_bytes()));
    state.my_grant = Some(session::sign_grant(
        &admin,
        session::Grant {
            role: session::ROLE_EDITOR,
            invite_id: rand::random(),
            issued_ms: now,
            expiry_ms: 0,
        },
    ));
    state.save(dir)?;
    let me = secret_key.public();
    // The shareable ticket is a v2 editor invite (carries the admin secret so
    // the first collaborator is a co-editor who can invite in turn).
    let ticket = session::mint_ticket(
        session_secret,
        Some((&admin, admin_pub)),
        session::ROLE_EDITOR,
        rand::random(),
        now,
        0,
        vec![AddrWire {
            id: *me.as_bytes(),
            relay: None,
            direct: vec![],
        }],
    );
    crate::registry::register_session(dir, crate::registry::SessionKind::Init);
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
    // P17: adopt a v2 invite's role. The grant is verified against the admin
    // public key the ticket carries; an expired grant is refused up front.
    let mut joined_role = NodeRole::Editor;
    if let (Some(admin_public), Some(grant)) = (&ticket.admin_public, &ticket.grant) {
        if !grant.verify(admin_public) {
            return Err(CliError::Refused(
                "invite grant failed signature verification (tampered or wrong session)".into(),
            ));
        }
        if grant.grant.is_expired(crate::now_ms()) {
            return Err(CliError::Refused(
                "this invite has expired — ask for a fresh one".into(),
            ));
        }
        joined_role = NodeRole::from_code(grant.grant.role);
        state.admin_public_key = Some(encode_hex32(admin_public));
        // Editor invites carry the admin secret so the joinee can invite too;
        // viewer/archive invites do not (they cannot sign grants).
        state.admin_secret_key = ticket.admin_secret.as_ref().map(|s| encode_hex32(&s.0));
        state.my_grant = Some(grant.clone());
        state.config.role = joined_role;
    }
    for addr in &ticket.bootstrap {
        if addr.id != *me.as_bytes()
            && let Some(id) = addr.endpoint_id()
        {
            state.known_members.insert(id.to_string(), addr.clone());
        }
    }
    state.save(dir)?;
    crate::registry::register_session(dir, crate::registry::SessionKind::Join);
    println!("Joined tazamun session");
    println!("  folder : {}", dir.display());
    println!("  peer id: {me}");
    if state.enforcing_roles() {
        println!("  role   : {}", joined_role.as_str());
    }
    println!("\nRun `tazamun start` to begin syncing.");
    Ok(())
}

/// `tazamun rekey` — rotate the session key (revocation), or `--accept` a
/// rotation. Requires the daemon stopped: it swaps the cryptographic root of the
/// session, keeping this folder's files, history, and config untouched.
async fn handle_rekey_cli(dir: &Path, accept: Option<String>) -> Result<(), CliError> {
    if ipc::daemon_alive(dir).await {
        return Err(CliError::Refused(
            "stop the daemon first — rekey rotates the session key (Ctrl-C on `tazamun start`)"
                .into(),
        ));
    }
    let mut state = AppState::load(dir)?;
    match accept {
        Some(ticket_str) => {
            let ticket = Ticket::decode(&ticket_str)?;
            let role = rekey_adopt(&mut state, &ticket, crate::now_ms())?;
            state.save(dir)?;
            println!(
                "✔ rekeyed: this folder now uses the new session key (role: {}).",
                role.as_str()
            );
            println!("Your files and history are untouched. Run `tazamun start` to reconnect.");
            println!("Anyone still on the old key can no longer connect.");
            Ok(())
        }
        None => {
            let ticket = rekey_rotate(&mut state, crate::now_ms())?;
            state.save(dir)?;
            println!("✔ rekeyed: rotated the session key and the admin key.");
            println!(
                "\nHand this new invite to every member you KEEP:\n\n  {}\n",
                ticket.encode()
            );
            println!("On each kept machine (daemon stopped):  tazamun rekey --accept <ticket>");
            println!(
                "Then everyone runs `tazamun start`. Anyone NOT given this invite is now locked out."
            );
            Ok(())
        }
    }
}

/// Rotates the session key in place (the revoking admin's side of rekey):
/// a fresh session secret, a fresh admin keypair, a fresh self editor grant, and
/// a cleared address book. Files, history, and config are untouched. Returns the
/// new editor invite to hand to kept members. Pure over `state` (no I/O).
pub(crate) fn rekey_rotate(state: &mut AppState, now_ms: u64) -> Result<Ticket, CliError> {
    if state.admin_secret_key().is_none() {
        return Err(CliError::Refused(
            "this session predates roles (no admin key), so there is nothing to rotate; \
             rekey needs a session created with a roles-capable build"
                .into(),
        ));
    }
    let new_secret: [u8; 32] = rand::random();
    let admin = iroh::SecretKey::generate();
    let admin_pub = *admin.public().as_bytes();
    state.session_secret = encode_hex32(&new_secret);
    state.admin_public_key = Some(encode_hex32(&admin_pub));
    state.admin_secret_key = Some(encode_hex32(&admin.to_bytes()));
    state.my_grant = Some(session::sign_grant(
        &admin,
        session::Grant {
            role: session::ROLE_EDITOR,
            invite_id: rand::random(),
            issued_ms: now_ms,
            expiry_ms: 0,
        },
    ));
    state.known_members.clear();
    let me = iroh::SecretKey::from_bytes(&my_endpoint_id(state)?).public();
    Ok(session::mint_ticket(
        new_secret,
        Some((&admin, admin_pub)),
        session::ROLE_EDITOR,
        rand::random(),
        now_ms,
        0,
        vec![AddrWire {
            id: *me.as_bytes(),
            relay: None,
            direct: vec![],
        }],
    ))
}

/// Adopts a rotation on a kept machine: verifies the new invite's grant, swaps
/// the session secret + admin keys + role in place, and reseeds the address book
/// from the rotation's bootstrap. Files/history/config untouched. Returns the
/// adopted role. Pure over `state` (no I/O).
pub(crate) fn rekey_adopt(
    state: &mut AppState,
    ticket: &Ticket,
    now_ms: u64,
) -> Result<NodeRole, CliError> {
    let (Some(admin_public), Some(grant)) = (&ticket.admin_public, &ticket.grant) else {
        return Err(CliError::Refused(
            "rekey --accept needs a v2 invite minted by `tazamun rekey`".into(),
        ));
    };
    if !grant.verify(admin_public) {
        return Err(CliError::Refused(
            "the rekey invite failed signature verification (tampered or wrong session)".into(),
        ));
    }
    if grant.grant.is_expired(now_ms) {
        return Err(CliError::Refused(
            "this rekey invite has expired — ask for a fresh one".into(),
        ));
    }
    let role = NodeRole::from_code(grant.grant.role);
    let my_id = my_endpoint_id(state)?;
    state.session_secret = encode_hex32(&ticket.secret.0);
    state.admin_public_key = Some(encode_hex32(admin_public));
    state.admin_secret_key = ticket.admin_secret.as_ref().map(|s| encode_hex32(&s.0));
    state.my_grant = Some(grant.clone());
    state.config.role = role;
    state.known_members.clear();
    for addr in &ticket.bootstrap {
        if addr.id != my_id
            && let Some(id) = addr.endpoint_id()
        {
            state.known_members.insert(id.to_string(), addr.clone());
        }
    }
    Ok(role)
}

/// This node's endpoint id bytes, from its persisted iroh secret key.
fn my_endpoint_id(state: &AppState) -> Result<[u8; 32], CliError> {
    let sk = crate::state::decode_hex32(&state.iroh_secret_key)
        .ok_or_else(|| CliError::Refused("state has no valid endpoint key".into()))?;
    Ok(*iroh::SecretKey::from_bytes(&sk).public().as_bytes())
}

/// Folds per-run [`NetFlags`] onto the persisted [`SessionConfig`] (flag >
/// config > default) into the runtime [`NetConfig`].
pub(crate) fn resolve_net_config(
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
    println!("  log    : {}", crate::state::log_file_path(dir).display());
    if airgap {
        println!("  mode   : AIRGAP — no relays, no external discovery, LAN only");
    }
    if !saved.strict {
        println!(
            "  edit   : EASY MODE — files stay writable; un-leased edits auto-publish (conflicts quarantined)"
        );
    }
    if !saved.role.can_edit() {
        println!(
            "  role   : {} — this folder syncs and reads but never locks or publishes",
            saved.role.as_str().to_uppercase()
        );
    }
    println!("\nPress Ctrl-C to stop. Use `tazamun status` from another shell.");
    tokio::select! {
        sig = tokio::signal::ctrl_c() => {
            match sig {
                Ok(()) => println!("\nStopping: releasing leases and saying goodbye…"),
                Err(e) => eprintln!("signal handler failed ({e}); shutting down"),
            }
        }
        // P21: the daemon can also be stopped remotely (the GUI's Stop button
        // sends an IPC Shutdown); the actor exits on its own and we follow.
        _ = handle.wait_shutdown() => {
            println!("\nStopped by request (IPC shutdown).");
        }
    }
    // Safe either way: if the actor already exited, this degrades to a join.
    handle.shutdown().await;
    println!("Stopped cleanly.");
    Ok(())
}

/// One device-wide session row for `tazamun ls`.
struct LsRow {
    path: String,
    kind: SessionKind,
    paused: bool,
    running: bool,
    supervised: bool,
    files: usize,
    peers_online: usize,
    peers_total: usize,
    pending: usize,
    readable: bool,
}

/// `tazamun ls` — the device-wide session table. Works with or without a
/// supervisor: it scans the registry and queries each folder's own socket, then
/// annotates which folders a running supervisor is hosting.
async fn handle_ls_cli(json: bool) -> Result<(), CliError> {
    let mut reg = Registry::load();
    let pruned = reg.prune(AppState::exists);
    if !pruned.is_empty() {
        let _ = reg.save();
    }
    // Best-effort: which folders is a live supervisor hosting?
    let supervised: std::collections::HashSet<String> =
        match supervisor::request(&ControlRequest::List, std::time::Duration::from_secs(2)).await {
            Ok(r) if r.ok => r
                .data
                .as_ref()
                .and_then(|d| d.get("hosted"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            _ => std::collections::HashSet::new(),
        };

    let mut rows = Vec::with_capacity(reg.sessions.len());
    for s in &reg.sessions {
        let dir = PathBuf::from(&s.path);
        let (files, readable) = match AppState::load(&dir) {
            Ok(st) => (st.files.values().filter(|f| !f.deleted).count(), true),
            Err(_) => (0, false),
        };
        let running = ipc::daemon_alive(&dir).await;
        let (peers_online, peers_total, pending) = if running {
            match ipc::request(&dir, &IpcRequest::Status).await {
                Ok(r) if r.ok => parse_status_counts(r.data.as_ref()),
                _ => (0, 0, 0),
            }
        } else {
            (0, 0, 0)
        };
        rows.push(LsRow {
            path: s.path.clone(),
            kind: s.kind,
            paused: s.paused,
            running,
            supervised: supervised.contains(&s.path),
            files,
            peers_online,
            peers_total,
            pending,
            readable,
        });
    }

    if json {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.path,
                    "kind": r.kind.as_str(),
                    "paused": r.paused,
                    "running": r.running,
                    "supervised": r.supervised,
                    "files": r.files,
                    "peers_online": r.peers_online,
                    "peers_total": r.peers_total,
                    "pending_pulls": r.pending,
                    "readable": r.readable,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
        return Ok(());
    }

    use console::style;
    if rows.is_empty() {
        println!("No sessions registered on this device.");
        println!("  create one:  tazamun init      join one:  tazamun join tzm1…");
        return Ok(());
    }
    let running = rows.iter().filter(|r| r.running).count();
    let paused = rows.iter().filter(|r| r.paused).count();
    let supervised_up = rows.iter().any(|r| r.supervised);
    println!(
        "{} session(s) · {running} running{}{}",
        rows.len(),
        if paused > 0 {
            format!(" · {paused} paused")
        } else {
            String::new()
        },
        if supervised_up {
            " · supervisor up".to_string()
        } else {
            String::new()
        }
    );
    for r in &rows {
        let dot = if r.paused {
            style("⏸").yellow()
        } else if r.running {
            style("●").green()
        } else if r.readable {
            style("○").dim()
        } else {
            style("⚠").red()
        };
        let state = if !r.readable {
            "unreadable".to_string()
        } else if r.paused {
            "paused".to_string()
        } else if r.running {
            format!(
                "running · {} files · peers {}/{}{}",
                r.files,
                r.peers_online,
                r.peers_total,
                if r.pending > 0 {
                    format!(" · {} pulling", r.pending)
                } else {
                    String::new()
                }
            )
        } else {
            format!("stopped · {} files", r.files)
        };
        let tag = if r.supervised { " [supervised]" } else { "" };
        println!(
            "  {dot} {:<40} {}{}",
            r.path,
            style(state).dim(),
            style(tag).cyan()
        );
    }
    Ok(())
}

/// Extracts (peers_online, peers_total, pending_pulls) from a `status` payload.
fn parse_status_counts(data: Option<&serde_json::Value>) -> (usize, usize, usize) {
    let Some(data) = data else {
        return (0, 0, 0);
    };
    let members = data.get("members").and_then(|v| v.as_array());
    let total = members.map(|m| m.len()).unwrap_or(0);
    let online = members
        .map(|m| {
            m.iter()
                .filter(|x| {
                    x.get("online")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    let pending = data
        .get("pending_pulls")
        .and_then(|v| v.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    (online, total, pending)
}

/// `tazamun pause` / `resume` on `--dir`. Ensures the folder is registered,
/// then either drives a running supervisor live (which persists the flag and
/// stops/starts the session) or, with no supervisor, just persists the flag.
async fn handle_pause_cli(dir: &Path, pause: bool) -> Result<(), CliError> {
    // Must be a real session folder.
    let _ = AppState::load(dir)?;
    ensure_registered(dir);
    let path = dir.to_string_lossy().to_string();

    if supervisor::control_alive().await {
        let req = if pause {
            ControlRequest::Pause { path: path.clone() }
        } else {
            ControlRequest::Resume { path: path.clone() }
        };
        let resp = supervisor::request(&req, std::time::Duration::from_secs(30)).await?;
        if !resp.ok {
            let (code, message) = resp
                .error
                .map(|e| (e.code, e.message))
                .unwrap_or_else(|| ("error".into(), "supervisor refused".into()));
            return Err(CliError::DaemonRefused { code, message });
        }
        if pause {
            println!("✔ paused {path} — the supervisor stopped it (registered; not deleted)");
        } else {
            println!("✔ resumed {path} — the supervisor is hosting it again");
        }
        return Ok(());
    }

    // No supervisor: persist the flag; the change takes effect on `start --all`.
    let mut reg = Registry::load();
    let _ = reg.set_paused(dir, pause);
    let _ = reg.save();
    if pause {
        println!("✔ paused {path} (registered; not deleted)");
        println!(
            "  no supervisor is running, so nothing was stopped now — `tazamun start --all` will skip it."
        );
        if ipc::daemon_alive(dir).await {
            println!("  it is currently running standalone; stop that daemon to pause it now.");
        }
    } else {
        println!("✔ resumed {path}");
        println!("  start it with:  tazamun --dir {path} start   (or `tazamun start --all`)");
    }
    Ok(())
}

/// Ensures `dir` has a registry entry so pause/resume can address it (a session
/// created before the registry existed, or after a lost registry, otherwise
/// could not be paused). Best-effort and idempotent.
fn ensure_registered(dir: &Path) {
    let abs = std::path::absolute(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    let mut reg = Registry::load();
    if !reg.sessions.iter().any(|s| s.path == abs) {
        reg.register(dir, SessionKind::Init, crate::now_ms());
        let _ = reg.save();
    }
}

/// `config show|set` — reads/writes the persisted per-session preferences.
/// `service install|uninstall|status` over the OS-native backend, with the
/// rotated daemon-log tail appended to `status`.
async fn handle_service_cli(dir: &Path, cmd: ServiceCmd) -> Result<(), CliError> {
    match cmd {
        ServiceCmd::Install { all: true } => {
            let msg = crate::service::install_supervisor()
                .map_err(|e| CliError::Refused(e.to_string()))?;
            println!("✔ {msg}");
            println!(
                "  migrate: to retire per-folder units, run `tazamun --dir <folder> service uninstall` for each."
            );
            Ok(())
        }
        ServiceCmd::Uninstall { all: true } => {
            let msg = crate::service::uninstall_supervisor()
                .map_err(|e| CliError::Refused(e.to_string()))?;
            println!("✔ {msg}");
            Ok(())
        }
        ServiceCmd::Status { all: true } => {
            println!(
                "service  : {} (device-wide supervisor)",
                crate::service::supervisor_name()
            );
            let platform = crate::service::supervisor_status()
                .map_err(|e| CliError::Refused(e.to_string()))?;
            for line in platform.lines() {
                println!("  {line}");
            }
            println!(
                "control  : {}",
                if crate::supervisor::control_alive().await {
                    "supervisor responding"
                } else {
                    "not responding"
                }
            );
            Ok(())
        }
        // Per-folder (default): a session must exist before a service is pinned.
        ServiceCmd::Install { all: false } => {
            let _ = AppState::load(dir)?;
            let msg = crate::service::install(dir).map_err(|e| CliError::Refused(e.to_string()))?;
            println!("✔ {msg}");
            Ok(())
        }
        ServiceCmd::Uninstall { all: false } => {
            let _ = AppState::load(dir)?;
            let msg =
                crate::service::uninstall(dir).map_err(|e| CliError::Refused(e.to_string()))?;
            println!("✔ {msg}");
            Ok(())
        }
        ServiceCmd::Status { all: false } => {
            let _ = AppState::load(dir)?;
            let name = crate::service::instance_name(dir);
            println!("service  : {name}");
            let platform = crate::service::platform_status(dir)
                .map_err(|e| CliError::Refused(e.to_string()))?;
            for line in platform.lines() {
                println!("  {line}");
            }
            println!(
                "daemon   : {}",
                if ipc::daemon_alive(dir).await {
                    "responding over IPC"
                } else {
                    "not responding"
                }
            );
            match crate::service::log_tail(dir, 5) {
                Some(lines) if !lines.is_empty() => {
                    println!("last log lines:");
                    for l in lines {
                        println!("  {l}");
                    }
                }
                _ => println!("last log lines: (no daemon.log yet)"),
            }
            Ok(())
        }
    }
}

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
            println!(
                "  strict          : {} ({})",
                on_off(c.strict),
                if c.strict {
                    "exclusive checkout"
                } else {
                    "easy mode — edit in place"
                }
            );
            println!(
                "  role            : {} ({})",
                c.role.as_str(),
                match c.role {
                    crate::state::NodeRole::Editor => "locks, edits, publishes",
                    crate::state::NodeRole::Viewer => "sync + read only",
                    crate::state::NodeRole::Archive => "receive-only, deep history",
                }
            );
            println!("  wait-timeout    : {}", fmt_dur(c.wait_timeout()));
            println!("  dashboard-port  : {}", c.dashboard_port);
            println!("  update-channel  : {}", c.update_channel);
            println!("  junk-filter     : {}", on_off(c.junk_filter));
            println!(
                "  sync-only       : {}",
                if c.sync_only.is_empty() {
                    "(everything)"
                } else {
                    &c.sync_only
                }
            );
            println!(
                "  sync-skip       : {}",
                if c.sync_skip.is_empty() {
                    "(none)"
                } else {
                    &c.sync_skip
                }
            );
            println!(
                "  max-file-size   : {}",
                crate::state::fmt_size(c.max_file_size)
            );
            println!(
                "  history-depth   : {}",
                if c.history_depth == 0 {
                    "auto".to_string()
                } else {
                    c.history_depth.to_string()
                }
            );
            println!(
                "  max-down        : {}",
                if c.max_down == 0 {
                    "unlimited".to_string()
                } else {
                    format!("{}/s", crate::state::fmt_size(c.max_down))
                }
            );
            println!("  audit           : {}", on_off(c.audit));
            println!("  hooks           : {}", on_off(c.hooks));
            println!("  notify          : {}", on_off(c.notify));
            println!(
                "\n(values shown are effective/clamped; per-run flags override network keys; \
                 changes apply on next `tazamun start`; `tazamun setup` edits these interactively)"
            );
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            // One parser for every key — shared with the setup panel and the
            // init wizard (state::SessionConfig::set_value).
            let note = state
                .config
                .set_value(&key, &value)
                .map_err(CliError::Refused)?;
            state.save(dir)?;
            println!("✔ config set: {note}");
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
        let _ = writeln!(out, "\nheld remote records ({}):", unapplied.len());
        for u in &unapplied {
            let _ = writeln!(out, "  ⚠ {}  ({})", s(&u["path"]), s(&u["reason"]));
        }
    }
    let held_local = data["held_local"].as_array().cloned().unwrap_or_default();
    if !held_local.is_empty() {
        let _ = writeln!(
            out,
            "\nheld local files — on disk, not synced ({}):",
            held_local.len()
        );
        for h in &held_local {
            let _ = writeln!(out, "  ○ {}  ({})", s(&h["path"]), s(&h["reason"]));
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

    // (f) quarantine hygiene (local, P18).
    sections.push(crate::doctor::quarantine_section(dir));

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
    for v in &versions {
        let pin = if v["pinned"].as_bool() == Some(true) {
            " 📌"
        } else {
            ""
        };
        let tag = v["tag"]
            .as_str()
            .map(|t| format!("  «{t}»"))
            .unwrap_or_default();
        println!(
            "  [{}] {}  {} bytes{tag}{pin}",
            v["n"].as_u64().unwrap_or(0),
            v["ts"].as_str().unwrap_or("-"),
            v["size"].as_u64().unwrap_or(0)
        );
    }
    let hb = data["history_bytes"].as_u64().unwrap_or(0);
    let sb = data["session_history_bytes"].as_u64().unwrap_or(0);
    println!(
        "\nhistory: {} for this path · {} for the whole session",
        indicatif::HumanBytes(hb),
        indicatif::HumanBytes(sb)
    );
    println!(
        "restore: tazamun restore {path} <N> (held lease) · tag/pin: tazamun tag|pin {path} <N> …"
    );
}

/// Renders the chunk-aware diff of the current file vs a kept version.
fn print_diff(path: &str, n: usize, data: &serde_json::Value) {
    let d = data;
    let tag = d["version_tag"]
        .as_str()
        .map(|t| format!(" («{t}»)"))
        .unwrap_or_default();
    println!("diff: {path}  current ⟵ version {n}{tag}");
    if d["identical_content"].as_bool() == Some(true) {
        println!("  identical content — nothing changed.");
        return;
    }
    let pct = d["changed_pct"].as_f64().unwrap_or(0.0);
    let get = |k: &str| d[k].as_u64().unwrap_or(0);
    println!(
        "  content changed : {pct:.1}%  ({} would transfer to a peer holding the old version)",
        indicatif::HumanBytes(get("transfer_bytes"))
    );
    println!(
        "  chunks          : {} → {}  (identical {}, added {}, removed {}, moved {})",
        get("old_chunks"),
        get("new_chunks"),
        get("identical"),
        get("added"),
        get("removed"),
        get("moved"),
    );
    println!(
        "  size            : {} → {}",
        indicatif::HumanBytes(get("old_bytes")),
        indicatif::HumanBytes(get("new_bytes")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::endpoint::RelayChoice;
    use crate::state::SessionConfig;

    /// Pins the updater's archive-path contract to dist's real layouts — which
    /// DIFFER by format: tar.gz nests `tazamun-<target>/`, zip is flat. The
    /// first shipped guess used the tar shape for both, and every Windows
    /// update died with "specified file not found in archive". Layouts here
    /// were read from the live v0.1.1 assets.
    #[test]
    fn update_bin_path_matches_the_dist_archive_layout() {
        let expand = |t: &str, target: &str, bin: &str| {
            t.replace("{{ target }}", target).replace("{{ bin }}", bin)
        };
        assert_eq!(
            expand(UPDATE_BIN_PATH_TAR, "x86_64-unknown-linux-gnu", "tazamun"),
            "tazamun-x86_64-unknown-linux-gnu/tazamun"
        );
        // The real zip holds exactly LICENSE, README.md, tazamun.exe — no dir.
        assert_eq!(
            expand(UPDATE_BIN_PATH_ZIP, "x86_64-pc-windows-msvc", "tazamun.exe"),
            "tazamun.exe"
        );
        // Exactly one space inside the braces — self_update's substitution is
        // whitespace-tolerant, but the plain-replace form above is not, so the
        // templates must stay in this spelling for the test to mean anything.
        assert!(UPDATE_BIN_PATH_TAR.contains("{{ target }}"));
        assert!(UPDATE_BIN_PATH_TAR.contains("{{ bin }}"));
        assert!(UPDATE_BIN_PATH_ZIP.contains("{{ bin }}"));
        // And the picker chooses the zip shape exactly on Windows.
        if cfg!(windows) {
            assert_eq!(update_bin_path_in_archive(), UPDATE_BIN_PATH_ZIP);
        } else {
            assert_eq!(update_bin_path_in_archive(), UPDATE_BIN_PATH_TAR);
        }
    }

    /// The managed-install note fires for npm and Homebrew layouts — including
    /// the exact path a real npm-on-Windows install reported — and stays quiet
    /// for plain installs.
    #[test]
    fn managed_installs_are_recognized_from_the_exe_path() {
        use std::path::Path;
        let npm_win = Path::new(
            r"C:\Users\cc1a2b\AppData\Roaming\npm\node_modules\tazamun\node_modules\.bin_real\tazamun.exe",
        );
        assert_eq!(managed_by_path(npm_win).map(|(m, _)| m), Some("npm"));
        let npm_unix = Path::new("/usr/local/lib/node_modules/tazamun/bin/tazamun");
        assert_eq!(managed_by_path(npm_unix).map(|(m, _)| m), Some("npm"));
        let brew_mac = Path::new("/opt/homebrew/Cellar/tazamun/0.1.2/bin/tazamun");
        assert_eq!(managed_by_path(brew_mac).map(|(m, _)| m), Some("Homebrew"));
        let brew_linux = Path::new("/home/linuxbrew/.linuxbrew/Cellar/tazamun/0.1.2/bin/tazamun");
        assert_eq!(
            managed_by_path(brew_linux).map(|(m, _)| m),
            Some("Homebrew")
        );
        for plain in [
            "/home/user/.cargo/bin/tazamun",
            "/usr/local/bin/tazamun",
            r"C:\Users\me\.cargo\bin\tazamun.exe",
        ] {
            assert_eq!(managed_by_path(Path::new(plain)), None, "{plain}");
        }
    }

    /// A hand-built Windows GNU binary must update from the MSVC release asset
    /// (there is no GNU asset), and every released triple maps to itself.
    #[test]
    fn update_target_normalizes_windows_gnu_to_msvc() {
        assert_eq!(
            normalize_update_target("x86_64-pc-windows-gnu"),
            "x86_64-pc-windows-msvc"
        );
        for released in [
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
        ] {
            assert_eq!(normalize_update_target(released), released);
        }
        // The live value is one of the released assets after normalization.
        assert!(!update_target().contains("windows-gnu"));
    }

    fn flags(relay: Option<&str>, no_relay: bool, no_lan: bool, airgap: bool) -> NetFlags {
        NetFlags {
            relay: relay.map(String::from),
            no_relay,
            no_lan,
            airgap,
        }
    }

    #[test]
    fn rekey_rotates_key_and_admin_and_a_kept_member_adopts_it() {
        use crate::state::decode_hex32;
        let dir = tempfile::tempdir().unwrap();
        init(dir.path()).unwrap();
        let mut a = AppState::load(dir.path()).unwrap();
        let old_secret = a.session_secret.clone();
        let old_admin = a.admin_public_key.clone();
        // A stale address-book entry must be dropped by the rotation.
        a.known_members.insert(
            "stale".into(),
            AddrWire {
                id: [9u8; 32],
                relay: None,
                direct: vec![],
            },
        );
        let ticket = rekey_rotate(&mut a, 1000).unwrap();
        assert_ne!(a.session_secret, old_secret, "session key rotated");
        assert_ne!(a.admin_public_key, old_admin, "admin key rotated");
        assert!(a.known_members.is_empty(), "address book cleared");
        assert!(a.enforcing_roles());
        assert_eq!(
            ticket.secret.0,
            decode_hex32(&a.session_secret).unwrap(),
            "the new invite carries the new key"
        );

        // A kept member adopts the rotation and ends up on the same new key.
        let dirb = tempfile::tempdir().unwrap();
        init(dirb.path()).unwrap();
        let mut b = AppState::load(dirb.path()).unwrap();
        let b_old = b.session_secret.clone();
        let role = rekey_adopt(&mut b, &ticket, 1000).unwrap();
        assert_eq!(role, NodeRole::Editor);
        assert_eq!(b.session_secret, a.session_secret, "B shares A's new key");
        assert_ne!(b.session_secret, b_old, "B's key actually changed");
        assert!(
            b.admin_secret_key.is_some(),
            "an editor rekey invite carries the admin secret"
        );

        // An expired rekey invite is refused (issued 0, ttl 1 ⇒ expired at now=1000).
        let admin = a.admin_secret_key().unwrap();
        let admin_pub = a.admin_public_bytes().unwrap();
        let expired = session::mint_ticket(
            decode_hex32(&a.session_secret).unwrap(),
            Some((&admin, admin_pub)),
            session::ROLE_EDITOR,
            [1u8; 16],
            0,
            1,
            vec![],
        );
        let mut c = AppState::load(dirb.path()).unwrap();
        assert!(
            rekey_adopt(&mut c, &expired, 1000).is_err(),
            "an expired rekey invite must be refused"
        );
    }

    #[test]
    fn rekey_refuses_a_legacy_session() {
        // A session with no admin key (legacy v1) has nothing to rotate.
        let dir = tempfile::tempdir().unwrap();
        init(dir.path()).unwrap();
        let mut s = AppState::load(dir.path()).unwrap();
        s.admin_secret_key = None;
        s.admin_public_key = None;
        assert!(rekey_rotate(&mut s, 1).is_err());
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
    fn config_set_values_parse_and_clamp_via_the_shared_parser() {
        use crate::consts::{MAX_LEASE_TTL, MIN_LEASE_TTL};
        // `config set` now routes through state::SessionConfig::set_value —
        // assert the clamp semantics survived the move.
        let mut c = SessionConfig::default();
        c.set_value("lease-ttl", "15m").unwrap();
        assert_eq!(c.lease_ttl(), std::time::Duration::from_secs(15 * 60));
        c.set_value("lease-ttl", "1s").unwrap();
        assert_eq!(c.lease_ttl(), MIN_LEASE_TTL, "clamps up to the floor");
        c.set_value("lease-ttl", "72h").unwrap();
        assert_eq!(c.lease_ttl(), MAX_LEASE_TTL, "clamps down to the ceiling");
        assert!(c.set_value("lease-ttl", "soon").is_err());
        // on/off parsing is liberal on input, strict on garbage.
        c.set_value("lan", "YES").unwrap();
        assert!(c.lan);
        c.set_value("lan", "0").unwrap();
        assert!(!c.lan);
        assert!(c.set_value("lan", "maybe").is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        // Catches arg/subcommand conflicts and drift after seven phases.
        Cli::command().debug_assert();
    }

    #[test]
    fn completions_generate_for_every_shell() {
        use clap::CommandFactory;
        use clap_complete::Shell;
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "tazamun", &mut buf);
            assert!(!buf.is_empty(), "{shell:?} completion was empty");
            assert!(
                String::from_utf8_lossy(&buf).contains("tazamun"),
                "{shell:?} completion missing the binary name"
            );
        }
    }

    #[test]
    fn man_page_renders_nonempty() {
        use clap::CommandFactory;
        let mut buf = Vec::new();
        clap_mangen::Man::new(Cli::command())
            .render(&mut buf)
            .expect("man render");
        assert!(!buf.is_empty());
        assert!(String::from_utf8_lossy(&buf).contains("tazamun"));
    }
}
