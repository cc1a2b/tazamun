//! `tazamun send` / `tazamun receive` — one-shot, session-less transfer
//! (P12). One file or folder, one single-use expiring ticket, no `init`, no
//! daemon.
//!
//! It reuses the proven stack: [`build_endpoint_with_alpns`] for the same
//! NAT-traversal / relay / LAN wiring, the FastCDC [`chunker`] for
//! content-defined chunks, BLAKE3 verify-on-arrival, a mutual proof-of-secret
//! handshake ([`control::proof`]) so a leaked ticket still needs the secret,
//! and atomic verify-then-rename assembly. It carries its own tiny
//! length-prefixed framed protocol on a dedicated ALPN, so it never touches
//! session state.
//!
//! Invariants: the receiver verifies every chunk against the manifest hash
//! before writing; final files appear only by atomic rename after the whole
//! manifest verifies, so an interrupted receive leaves nothing at the
//! destination (partial progress lives in a hidden staging dir for resume);
//! the sender's ticket dies after the first successful receive or its TTL.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, SecretKey};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::consts::{MAX_FRAME, SEND_ALPN};
use crate::net::control::proof;
use crate::net::endpoint::{NetConfig, build_endpoint_with_alpns};
use crate::proto::ChunkRef;
use crate::session::AddrWire;
use crate::sync::chunker;
use crate::sync::ignore::IgnoreSet;
use crate::sync::index::sanitize_rel_path;

const TICKET_PREFIX: &str = "tzs1";
const HANDSHAKE_LABEL_SEND: &[u8] = b"tazamun-send-sender";
const HANDSHAKE_LABEL_RECV: &[u8] = b"tazamun-send-receiver";
/// Per-message deadline on the wire; a stalled peer never wedges forever.
const IO_DEADLINE: Duration = Duration::from_secs(60);

// ─── ticket (tzs1…) ──────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("send ticket does not start with `{TICKET_PREFIX}`")]
    BadPrefix,
    #[error("send ticket body is not valid base32")]
    BadEncoding,
    #[error("unsupported send-ticket version {0}")]
    BadVersion(u8),
    #[error("send ticket payload is malformed")]
    Malformed,
}

/// A single-use transfer ticket: the shared proof secret plus where to reach
/// the sender. Mirrors the session `Ticket` format on its own `tzs1` prefix.
#[derive(Clone, Serialize, Deserialize)]
struct SendTicketWire {
    version: u8,
    secret: [u8; 32],
    ttl_secs: u32,
    bootstrap: Vec<AddrWire>,
}

/// The decoded form used by `receive`.
pub struct SendTicket {
    pub secret: [u8; 32],
    pub ttl_secs: u32,
    pub bootstrap: Vec<AddrWire>,
}

impl SendTicket {
    fn encode(&self) -> String {
        let wire = SendTicketWire {
            version: 1,
            secret: self.secret,
            ttl_secs: self.ttl_secs,
            bootstrap: self.bootstrap.clone(),
        };
        let bytes = postcard::to_stdvec(&wire).unwrap_or_default();
        let mut out = String::from(TICKET_PREFIX);
        out.push_str(&data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase());
        out
    }

    pub fn decode(s: &str) -> Result<Self, TicketError> {
        let body = s
            .trim()
            .strip_prefix(TICKET_PREFIX)
            .ok_or(TicketError::BadPrefix)?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(|_| TicketError::BadEncoding)?;
        let wire: SendTicketWire =
            postcard::from_bytes(&bytes).map_err(|_| TicketError::Malformed)?;
        if wire.version != 1 {
            return Err(TicketError::BadVersion(wire.version));
        }
        Ok(Self {
            secret: wire.secret,
            ttl_secs: wire.ttl_secs,
            bootstrap: wire.bootstrap,
        })
    }
}

// ─── wire protocol ───────────────────────────────────────────────────────────

/// One file in the transfer: its relative path, total size, and ordered
/// content-defined chunks (hash + length).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendEntry {
    pub path: String,
    pub size: u64,
    pub chunks: Vec<ChunkRef>,
}

