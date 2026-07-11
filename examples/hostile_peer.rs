//! cc1a2b's pentest kit: a runnable hostile peer.
//!
//! Connects to a target session over the *real* transport, completes the real
//! proof-of-secret handshake (an insider who holds the invite ticket), then
//! sends attacker-chosen frames for the selected scenario. Use it to confirm a
//! live daemon shrugs off a former member turning malicious.
//!
//! ```text
//! cargo run --example hostile_peer -- --ticket tzm1… --scenario all
//! cargo run --example hostile_peer -- --ticket tzm1… --scenario lease-grant-flood --count 500
//! ```
//!
//! After a run the target should be unharmed: `tazamun status` still answers,
//! the file table is unchanged, nothing was written outside the folder, and
//! there is no panic in the daemon log. See docs/PENTEST_PLAYBOOK.md.

use std::collections::BTreeMap;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tazamun::consts::CTL_ALPN;
use tazamun::net::control::{handshake_initiator, proof};
use tazamun::net::endpoint::{NetConfig, RelayChoice, build_endpoint};
use tazamun::proto::{ChunkRef, FileRecord, LeaseInfo, ManifestRef, Msg, read_msg, write_msg};
use tazamun::session::{SessionKeys, Ticket};
use tazamun::state::RelPath;

#[derive(Parser)]
#[command(about = "tazamun hostile-peer pentest driver")]
struct Args {
    /// tzm1… invite ticket for the target session (an insider who holds it).
    #[arg(long)]
    ticket: String,
    /// Which attack to run.
    #[arg(long, value_enum, default_value_t = Scenario::All)]
    scenario: Scenario,
    /// Message count for flood/storm scenarios.
    #[arg(long, default_value_t = 200)]
    count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Scenario {
    /// Flood LockGrant for paths the daemon never requested.
    LeaseGrantFlood,
    /// Storm of malformed FileMeta: size mismatches and unservable blob bombs.
    ManifestStorm,
    /// One Index packed with traversal / absolute / reserved / NUL / overlong
    /// paths.
    TraversalIndex,
    /// Record a valid proof, then replay it against a fresh connection.
    ReplayHandshake,
    /// Run every scenario in sequence.
    All,
}

/// Builds a `RelPath` straight from an arbitrary string, skipping the
/// sanitizer — exactly how a hostile peer puts a path on the wire. The daemon
/// re-sanitizes at its boundary; that is what we are exercising.
fn wire_path(s: &str) -> RelPath {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .expect("RelPath deserializes any string")
}

fn attacker_vv(n: u64) -> BTreeMap<String, u64> {
    BTreeMap::from([("attacker".to_string(), n + 1)])
}

type Authed = (
    iroh::Endpoint,
    iroh::endpoint::Connection,
    iroh::endpoint::SendStream,
    iroh::endpoint::RecvStream,
);

async fn connect_authed(ticket: &Ticket) -> Authed {
    let endpoint = build_endpoint(
        iroh::SecretKey::generate(),
        &NetConfig {
            relay: RelayChoice::Default,
            lan: true,
            ..Default::default()
        },
    )
    .await
    .expect("build endpoint");
    let keys = SessionKeys::derive(&ticket.secret);
    let addr = ticket
        .bootstrap
        .first()
        .and_then(|w| w.to_endpoint_addr())
        .expect("ticket carries a bootstrap address");
    let conn = endpoint
        .connect(addr, CTL_ALPN)
        .await
        .expect("connect to target");
    let (mut send, recv) = handshake_initiator(&conn, &keys, endpoint.id())
        .await
        .expect("handshake with the target (need the correct secret / ticket)");
    // Announce an empty index so the target counts us as a synced voter.
    write_msg(
        &mut send,
        &Msg::Index {
            lamport: 0,
            files: vec![],
            leases: vec![],
        },
    )
    .await
    .expect("send empty index");
    // The Connection is returned and held for the whole scenario so it stays
    // open while we drive its streams.
    (endpoint, conn, send, recv)
}

async fn lease_grant_flood(send: &mut iroh::endpoint::SendStream, count: usize) {
    println!("[lease-grant-flood] sending {count} unrequested LockGrant messages");
    for i in 0..count {
        let msg = Msg::LockGrant {
            path: wire_path(&format!("phantom/{i}.txt")),
        };
        if write_msg(send, &msg).await.is_err() {
            println!("[lease-grant-flood] target closed the stream at {i} (that is fine)");
            return;
        }
    }
    // Also renew leases we do not hold — must be ignored.
    let _ = write_msg(
        send,
        &Msg::LockRenew {
            path: wire_path("phantom/0.txt"),
            lamport: 999,
            ttl_ms: 90_000,
        },
    )
    .await;
    println!("[lease-grant-flood] done; the daemon must hold no new leases");
}

async fn manifest_storm(send: &mut iroh::endpoint::SendStream, count: usize) {
    println!("[manifest-storm] sending {count} malformed FileMeta messages");
    for i in 0..count {
        // Alternate: a size-lying inline manifest, and an unservable blob bomb
        // claiming a petabyte with a manifest blob we will never serve.
        let record = if i % 2 == 0 {
            FileRecord {
                size: 1_000_000,
                manifest: ManifestRef::Inline(vec![ChunkRef {
                    hash: [0xAB; 32],
                    len: 4,
                }]),
                vv: attacker_vv(i as u64),
                deleted: false,
                updated_at_ms: 1,
            }
        } else {
            FileRecord {
                size: u64::MAX,
                manifest: ManifestRef::Blob {
                    hash: [i as u8; 32],
                },
                vv: attacker_vv(i as u64),
                deleted: false,
                updated_at_ms: 1,
            }
        };
        let msg = Msg::FileMeta {
            path: wire_path(&format!("storm/f{i}.bin")),
            record,
            lamport: 10 + i as u64,
        };
        if write_msg(send, &msg).await.is_err() {
            println!("[manifest-storm] target closed the stream at {i} (that is fine)");
            return;
        }
    }
    println!("[manifest-storm] done; the daemon must write nothing (all unverifiable)");
}

async fn traversal_index(send: &mut iroh::endpoint::SendStream) {
    let evil = [
        "../pwn",
        "/etc/passwd",
        "C:\\pwn",
        "a\\b",
        "..",
        "x/../../pwn",
        ".tazamun/state.json",
        "n\0ul",
        "con",
        "aux.txt",
    ];
    let record = || FileRecord {
        size: 4,
        manifest: ManifestRef::Inline(vec![ChunkRef {
            hash: [9u8; 32],
            len: 4,
        }]),
        vv: attacker_vv(9),
        deleted: false,
        updated_at_ms: 1,
    };
    let mut files: Vec<(RelPath, FileRecord)> =
        evil.iter().map(|p| (wire_path(p), record())).collect();
    // Plus an overlong path.
    files.push((
        wire_path(&format!(
            "{}/x",
            "a".repeat(tazamun::consts::MAX_PATH_LEN + 8)
        )),
        record(),
    ));
    println!(
        "[traversal-index] sending one Index with {} hostile paths",
        files.len()
    );
    let leases = vec![LeaseInfo {
        path: wire_path("../evil-lease"),
        holder: "attacker".to_string(),
        lamport: 1,
        expires_in_ms: 90_000,
    }];
    let _ = write_msg(
        send,
        &Msg::Index {
            lamport: 42,
            files,
            leases,
        },
    )
    .await;
    println!("[traversal-index] done; every record must be dropped whole");
}

async fn replay_handshake(ticket: &Ticket) {
    println!("[replay-handshake] recording a valid proof, then replaying it");
    let endpoint = build_endpoint(
        iroh::SecretKey::generate(),
        &NetConfig {
            relay: RelayChoice::Default,
            lan: true,
            ..Default::default()
        },
    )
    .await
    .expect("build endpoint");
    let keys = SessionKeys::derive(&ticket.secret);
    let addr = ticket
        .bootstrap
        .first()
        .and_then(|w| w.to_endpoint_addr())
        .expect("ticket bootstrap addr");
    let me = endpoint.id();

    // Connection 1: valid handshake; record nonce_a + proof.
    let mut recorded: Option<([u8; 16], [u8; 32])> = None;
    if let Ok(conn) = endpoint.connect(addr.clone(), CTL_ALPN).await {
        let remote = conn.remote_id();
        if let Ok((mut send, mut recv)) = conn.open_bi().await {
            let na: [u8; 16] = rand::random();
            if write_msg(&mut send, &Msg::Hello { nonce: na })
                .await
                .is_ok()
                && let Ok(Msg::HelloAck { nonce: nb, .. }) = read_msg(&mut recv).await
            {
                let mine = proof(&keys.auth, b"init", &me, &remote, &na, &nb);
                let _ = write_msg(&mut send, &Msg::Proof { proof: mine }).await;
                recorded = Some((na, mine));
            }
        }
        conn.close(iroh::endpoint::VarInt::from_u32(0), b"done");
    }
    let Some((na_old, proof_old)) = recorded else {
        println!("[replay-handshake] could not record a proof (is the target reachable?)");
        endpoint.close().await;
        return;
    };

    // Connection 2: replay the recorded proof against a fresh nonce_b.
    let mut accepted = false;
    if let Ok(conn) = endpoint.connect(addr, CTL_ALPN).await {
        if let Ok((mut send, mut recv)) = conn.open_bi().await {
            let _ = write_msg(&mut send, &Msg::Hello { nonce: na_old }).await;
            if matches!(read_msg(&mut recv).await, Ok(Msg::HelloAck { .. })) {
                let _ = write_msg(&mut send, &Msg::Proof { proof: proof_old }).await;
                accepted = matches!(
                    tokio::time::timeout(Duration::from_secs(2), read_msg(&mut recv)).await,
                    Ok(Ok(Msg::Index { .. }))
                );
            }
        }
        conn.close(iroh::endpoint::VarInt::from_u32(0), b"done");
    }
    endpoint.close().await;
    if accepted {
        println!("[replay-handshake] *** VULNERABLE: the replayed proof was ACCEPTED ***");
    } else {
        println!("[replay-handshake] OK: the replayed proof was rejected (nonce binding holds)");
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let ticket = Ticket::decode(&args.ticket).expect("decode ticket");

    // Scenarios that need an authenticated control stream share one connection.
    let needs_authed = matches!(
        args.scenario,
        Scenario::LeaseGrantFlood
            | Scenario::ManifestStorm
            | Scenario::TraversalIndex
            | Scenario::All
    );
    let mut authed = if needs_authed {
        Some(connect_authed(&ticket).await)
    } else {
        None
    };

    match args.scenario {
        Scenario::LeaseGrantFlood => {
            lease_grant_flood(&mut authed.as_mut().unwrap().2, args.count).await
        }
        Scenario::ManifestStorm => {
            manifest_storm(&mut authed.as_mut().unwrap().2, args.count).await
        }
        Scenario::TraversalIndex => traversal_index(&mut authed.as_mut().unwrap().2).await,
        Scenario::ReplayHandshake => replay_handshake(&ticket).await,
        Scenario::All => {
            let send = &mut authed.as_mut().unwrap().2;
            lease_grant_flood(send, args.count).await;
            manifest_storm(send, args.count).await;
            traversal_index(send).await;
            replay_handshake(&ticket).await;
        }
    }

    // Give the target a moment to process before we tear down.
    tokio::time::sleep(Duration::from_secs(2)).await;
    if let Some((endpoint, _conn, _, _)) = authed {
        endpoint.close().await;
    }
    println!(
        "\nDONE. Now confirm the target is healthy:\n  \
         - `tazamun status` still answers and the file table is unchanged\n  \
         - nothing was written outside the session folder\n  \
         - no panic in the daemon log (.tazamun/logs/daemon.log)"
    );
}
