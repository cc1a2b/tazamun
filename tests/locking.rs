//! Strict exclusive-checkout semantics across real nodes.

mod common;

use std::time::Duration;

use common::{RawPeer, TestNode, WAIT, WAIT_MESH, wait_until};

#[tokio::test(flavor = "multi_thread")]
async fn deny_while_held_then_grant_after_release() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("f.txt"), b"contended").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("f.txt").is_some() }, WAIT).await,
        "initial sync"
    );

    a.lock_ok("f.txt").await;
    let denied = b.lock("f.txt").await;
    assert!(!denied.ok);
    assert_eq!(
        denied.error.as_ref().map(|e| e.code.as_str()),
        Some("lease_held"),
        "{denied:?}"
    );

    a.unlock_ok("f.txt").await;
    // B may need a moment to observe the release.
    assert!(
        wait_until(|| async { b.lock("f.txt").await.ok }, WAIT).await,
        "lock after release failed"
    );
    b.unlock_ok("f.txt").await;

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_race_single_winner_consistent_on_both() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("race.txt"), b"go").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("race.txt").is_some() }, WAIT).await,
        "initial sync"
    );

    let (ra, rb) = tokio::join!(a.lock("race.txt"), b.lock("race.txt"));
    assert!(
        ra.ok ^ rb.ok,
        "exactly one node must win the race: a={ra:?} b={rb:?}"
    );

    // Both nodes agree on the holder.
    let winner = if ra.ok { a.id() } else { b.id() }.to_string();
    let holder_on = |status: serde_json::Value| {
        status["leases"].as_array().and_then(|l| {
            l.first()
                .map(|e| e["holder"].as_str().unwrap_or("").to_string())
        })
    };
    let sa = holder_on(a.status().await);
    let sb = holder_on(b.status().await);
    assert_eq!(sa.as_deref(), Some(winner.as_str()), "a status: {sa:?}");
    assert_eq!(sb.as_deref(), Some(winner.as_str()), "b status: {sb:?}");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_peers_lock_refused() {
    let a = TestNode::init().await;
    let resp = a.lock("anything.txt").await;
    assert!(!resp.ok);
    assert_eq!(
        resp.error.as_ref().map(|e| e.code.as_str()),
        Some("strict_offline"),
        "{resp:?}"
    );
    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn voter_disconnect_aborts() {
    let a = TestNode::init().await;
    // A mute peer: authenticates, sends an empty index, then never votes.
    let mute = RawPeer::connect_authed(&a.invite().await).await;
    // Wait until the peer is a usable voter — connected AND its index received —
    // so the lock passes the FRESHNESS precondition and actually enters the
    // pending state (otherwise it is correctly refused with "syncing").
    assert!(
        wait_until(|| async { a.synced_peers().await >= 1 }, WAIT).await,
        "mute peer index not exchanged"
    );

    let started = std::time::Instant::now();
    let lock_fut = a.lock("stuck.txt");
    let drop_fut = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        mute.close();
    };
    let (resp, ()) = tokio::join!(lock_fut, drop_fut);
    let elapsed = started.elapsed();

    assert!(!resp.ok, "lock must fail when the only voter vanishes");
    assert_eq!(
        resp.error.as_ref().map(|e| e.code.as_str()),
        Some("voter_lost"),
        "{resp:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "abort must be driven by the disconnect, not the timeout ({elapsed:?})"
    );
    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn holder_crash_ttl_expiry_allows_takeover() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("crashy.txt"), b"held tight").unwrap();
    let a = TestNode::start(dir).await;
    let invite = a.invite().await;
    let b = TestNode::join(&invite).await;
    let c = TestNode::join(&invite).await;
    // Full mesh: B and C only learn each other through gossip presence, so this
    // can take a couple of beacon intervals. Also require A to have both peers'
    // indexes so the lock below passes FRESHNESS without a race.
    assert!(
        wait_until(
            || async {
                b.read_file("crashy.txt").is_some()
                    && c.read_file("crashy.txt").is_some()
                    && b.online_peers().await >= 2
                    && c.online_peers().await >= 2
                    && a.synced_peers().await >= 2
            },
            WAIT_MESH
        )
        .await,
        "mesh did not form"
    );

    a.lock_ok("crashy.txt").await;
    let denied = b.lock("crashy.txt").await;
    assert!(!denied.ok, "lease is held by A: {denied:?}");

    // A crashes without releasing: no Bye, no LockRelease.
    a.handle.kill().await;

    // Within the 2s test TTL (plus sweep slack) the lease frees and B takes
    // over with C as the remaining voter.
    assert!(
        wait_until(|| async { b.lock("crashy.txt").await.ok }, WAIT).await,
        "takeover after TTL expiry failed"
    );
    b.unlock_ok("crashy.txt").await;

    b.handle.shutdown().await;
    c.handle.shutdown().await;
}