#[derive(Debug, Serialize, Deserialize)]
enum SendMsg {
    /// receiver → sender: begin the mutual proof.
    Hello { nonce: [u8; 16] },
    /// sender → receiver: proof of the ticket secret + the sender's nonce.
    HelloAck { nonce: [u8; 16], proof: [u8; 32] },
    /// receiver → sender: proof back.
    Proof { proof: [u8; 32] },
    /// sender → receiver: the full manifest of what is on offer.
    Manifest { entries: Vec<SendEntry> },
    /// receiver → sender: per-entry resume point (chunk index to start from).
    Start { resume: Vec<u32> },
    /// sender → receiver: one chunk's bytes (strictly ordered; the receiver
    /// knows the expected hash from its position).
    Chunk { data: Vec<u8> },
    /// sender → receiver: the current entry is fully streamed.
    EntryDone,
    /// sender → receiver: the whole manifest is streamed.
    AllDone,
    /// receiver → sender: everything verified and assembled.
    Received,
    /// either direction: a fatal error, for a clean close.
    Fail { msg: String },
}

/// Reads one length-prefixed postcard frame (`u32` BE length, then body),
/// rejecting a zero or over-cap length exactly like the session framing.
async fn read_frame(recv: &mut RecvStream) -> Result<SendMsg, String> {
    let mut len = [0u8; 4];
    tokio::time::timeout(IO_DEADLINE, recv.read_exact(&mut len))
        .await
        .map_err(|_| "read timed out".to_string())?
        .map_err(|e| format!("stream closed: {e}"))?;
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(format!("frame length {n} out of bounds"));
    }
    let mut buf = vec![0u8; n];
    tokio::time::timeout(IO_DEADLINE, recv.read_exact(&mut buf))
        .await
        .map_err(|_| "read timed out".to_string())?
        .map_err(|e| format!("stream closed mid-frame: {e}"))?;
    postcard::from_bytes(&buf).map_err(|e| format!("bad frame: {e}"))
}

async fn write_frame(send: &mut SendStream, msg: &SendMsg) -> Result<(), String> {
    let body = postcard::to_stdvec(msg).map_err(|e| format!("encode: {e}"))?;
    if body.len() > MAX_FRAME {
        return Err(format!("frame body {} exceeds cap", body.len()));
    }
    let len = (body.len() as u32).to_be_bytes();
    send.write_all(&len).await.map_err(|e| e.to_string())?;
    send.write_all(&body).await.map_err(|e| e.to_string())?;
    Ok(())
}

// ─── send ────────────────────────────────────────────────────────────────────

/// What `send` did, for the caller to report.
pub struct SendOutcome {
    pub files: usize,
    pub bytes: u64,
}

/// Serves `path` (a file or folder) over a fresh ephemeral endpoint until one
/// receiver completes, or `ttl` elapses. `on_ticket` is called once with the
/// `tzs1…` string as soon as the endpoint has addresses (so the CLI prints
/// it and tests capture it).
pub async fn send(
    path: &Path,
    net: &NetConfig,
    ttl: Duration,
    on_ticket: impl FnOnce(&str),
) -> Result<SendOutcome, String> {
    let entries = build_entries(path)?;
    if entries.is_empty() {
        return Err(format!("{} has nothing to send", path.display()));
    }
    let root = send_root(path);
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();

    let secret: [u8; 32] = rand::random();
    let ep_key = SecretKey::generate();
    let endpoint = build_endpoint_with_alpns(ep_key, net, vec![SEND_ALPN.to_vec()])
        .await
        .map_err(|e| format!("endpoint: {e}"))?;

    let bootstrap = endpoint_bootstrap(&endpoint).await;
    let ticket = SendTicket {
        secret,
        ttl_secs: ttl.as_secs().min(u32::MAX as u64) as u32,
        bootstrap,
    };
    on_ticket(&ticket.encode());

    // Accept connections until one completes the transfer or the TTL fires.
    // A failed/incomplete receiver does not burn the ticket — the next one
    // may succeed within the window.
    let me = endpoint.id();
    let deadline = tokio::time::Instant::now() + ttl;
    loop {
        let incoming = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                let _ = endpoint.close().await;
                return Err("ticket expired before anyone received it".to_string());
            }
            inc = endpoint.accept() => match inc {
                Some(inc) => inc,
                None => {
                    let _ = endpoint.close().await;
                    return Err("endpoint closed".to_string());
                }
            }
        };
        let conn = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                debug!("send: inbound connection failed: {e}");
                continue;
            }
        };
        match serve_one(&conn, me, &secret, &entries, &root, total_bytes).await {
            Ok(()) => {
                // Give the QUIC close a moment to flush, then done.
                conn.close(0u32.into(), b"done");
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = endpoint.close().await;
                return Ok(SendOutcome {
                    files: entries.len(),
                    bytes: total_bytes,
                });
            }
            Err(e) => {
                warn!("send: receiver did not complete: {e}");
                conn.close(1u32.into(), b"failed");
                // loop and wait for another receiver until the deadline.
            }
        }
    }
}

