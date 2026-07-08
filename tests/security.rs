//! Adversarial behavior: hostile paths, oversized frames, forged tickets and
//! wrong-secret handshakes.

mod common;

use std::time::Duration;

use common::{RawPeer, TestNode, WAIT, seed_known_member, wait_until};
use tazamun::proto::{FileRecord, ManifestRef, Msg};
use tazamun::session::Ticket;
use tazamun::state::RelPath;

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
