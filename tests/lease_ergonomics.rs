//! Phase 4 integration tests: consensus-safe configurable TTL, autolock, and
//! the lock waitlist — exercised across real nodes.

mod common;

use std::time::Duration;

use common::{TestNode, WAIT, WAIT_MESH, set_autolock, test_net, wait_until};
use tazamun::ipc::IpcRequest;
use tazamun::locks::LockTimings;

/// TTL is lease-scoped: a holder with a long TTL is honored by a peer whose own
/// configured TTL is much shorter, proving nodes may run different configs
/// without protocol divergence.
#[tokio::test(flavor = "multi_thread")]
async fn long_ttl_is_honored_by_a_peer_with_shorter_config() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("f.txt"), b"x").unwrap();
    // A holds with a 300s TTL; B runs the fast 2s test config.
    let a = TestNode::start_with_timings(
        dir,
        test_net(),
        LockTimings {
            ttl: Duration::from_secs(300),
            renew: Duration::from_secs(100),
            acquire_timeout: Duration::from_secs(3),
        },
    )
    .await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("f.txt").is_some() }, WAIT).await,
        "initial sync"
    );

    a.lock_ok("f.txt").await;
    // B must record the lease with the holder's 300s TTL, not clamp it to its
    // own 2s config.
    let honored = wait_until(
        || async {
            b.status().await["leases"]
                .as_array()
                .and_then(|l| l.first())
                .and_then(|e| e["expires_in_ms"].as_u64())
                .map(|ms| ms > 100_000)
                .unwrap_or(false)
        },
        WAIT,
    )
    .await;
    assert!(honored, "B did not honor the holder's long TTL");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

/// Autolock happy path: with autolock on, an un-leased edit auto-acquires the
/// lease and publishes, so it syncs to the peer.
#[tokio::test(flavor = "multi_thread")]
async fn autolock_publishes_an_unleased_edit() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("auto.txt"), b"v1").unwrap();
    set_autolock(dir.path(), true);
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { b.read_file("auto.txt").as_deref() == Some(b"v1") },
            WAIT
        )
        .await,
        "initial sync"
    );

    // Un-leased force write; autolock acquires the lease and publishes it.
    a.force_write("auto.txt", b"v2-by-autolock");
    assert!(
        wait_until(
            || async { b.read_file("auto.txt").as_deref() == Some(b"v2-by-autolock") },
            WAIT_MESH,
        )
        .await,
        "autolock edit did not sync to the peer"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

/// Autolock race: both nodes force-write the same path at once. Exactly one
/// wins and publishes; the loser reverts and preserves its bytes in
/// quarantine. The Golden Invariant outranks convenience.
#[tokio::test(flavor = "multi_thread")]
async fn autolock_race_leaves_one_winner_and_a_quarantine() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("race.txt"), b"base").unwrap();
    set_autolock(dir.path(), true);
    let a = TestNode::start(dir).await;

    let bdir = tempfile::tempdir().unwrap();
    tazamun::cli::join(bdir.path(), &a.invite().await).expect("join");
    set_autolock(bdir.path(), true);
    let b = TestNode::start_with(bdir, test_net()).await;
    assert!(
        wait_until(
            || async { b.read_file("race.txt").as_deref() == Some(b"base") },
            WAIT
        )
        .await,
        "initial sync"
    );

    // Both write un-leased, concurrently.
    a.force_write("race.txt", b"from-A");
    b.force_write("race.txt", b"from-B");

    // Converge to a single winner's bytes (not the base), on both nodes.
    let converged = wait_until(
        || async {
            let ca = a.read_file("race.txt");
            let cb = b.read_file("race.txt");
            ca.is_some() && ca == cb && ca.as_deref() != Some(b"base")
        },
        WAIT_MESH,
    )
    .await;
    assert!(converged, "the race did not converge to one winner");

    // Golden Invariant: BOTH written variants are recoverable — the winner's on
    // disk, the loser's in a quarantine. Neither is silently overwritten.
    let winner = a.read_file("race.txt").unwrap();
    let mut recoverable: Vec<Vec<u8>> = vec![winner.clone()];
    recoverable.extend(a.conflict_contents());
    recoverable.extend(b.conflict_contents());
    assert!(
        recoverable.iter().any(|v| v.as_slice() == b"from-A"),
        "from-A must be recoverable"
    );
    assert!(
        recoverable.iter().any(|v| v.as_slice() == b"from-B"),
        "from-B (the loser) must be preserved, never silently overwritten"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

/// Waitlist handoff: A holds, B waitlists, A unlocks, B acquires. The holder
/// lists the waiter while it holds.
#[tokio::test(flavor = "multi_thread")]
async fn waitlist_handoff_from_holder_to_waiter() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("w.txt"), b"x").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("w.txt").is_some() }, WAIT).await,
        "initial sync"
    );

    a.lock_ok("w.txt").await;
    assert!(!b.lock("w.txt").await.ok, "B cannot lock while A holds");

    // B registers interest (what `lock --wait` does under the hood).
    let wr = b
        .handle
        .request(IpcRequest::LockWait {
            path: "w.txt".into(),
        })
        .await;
    assert!(wr.ok, "waitlist registration failed: {wr:?}");

    // A lists B as a waiter while it holds the lease.
    assert!(
        wait_until(
            || async {
                a.status().await["leases"]
                    .as_array()
                    .and_then(|l| l.first())
                    .and_then(|e| e["waiters"].as_array().map(|w| !w.is_empty()))
                    .unwrap_or(false)
            },
            WAIT,
        )
        .await,
        "holder should list the waiter"
    );

    // A unlocks; the waiter (retrying the acquire) now wins it.
    a.unlock_ok("w.txt").await;
    assert!(
        wait_until(|| async { b.lock("w.txt").await.ok }, WAIT).await,
        "waiter did not acquire after release"
    );
    b.unlock_ok("w.txt").await;

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

/// Waitlist + holder crash: with a third peer keeping reachability, a waiter
/// acquires once the crashed holder's lease expires by TTL.
#[tokio::test(flavor = "multi_thread")]
async fn waitlist_acquires_after_holder_crash_ttl_expiry() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("c.txt"), b"x").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    let c = TestNode::join(&a.invite().await).await;

    // All three mesh together (each sees two peers).
    assert!(
        wait_until(
            || async {
                a.connected_peers().await >= 2
                    && b.connected_peers().await >= 2
                    && c.connected_peers().await >= 2
            },
            WAIT_MESH,
        )
        .await,
        "three-node mesh did not form"
    );
    assert!(
        wait_until(
            || async { b.read_file("c.txt").is_some() && c.read_file("c.txt").is_some() },
            WAIT,
        )
        .await,
        "initial sync"
    );

    a.lock_ok("c.txt").await;
    assert!(!b.lock("c.txt").await.ok, "B cannot lock while A holds");

    // A crashes. B still has C for reachability, and A's lease expires by TTL
    // (the fast 2s test TTL), after which B acquires.
    a.handle.shutdown().await;
    assert!(
        wait_until(|| async { b.lock("c.txt").await.ok }, WAIT_MESH).await,
        "waiter did not acquire after the crashed holder's TTL expired"
    );
    b.unlock_ok("c.txt").await;

    b.handle.shutdown().await;
    c.handle.shutdown().await;
}