/// Handles one receiver over `conn`: mutual proof, manifest, then stream the
/// chunks it asks for from the resume point.
async fn serve_one(
    conn: &Connection,
    me: iroh::EndpointId,
    secret: &[u8; 32],
    entries: &[SendEntry],
    root: &Path,
    total_bytes: u64,
) -> Result<(), String> {
    let (mut send, mut recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
    let remote = conn.remote_id();

    // Handshake (sender = acceptor side).
    let SendMsg::Hello { nonce: nonce_r } = read_frame(&mut recv).await? else {
        return Err("expected Hello".to_string());
    };
    let nonce_s: [u8; 16] = rand::random();
    let my_proof = proof(
        secret,
        HANDSHAKE_LABEL_SEND,
        &me,
        &remote,
        &nonce_r,
        &nonce_s,
    );
    write_frame(
        &mut send,
        &SendMsg::HelloAck {
            nonce: nonce_s,
            proof: my_proof,
        },
    )
    .await?;
    let SendMsg::Proof { proof: their } = read_frame(&mut recv).await? else {
        return Err("expected Proof".to_string());
    };
    let expected = proof(
        secret,
        HANDSHAKE_LABEL_RECV,
        &me,
        &remote,
        &nonce_r,
        &nonce_s,
    );
    if !constant_time_eq(&expected, &their) {
        return Err("receiver failed the proof-of-secret (wrong ticket?)".to_string());
    }

    // Offer and resume plan.
    write_frame(
        &mut send,
        &SendMsg::Manifest {
            entries: entries.to_vec(),
        },
    )
    .await?;
    let SendMsg::Start { resume } = read_frame(&mut recv).await? else {
        return Err("expected Start".to_string());
    };
    if resume.len() != entries.len() {
        return Err("receiver sent a malformed resume plan".to_string());
    }

    let bar = progress_bar(total_bytes, "sending");
    let mut sent: u64 = 0;
    for (entry, &from) in entries.iter().zip(&resume) {
        // Re-chunk this file and stream from the resume index. FastCDC is
        // deterministic, so boundaries match the manifest exactly.
        let abs = root.join(fs_rel(&entry.path));
        let mut index = 0u32;
        let mut pending: Result<(), String> = Ok(());
        let stream_res = chunker::chunk_file(&abs, |_r, data| {
            if index < from {
                index += 1;
                return Ok(());
            }
            index += 1;
            // Block the chunker thread on the async write via a tiny bridge:
            // record any error and stop by returning it.
            pending = futures_write(&mut send, &data);
            if pending.is_err() {
                return Err(chunker::ChunkError::Io(std::io::Error::other(
                    "send failed",
                )));
            }
            sent += data.len() as u64;
            bar.set_position(sent);
            Ok(())
        });
        pending?;
        stream_res.map_err(|e| format!("re-chunking {}: {e}", entry.path))?;
        write_frame(&mut send, &SendMsg::EntryDone).await?;
    }
    write_frame(&mut send, &SendMsg::AllDone).await?;

    // The receiver confirms it verified and assembled everything.
    match read_frame(&mut recv).await? {
        SendMsg::Received => {
            bar.finish_with_message("sent");
            Ok(())
        }
        SendMsg::Fail { msg } => Err(format!("receiver reported: {msg}")),
        _ => Err("expected Received".to_string()),
    }
}

/// Writes one `Chunk` frame synchronously from inside the (blocking) chunker
/// callback by driving the future to completion on the current runtime handle.
fn futures_write(send: &mut SendStream, data: &[u8]) -> Result<(), String> {
    let handle = tokio::runtime::Handle::current();
    let msg = SendMsg::Chunk {
        data: data.to_vec(),
    };
    tokio::task::block_in_place(|| handle.block_on(write_frame(send, &msg)))
}

// ─── receive ─────────────────────────────────────────────────────────────────

/// What `receive` produced.
#[derive(Debug)]
pub struct RecvOutcome {
    pub files: usize,
    pub bytes: u64,
    pub dest: PathBuf,
}

/// Connects to the sender named by `ticket`, verifies every chunk, and
/// assembles the files under `dest`. Interrupted mid-transfer leaves nothing
/// at the destination; partial progress is kept in `dest/.tazamun-recv/…` for
/// a resumed `receive`.
pub async fn receive(ticket: &str, dest: &Path, net: &NetConfig) -> Result<RecvOutcome, String> {
    let ticket = SendTicket::decode(ticket).map_err(|e| e.to_string())?;
    let addr = ticket
        .bootstrap
        .iter()
        .find_map(AddrWire::to_endpoint_addr)
        .ok_or("ticket carries no reachable address")?;

    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let ep_key = SecretKey::generate();
    let endpoint = build_endpoint_with_alpns(ep_key, net, vec![SEND_ALPN.to_vec()])
        .await
        .map_err(|e| format!("endpoint: {e}"))?;
    let conn = endpoint
        .connect(addr, SEND_ALPN)
        .await
        .map_err(|e| format!("could not reach the sender: {e}"))?;
    let me = endpoint.id();
    let remote = conn.remote_id();
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;

    // Handshake (receiver = initiator side).
    let nonce_r: [u8; 16] = rand::random();
    write_frame(&mut send, &SendMsg::Hello { nonce: nonce_r }).await?;
    let SendMsg::HelloAck {
        nonce: nonce_s,
        proof: their,
    } = read_frame(&mut recv).await?
    else {
        return Err("expected HelloAck".to_string());
    };
    let expected = proof(
        &ticket.secret,
        HANDSHAKE_LABEL_SEND,
        &remote,
        &me,
        &nonce_r,
        &nonce_s,
    );
    if !constant_time_eq(&expected, &their) {
        return Err("sender failed the proof-of-secret (wrong ticket?)".to_string());
    }
    let my_proof = proof(
        &ticket.secret,
        HANDSHAKE_LABEL_RECV,
        &remote,
        &me,
        &nonce_r,
        &nonce_s,
    );
    write_frame(&mut send, &SendMsg::Proof { proof: my_proof }).await?;

    // Manifest.
    let SendMsg::Manifest { entries } = read_frame(&mut recv).await? else {
        return Err("expected Manifest".to_string());
    };
    validate_manifest(&entries)?;
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();

    // Resume: for each entry, how many leading chunks already sit verified in
    // staging. Staging is keyed by the manifest id so a retry to the same dest
    // continues instead of restarting.
    let staging = dest.join(".tazamun-recv").join(manifest_id(&entries));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let mut resume = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        resume.push(verified_prefix(&staging.join(i.to_string()), entry));
    }
    write_frame(
        &mut send,
        &SendMsg::Start {
            resume: resume.clone(),
        },
    )
    .await?;

    // Receive + verify + stage, entry by entry.
    let bar = progress_bar(total_bytes, "receiving");
    let mut done: u64 = entries
        .iter()
        .zip(&resume)
        .map(|(e, &r)| {
            e.chunks
                .iter()
                .take(r as usize)
                .map(|c| c.len as u64)
                .sum::<u64>()
        })
        .sum();
    bar.set_position(done);
    for (i, entry) in entries.iter().enumerate() {
        let part = staging.join(i.to_string());
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)
            .map_err(|e| format!("staging {}: {e}", entry.path))?;
        let mut idx = resume[i] as usize;
        loop {
            match read_frame(&mut recv).await? {
                SendMsg::Chunk { data } => {
                    let expected = entry
                        .chunks
                        .get(idx)
                        .ok_or_else(|| format!("sender sent an extra chunk for {}", entry.path))?;
                    let got = blake3::hash(&data);
                    if got.as_bytes() != &expected.hash || data.len() as u32 != expected.len {
                        return Err(abort(&mut send, "chunk failed verification").await);
                    }
                    file.write_all(&data).map_err(|e| e.to_string())?;
                    idx += 1;
                    done += data.len() as u64;
                    bar.set_position(done);
                }
                SendMsg::EntryDone => break,
                SendMsg::Fail { msg } => return Err(format!("sender aborted: {msg}")),
                _ => return Err("expected Chunk or EntryDone".to_string()),
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        if idx != entry.chunks.len() {
            return Err(abort(&mut send, "entry ended before all chunks arrived").await);
        }
    }
    match read_frame(&mut recv).await? {
        SendMsg::AllDone => {}
        SendMsg::Fail { msg } => return Err(format!("sender aborted: {msg}")),
        _ => return Err("expected AllDone".to_string()),
    }

    // Everything verified: publish atomically, then drop the staging dir. The
    // destination paths appear only here, so an earlier interruption left the
    // destination untouched.
    for (i, entry) in entries.iter().enumerate() {
        let part = staging.join(i.to_string());
        let final_path = dest.join(fs_rel(&entry.path));
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Same-filesystem rename (staging lives under dest) is atomic. Clear
        // any stale destination first; Windows refuses to rename over a
        // read-only or briefly-locked file, so go through the bounded retry.
        let _ = crate::guard::set_writable(&final_path);
        let _ = std::fs::remove_file(&final_path);
        crate::win_fs::with_retry("publish", &final_path, || {
            std::fs::rename(&part, &final_path)
        })
        .map_err(|e| format!("publishing {}: {e}", entry.path))?;
    }
    let _ = std::fs::remove_dir_all(dest.join(".tazamun-recv"));
    write_frame(&mut send, &SendMsg::Received).await?;
    let _ = send.finish();
    bar.finish_with_message("received");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = endpoint.close().await;

    Ok(RecvOutcome {
        files: entries.len(),
        bytes: total_bytes,
        dest: dest.to_path_buf(),
    })
}

/// Signals the peer of a fatal condition (best-effort) and returns the
/// message, so the caller can `return Err(abort(..).await)` from any context.
async fn abort(send: &mut SendStream, msg: &str) -> String {
    let _ = write_frame(
        send,
        &SendMsg::Fail {
            msg: msg.to_string(),
        },
    )
    .await;
    msg.to_string()
}

// ─── helpers (pure where possible) ───────────────────────────────────────────

/// Builds the manifest for a path: a single file, or every non-junk file
/// under a folder (the P11 junk filter keeps editor scratch out of a send).
fn build_entries(path: &Path) -> Result<Vec<SendEntry>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    if meta.is_file() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("file has a non-UTF-8 name")?;
        out.push(entry_for(path, name)?);
        return Ok(out);
    }
    if !meta.is_dir() {
        return Err(format!("{} is neither a file nor a folder", path.display()));
    }
    let ignore = IgnoreSet::build("", true, "", "", 0);
    let mut files = Vec::new();
    collect_files(path, path, &mut files)?;
    files.sort();
    for rel in files {
        if let Ok(sane) = sanitize_rel_path(&rel)
            && !ignore.verdict(&sane, None).is_sync()
        {
            continue;
        }
        out.push(entry_for(&path.join(fs_rel(&rel)), &rel)?);
    }
    Ok(out)
}

