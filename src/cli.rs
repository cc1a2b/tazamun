//! Command-line interface and command handlers.
//!
//! Invariant: mutating commands go through the running daemon's IPC socket;
//! this module never touches synced files directly — the daemon is the only
//! writer, so the strict-mode guarantees cannot be bypassed from the CLI.

use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand};

use crate::daemon::{DaemonConfig, DaemonError};
use crate::ipc::{self, IpcRequest, IpcResponse};
use crate::locks::LockTimings;
use crate::net::endpoint::{NetConfig, RelayChoice};
use crate::session::{AddrWire, SessionSecret, Ticket};
use crate::state::{AppState, encode_hex32};

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
    /// Show members, connections, leases and pending pulls.
    Status,
    /// Print a fresh invite ticket for this session.
    Invite,
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
pub async fn run(cli: Cli) -> Result<(), CliError> {
    let dir = std::path::absolute(&cli.dir).map_err(crate::state::StateError::Io)?;
    match cli.cmd {
        Cmd::Init => init(&dir),
        Cmd::Join { ticket } => join(&dir, &ticket),
        Cmd::Start {
            relay,
            no_relay,
            lan,
        } => start(&dir, relay, no_relay, lan).await,
        Cmd::Status => {
            let data = request(&dir, IpcRequest::Status).await?;
            print_status(&data);
            Ok(())
        }
        Cmd::Invite => {
            let data = request(&dir, IpcRequest::Invite).await?;
            let ticket = data
                .get("ticket")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            println!("Share this ticket to invite someone:\n\n  {ticket}\n");
            Ok(())
        }
        Cmd::Lock { path } => {
            let data = request(&dir, IpcRequest::Lock { path: path.clone() }).await?;
            let ttl = data.get("ttl_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            if data.get("already").and_then(|v| v.as_bool()) == Some(true) {
                println!("✔ {path} — you already hold this lease");
            } else {
                println!(
                    "✔ {path} is now writable (lease TTL {}s, auto-renewed)",
                    ttl / 1000
                );
            }
            Ok(())
        }
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

fn print_status(data: &serde_json::Value) {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("-").to_string();
    println!("peer id : {}", s(&data["id"]));
    println!("folder  : {}", s(&data["dir"]));
    println!(
        "files   : {} ({} bytes)",
        data["file_count"].as_u64().unwrap_or(0),
        data["total_bytes"].as_u64().unwrap_or(0)
    );
    let members = data["members"].as_array().cloned().unwrap_or_default();
    println!("\nmembers ({}):", members.len());
    if members.is_empty() {
        println!("  (none known yet — share `tazamun invite`)");
    }
    for m in members {
        let online = if m["online"].as_bool().unwrap_or(false) {
            "online "
        } else {
            "offline"
        };
        let rtt = m["rtt_ms"]
            .as_u64()
            .map(|v| format!("{v} ms"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {}  {}  {:<7}  rtt {}",
            short_id(&s(&m["id"])),
            online,
            s(&m["conn"]),
            rtt
        );
    }
    let leases = data["leases"].as_array().cloned().unwrap_or_default();
    println!("\nactive leases ({}):", leases.len());
    for l in leases {
        let holder = if l["mine"].as_bool().unwrap_or(false) {
            "you".to_string()
        } else {
            short_id(&s(&l["holder"]))
        };
        println!(
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
        println!("\npending pulls ({}):", pulls.len());
        for p in pulls {
            println!("  {}", p.as_str().unwrap_or("-"));
        }
    }
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

fn short_id(id: &str) -> String {
    id.chars().take(10).collect()
}
