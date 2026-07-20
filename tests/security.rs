//! Adversarial behavior: hostile paths, oversized frames, forged tickets and
//! wrong-secret handshakes.

mod common;

use std::time::Duration;

use common::{RawPeer, TestNode, WAIT, handshake_outcome, seed_known_member, wait_until};
use tazamun::proto::{FileRecord, ManifestRef, Msg};
use tazamun::session::{SessionKeys, SessionSecret, Ticket};
use tazamun::state::RelPath;
use tazamun::sync::index::sanitize_rel_path;

fn hostile_record() -> FileRecord {
    FileRecord {
        size: 4,
        manifest: ManifestRef::Inline(vec![tazamun::proto::ChunkRef {
            hash: [9u8; 32],
            len: 4,
        }]),
        vv: std::collections::BTreeMap::from([("attacker".to_string(), 9u64)]),
        deleted: false,
        updated_at_ms: 1,
    }
}

/// Builds a RelPath that skips sanitization, exactly like a hostile peer
/// serializing arbitrary strings on the wire.
fn wire_path(s: &str) -> RelPath {
    let msg = Msg::LockRelease {
        path: serde_json::from_value(serde_json::Value::String(s.to_string()))
            .expect("RelPath deserializes any string"),
    };
    match msg {
        Msg::LockRelease { path } => path,
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_paths_in_index_fully_ignored() {
    let a = TestNode::init().await;
    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    assert!(
        wait_until(|| async { a.online_peers().await >= 1 }, WAIT).await,
        "raw peer not connected"
    );

    let evil = vec![
        "../pwn",
        "/etc/passwd",
        "C:\\pwn",
        "a\\b",
        "..",
        "x/../../pwn",
        ".tazamun/state.json",
        "nul\0byte",
        "",
    ];
    let files: Vec<(RelPath, FileRecord)> = evil
        .iter()
        .map(|p| (wire_path(p), hostile_record()))
        .collect();
    raw.send_msg(&Msg::Index {
        lamport: 99,
        files,
        leases: vec![],
    })
    .await;

    // Give the daemon time to process (and, if broken, to act on) the index.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Nothing escaped the root, nothing was scheduled, the node still works.
    let parent = a.root().parent().expect("temp parent");
    assert!(
        !parent.join("pwn").exists(),
        "path traversal escaped the root"
    );
    assert!(!a.root().join("pwn").exists());
    assert!(!std::path::Path::new("/etc/tazamun-pwn").exists());
    let status = a.status().await;
    assert_eq!(status["file_count"].as_u64(), Some(0));
    assert_eq!(
        status["pending_pulls"].as_array().map(Vec::len),
        Some(0),
        "hostile records must be dropped whole, never queued"
    );
    // state.json is still loadable — the reserved-path record never touched it.
    assert!(tazamun::state::AppState::load(a.root()).is_ok());

    raw.close();
    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_frame_closes_connection() {
    let a = TestNode::init().await;
    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    assert!(
        wait_until(|| async { a.online_peers().await >= 1 }, WAIT).await,
        "raw peer not connected"
    );

    // Header claims 4 MiB + 1: the node must drop the connection on the
    // header alone, before any body arrives.
    let oversized = (tazamun::consts::MAX_FRAME as u32) + 1;
    let _ = raw.send_raw_frame(oversized, &[0u8; 64]).await;

    // The control connection must be torn down. (Membership `online` lingers
    // for ONLINE_WINDOW via last_seen, so assert on the live connection.)
    assert!(
        wait_until(|| async { a.connected_peers().await == 0 }, WAIT).await,
        "node kept the connection after an oversized frame"
    );
    // And the daemon itself is unharmed.
    assert!(a.status().await["id"].is_string());

    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_ticket_rejected() {
    let a = TestNode::init().await;
    let good = a.invite().await;
    assert!(Ticket::decode(&good).is_ok());

    // Wrong prefix.
    let bad_prefix = format!("xzm1{}", &good[4..]);
    assert!(matches!(
        Ticket::decode(&bad_prefix),
        Err(tazamun::session::TicketError::BadPrefix)
    ));

    // Illegal character.
    let bad_char = format!("{}!", &good[..good.len() - 1]);
    assert!(matches!(
        Ticket::decode(&bad_char),
        Err(tazamun::session::TicketError::BadEncoding)
    ));

    // Truncation.
    assert!(Ticket::decode(&good[..good.len() - 5]).is_err());

    // Bit-flip inside the SECRET region (right after the 4-char prefix and
    // the 1-byte version varint). Either decoding breaks, or the secret is
    // now different — a wrong secret the handshake rejects. Tamper is never a
    // silent no-op on the secret.
    let orig = Ticket::decode(&good).expect("good decodes");
    let mut chars: Vec<char> = good.chars().collect();
    let target = 8; // base32 char covering secret bytes
    chars[target] = if chars[target] == 'a' { 'b' } else { 'a' };
    let flipped: String = chars.into_iter().collect();
    match Ticket::decode(&flipped) {
        Err(_) => {}
        Ok(t) => assert_ne!(
            t.secret.0, orig.secret.0,
            "a flip in the secret region must change the secret"
        ),
    }

    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_handshake_fails_both_directions() {
    // Direction 1: an initiator with a forged proof never gets post-auth data.
    let a = TestNode::init().await;
    let invite = a.invite().await;
    let got_index = RawPeer::probe_with_wrong_secret(&invite).await;
    assert!(!got_index, "node accepted a forged initiator proof");
    assert_eq!(a.online_peers().await, 0);

    // Direction 2: the node dials an acceptor that answers with a wrong
    // secret; the node must abort before completing the handshake.
    let raw_endpoint =
        tazamun::net::endpoint::build_endpoint(iroh::SecretKey::generate(), &common::test_net())
            .await
            .expect("raw endpoint");
    let raw_id = raw_endpoint.id();
    let raw_addr = raw_endpoint.addr();

    let dir = tempfile::tempdir().expect("tempdir");
    tazamun::cli::join(dir.path(), &invite).expect("join");
    seed_known_member(dir.path(), raw_id, &raw_addr);

    let acceptor = tokio::spawn(RawPeer::accept_with_wrong_secret(raw_endpoint));
    let b = TestNode::start(dir).await;

    let handshake_completed = tokio::time::timeout(WAIT, acceptor)
        .await
        .expect("acceptor did not finish")
        .expect("acceptor task");
    assert!(
        !handshake_completed,
        "node completed a handshake with a wrong-secret acceptor"
    );

    // The wrong-secret endpoint never becomes an authenticated peer.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = b.status().await;
    let authed = status["members"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter(|e| e["conn"].as_str() != Some("None"))
                .count()
        })
        .unwrap_or(0);
    // B is legitimately connected to A (same session); the raw endpoint must
    // not appear as connected.
    assert!(
        authed <= 1,
        "wrong-secret peer shows as authenticated: {status}"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

/// Wrong-secret matrix over the real handshake: initiator wrong / acceptor
/// wrong / both-wrong all fail closed, and the initiator's error is always the
/// same generic reason — no oracle distinguishes a bad proof from a wrong peer.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_matrix_fails_closed_without_oracle() {
    let good = SessionKeys::derive(&SessionSecret([1u8; 32]));
    let wrong1 = SessionKeys::derive(&SessionSecret([2u8; 32]));
    let wrong2 = SessionKeys::derive(&SessionSecret([3u8; 32]));

    // Control: matching secrets authenticate.
    let (i, a) = handshake_outcome(good.clone(), good.clone()).await;
    assert!(
        i.is_ok() && a.is_ok(),
        "matching secrets must authenticate: init={i:?} acc={a:?}"
    );

    for (label, ik, ak) in [
        ("initiator wrong", wrong1.clone(), good.clone()),
        ("acceptor wrong", good.clone(), wrong1.clone()),
        ("both wrong (different)", wrong1.clone(), wrong2.clone()),
    ] {
        let (i, a) = handshake_outcome(ik, ak).await;
        assert!(i.is_err(), "{label}: initiator must fail closed: {i:?}");
        assert!(a.is_err(), "{label}: acceptor must fail closed: {a:?}");
        // The initiator always reaches the verify step first, so it returns the
        // generic "handshake failed" regardless of which side is wrong.
        assert_eq!(
            i.unwrap_err(),
            "handshake failed",
            "{label}: initiator error must be the generic reason (no oracle)"
        );
    }
}

/// Replay: a proof recorded from one valid handshake, replayed against a fresh
/// session (fresh acceptor nonce), must be rejected — proofs bind both nonces.
#[tokio::test(flavor = "multi_thread")]
async fn recorded_proof_replayed_into_fresh_session_is_rejected() {
    let a = TestNode::init().await;
    let (first_authed, replay_rejected) = RawPeer::capture_then_replay(&a.invite().await).await;
    assert!(
        first_authed,
        "a valid handshake must authenticate (control)"
    );
    assert!(
        replay_rejected,
        "a recorded proof replayed against fresh nonces must be rejected"
    );
    // The daemon is unharmed by the replay attempt.
    assert!(a.status().await["id"].is_string());
    a.handle.shutdown().await;
}

/// Nonce freshness: the node's `nonce_b` is fresh on every handshake (a
/// statistical sanity check — no repeats across many handshakes).
#[tokio::test(flavor = "multi_thread")]
async fn acceptor_nonces_are_fresh_across_handshakes() {
    let a = TestNode::init().await;
    let invite = a.invite().await;
    let want = 24usize;
    let mut seen: std::collections::HashSet<[u8; 16]> = std::collections::HashSet::new();
    let mut attempts = 0;
    while seen.len() < want && attempts < want * 3 {
        attempts += 1;
        if let Some(nonce) = RawPeer::capture_acceptor_nonce(&invite).await {
            assert_ne!(nonce, [0u8; 16], "nonce_b must not be all-zero");
            seen.insert(nonce);
        }
    }
    assert_eq!(
        seen.len(),
        want,
        "every handshake must use a fresh nonce_b (no repeats)"
    );
    a.handle.shutdown().await;
}

/// The insider boundary: after a VALID handshake, a malicious authenticated
/// peer sends protocol-illegal sequences. Each is ignored, the daemon stays
/// healthy, the sync loop never wedges, and nothing un-verified is written.
#[tokio::test(flavor = "multi_thread")]
async fn insider_illegal_control_sequences_stay_healthy() {
    let a = TestNode::init().await;
    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    assert!(
        wait_until(|| async { a.online_peers().await >= 1 }, WAIT).await,
        "raw peer not connected"
    );

    // 1. LockGrant for a path we never requested — ignored (not a voter grant).
    raw.send_msg(&Msg::LockGrant {
        path: wire_path("victim.txt"),
    })
    .await;
    // 2. LockRenew for a lease we do not hold — ignored (holder mismatch).
    raw.send_msg(&Msg::LockRenew {
        path: wire_path("victim.txt"),
        lamport: 7,
        ttl_ms: 90_000,
    })
    .await;
    // 3. FileMeta advertising content the insider cannot serve (the raw
    //    endpoint runs no blobs protocol) — the pull fails, nothing is written.
    raw.send_msg(&Msg::FileMeta {
        path: wire_path("insider.txt"),
        record: hostile_record(),
        lamport: 40,
    })
    .await;

    tokio::time::sleep(Duration::from_secs(2)).await;

    let status = a.status().await;
    assert_eq!(
        status["file_count"].as_u64(),
        Some(0),
        "insider content must not be written"
    );
    let leases = status["leases"].as_array().cloned().unwrap_or_default();
    assert!(
        leases.is_empty(),
        "illegal lock messages must not create a lease: {leases:?}"
    );
    assert!(status["id"].is_string(), "daemon must stay responsive");
    assert!(
        a.connected_peers().await >= 1,
        "peer wrongly dropped for ignorable messages"
    );
    assert!(a.read_file("insider.txt").is_none());

    raw.close();
    a.handle.shutdown().await;
}

/// A hostile `Index` advertising hundreds of valid-but-unservable paths must
/// not exceed the concurrent-pull cap, must write nothing (no content is
/// verifiable), and must not wedge the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_index_flood_respects_pull_cap() {
    let a = TestNode::init().await;
    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    assert!(
        wait_until(|| async { a.online_peers().await >= 1 }, WAIT).await,
        "raw peer not connected"
    );

    let n = 300u32;
    let files: Vec<(RelPath, FileRecord)> = (0..n)
        .map(|i| {
            let rel = sanitize_rel_path(&format!("flood/f{i}.bin")).expect("valid path");
            let rec = FileRecord {
                size: 4,
                manifest: ManifestRef::Inline(vec![tazamun::proto::ChunkRef {
                    hash: blake3::hash(&i.to_le_bytes()).into(),
                    len: 4,
                }]),
                vv: std::collections::BTreeMap::from([("attacker".to_string(), 1u64)]),
                deleted: false,
                updated_at_ms: 1,
            };
            (rel, rec)
        })
        .collect();
    raw.send_msg(&Msg::Index {
        lamport: 5,
        files,
        leases: vec![],
    })
    .await;

    // The concurrent-pull cap must hold at every observation.
    let cap = tazamun::consts::MAX_CONCURRENT_PULLS;
    for _ in 0..20 {
        let active = a.status().await["pending_pulls"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        assert!(
            active <= cap,
            "active pulls {active} exceeded MAX_CONCURRENT_PULLS {cap}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(
        a.file_count().await,
        0,
        "unservable flood must not write files"
    );
    assert!(
        a.status().await["id"].is_string(),
        "daemon must stay healthy under the flood"
    );

    raw.close();
    a.handle.shutdown().await;
}

/// Wire path traversal via `FileMeta` (not just `Index`): `../`, absolute,
/// drive-letter, backslash, NUL, reserved, and overlong paths are dropped whole
/// by the sanitizer — never materialized, never queued.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_paths_in_filemeta_dropped_whole() {
    let a = TestNode::init().await;
    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    assert!(
        wait_until(|| async { a.online_peers().await >= 1 }, WAIT).await,
        "raw peer not connected"
    );

    let mut evil: Vec<String> = [
        "../pwn",
        "/etc/passwd",
        "C:\\pwn",
        "a\\b",
        "..",
        "x/../../pwn",
        ".tazamun/state.json",
        "n\0ul",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Overlong path (> MAX_PATH_LEN) must also be dropped whole.
    evil.push(format!(
        "{}/deep",
        "a".repeat(tazamun::consts::MAX_PATH_LEN + 8)
    ));

    for p in &evil {
        raw.send_msg(&Msg::FileMeta {
            path: wire_path(p),
            record: hostile_record(),
            lamport: 3,
        })
        .await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let parent = a.root().parent().expect("temp parent");
    assert!(
        !parent.join("pwn").exists(),
        "traversal escaped the root via FileMeta"
    );
    let status = a.status().await;
    assert_eq!(status["file_count"].as_u64(), Some(0));
    assert_eq!(
        status["pending_pulls"].as_array().map(Vec::len),
        Some(0),
        "hostile FileMeta must be dropped whole, never queued"
    );
    assert!(tazamun::state::AppState::load(a.root()).is_ok());

    raw.close();
    a.handle.shutdown().await;
}