/// Hashes one file into its chunk manifest without holding the bytes.
fn entry_for(abs: &Path, rel: &str) -> Result<SendEntry, String> {
    let (chunks, size) =
        chunker::chunk_file(abs, |_, _| Ok(())).map_err(|e| format!("chunking {rel}: {e}"))?;
    Ok(SendEntry {
        path: rel.to_string(),
        size,
        chunks,
    })
}

/// The directory files are resolved against: the file's parent for a single
/// file, the folder itself for a folder.
fn send_root(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    }
}

/// Recursively lists forward-slash relative paths under `root`, skipping the
/// receiver-side staging directory.
fn collect_files(root: &Path, cur: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(cur).map_err(|e| format!("{}: {e}", cur.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.file_name().and_then(|n| n.to_str()) == Some(".tazamun-recv") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect_files(root, &p, out)?;
        } else if meta.is_file()
            && let Ok(rel) = p.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Converts a forward-slash relative path to an OS path fragment.
fn fs_rel(rel: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

/// A stable id for a manifest, so resume staging survives a reconnect.
fn manifest_id(entries: &[SendEntry]) -> String {
    let bytes = postcard::to_stdvec(entries).unwrap_or_default();
    let hash = blake3::hash(&bytes);
    data_encoding::HEXLOWER.encode(&hash.as_bytes()[..8])
}

/// How many leading chunks of `entry` are already present *and verified* in
/// the staging file: re-hash from the front, stop at the first mismatch, and
/// truncate the file to that boundary so a resumed append is clean.
fn verified_prefix(part: &Path, entry: &SendEntry) -> u32 {
    let Ok(mut f) = std::fs::File::open(part) else {
        return 0;
    };
    let mut good = 0u32;
    let mut good_bytes = 0u64;
    for chunk in &entry.chunks {
        let mut buf = vec![0u8; chunk.len as usize];
        if f.read_exact(&mut buf).is_err() {
            break;
        }
        if blake3::hash(&buf).as_bytes() != &chunk.hash {
            break;
        }
        good += 1;
        good_bytes += chunk.len as u64;
    }
    drop(f);
    // Trim any partial/garbage tail past the last verified chunk.
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(part) {
        let _ = f.set_len(good_bytes);
    }
    good
}

/// Bounds an incoming manifest before we allocate against its claims.
fn validate_manifest(entries: &[SendEntry]) -> Result<(), String> {
    if entries.len() > crate::consts::MAX_CHUNKS_PER_FILE {
        return Err("manifest lists an implausible number of files".to_string());
    }
    for e in entries {
        sanitize_rel_path(&e.path)
            .map_err(|_| format!("manifest has a hostile path: {}", e.path))?;
        crate::sync::manifest::check(&e.chunks, e.size)
            .map_err(|err| format!("manifest for {} is inconsistent: {err}", e.path))?;
    }
    Ok(())
}

async fn endpoint_bootstrap(endpoint: &Endpoint) -> Vec<AddrWire> {
    // Wait briefly for the endpoint to learn its addresses (relay + direct).
    for _ in 0..40 {
        let addr = endpoint.addr();
        if !addr.addrs.is_empty() {
            return vec![AddrWire::from_endpoint_addr(&addr)];
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    vec![AddrWire::from_endpoint_addr(&endpoint.addr())]
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

fn progress_bar(total: u64, verb: &str) -> indicatif::ProgressBar {
    let bar = indicatif::ProgressBar::new(total);
    bar.set_style(
        indicatif::ProgressStyle::with_template(
            "{msg} {wide_bar} {bytes}/{total_bytes} ({bytes_per_sec})",
        )
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
    );
    bar.set_message(verb.to_string());
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, lens: &[u32]) -> SendEntry {
        let chunks: Vec<ChunkRef> = lens
            .iter()
            .map(|&len| ChunkRef {
                hash: *blake3::hash(&vec![0u8; len as usize]).as_bytes(),
                len,
            })
            .collect();
        SendEntry {
            path: path.to_string(),
            size: lens.iter().map(|&l| l as u64).sum(),
            chunks,
        }
    }

    #[test]
    fn ticket_roundtrips_on_its_own_prefix() {
        let t = SendTicket {
            secret: [7u8; 32],
            ttl_secs: 600,
            bootstrap: vec![AddrWire {
                id: [9u8; 32],
                relay: Some("https://r.example./".to_string()),
                direct: vec!["127.0.0.1:5555".parse().unwrap()],
            }],
        };
        let s = t.encode();
        assert!(s.starts_with("tzs1"));
        assert_eq!(s, s.to_lowercase());
        let back = SendTicket::decode(&s).unwrap();
        assert_eq!(back.secret, t.secret);
        assert_eq!(back.ttl_secs, 600);
        assert_eq!(back.bootstrap, t.bootstrap);
    }

    #[test]
    fn ticket_rejects_session_prefix_and_garbage() {
        // A session ticket is not a send ticket.
        assert!(matches!(
            SendTicket::decode("tzm1abcdef"),
            Err(TicketError::BadPrefix)
        ));
        assert!(matches!(
            SendTicket::decode("tzs1!!!!"),
            Err(TicketError::BadEncoding)
        ));
    }

    #[test]
    fn manifest_id_is_stable_and_content_addressed() {
        let a = vec![entry("a.txt", &[10, 20])];
        let b = vec![entry("a.txt", &[10, 20])];
        let c = vec![entry("a.txt", &[10, 21])];
        assert_eq!(manifest_id(&a), manifest_id(&b));
        assert_ne!(manifest_id(&a), manifest_id(&c));
    }

    #[test]
    fn verified_prefix_counts_good_chunks_and_trims_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("0");
        // Real chunk contents so the hashes match.
        let c0 = b"hello".to_vec();
        let c1 = b"world!!".to_vec();
        let e = SendEntry {
            path: "f".to_string(),
            size: (c0.len() + c1.len()) as u64,
            chunks: vec![
                ChunkRef {
                    hash: *blake3::hash(&c0).as_bytes(),
                    len: c0.len() as u32,
                },
                ChunkRef {
                    hash: *blake3::hash(&c1).as_bytes(),
                    len: c1.len() as u32,
                },
            ],
        };
        // Nothing staged yet.
        assert_eq!(verified_prefix(&part, &e), 0);
        // First chunk plus a torn tail: prefix is 1, tail trimmed.
        let mut staged = c0.clone();
        staged.extend_from_slice(b"wor"); // partial second chunk
        std::fs::write(&part, &staged).unwrap();
        assert_eq!(verified_prefix(&part, &e), 1);
        assert_eq!(std::fs::metadata(&part).unwrap().len(), c0.len() as u64);
        // Both chunks fully present: prefix is 2.
        let mut full = c0.clone();
        full.extend_from_slice(&c1);
        std::fs::write(&part, &full).unwrap();
        assert_eq!(verified_prefix(&part, &e), 2);
    }

    #[test]
    fn validate_manifest_rejects_hostile_paths_and_bad_sizes() {
        assert!(validate_manifest(&[entry("../escape", &[4])]).is_err());
        let mut bad = entry("ok.txt", &[4]);
        bad.size = 999; // size disagrees with the chunk lengths
        assert!(validate_manifest(&[bad]).is_err());
        assert!(validate_manifest(&[entry("ok.txt", &[4, 8])]).is_ok());
    }

    #[test]
    fn build_entries_skips_junk_in_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"real").unwrap();
        std::fs::write(dir.path().join(".keep.txt.swp"), b"vim").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/.DS_Store"), b"meta").unwrap();
        std::fs::write(dir.path().join("sub/data.bin"), b"bytes").unwrap();
        let entries = build_entries(dir.path()).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"keep.txt"));
        assert!(paths.contains(&"sub/data.bin"));
        assert!(!paths.iter().any(|p| p.contains(".swp")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("DS_Store")), "{paths:?}");
    }
}
