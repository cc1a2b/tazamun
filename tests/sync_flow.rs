//! End-to-end sync flows between real in-process nodes (fully offline).

mod common;

use std::time::Duration;

use common::{TestNode, WAIT, assert_converged, pseudo_random, wait_until};

#[tokio::test(flavor = "multi_thread")]
async fn initial_sync_converges_bidirectional() {
    // A starts with genesis files created before its first start.
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("hello.txt"), b"salam from A").unwrap();
    std::fs::create_dir_all(dir.path().join("nested/deep")).unwrap();
    std::fs::write(dir.path().join("nested/deep/data.bin"), vec![7u8; 4096]).unwrap();
    let a = TestNode::start(dir).await;

    assert!(
        wait_until(|| async { a.file_count().await == 2 }, WAIT).await,
        "genesis import did not finish"
    );

    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.online_peers().await >= 1 && b.online_peers().await >= 1 },
            WAIT
        )
        .await,
        "nodes did not connect"
    );

    // A → B initial sync.
    assert_converged(&a, &b).await;
    assert_eq!(b.read_file("hello.txt").unwrap(), b"salam from A");
    assert!(b.is_readonly("hello.txt"), "synced files must be read-only");
    assert!(b.is_readonly("nested/deep/data.bin"));

    // B → A: a brand-new file created under a lease.
    b.lock_ok("from-b.txt").await;
    b.write_file("from-b.txt", b"marhaba from B");
    b.unlock_ok("from-b.txt").await;
    assert_converged(&a, &b).await;
    assert_eq!(a.read_file("from-b.txt").unwrap(), b"marhaba from B");
    assert!(a.is_readonly("from-b.txt"));
    assert!(b.is_readonly("from-b.txt"), "unlock re-applies read-only");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_and_zero_length_files() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("empty.bin"), b"").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;

    assert!(
        wait_until(|| async { b.read_file("empty.bin").is_some() }, WAIT).await,
        "zero-length file did not sync"
    );
    assert_eq!(b.read_file("empty.bin").unwrap().len(), 0);
    assert!(b.is_readonly("empty.bin"));

    // A zero-length file created live under a lease also syncs.
    b.lock_ok("also-empty.txt").await;
    b.write_file("also-empty.txt", b"");
    b.unlock_ok("also-empty.txt").await;
    assert!(
        wait_until(
            || async { a.read_file("also-empty.txt").map(|d| d.len()) == Some(0) },
            WAIT
        )
        .await,
        "live zero-length file did not sync"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_propagates_tombstone() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("doomed.txt"), b"short life").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("doomed.txt").is_some() }, WAIT).await,
        "file did not reach B"
    );

    // Delete under a lease on A; B must apply the tombstone.
    a.lock_ok("doomed.txt").await;
    a.delete_file("doomed.txt");
    assert!(
        wait_until(|| async { a.file_count().await == 0 }, WAIT).await,
        "tombstone not committed on A"
    );
    a.unlock_ok("doomed.txt").await;

    assert!(
        wait_until(|| async { b.read_file("doomed.txt").is_none() }, WAIT).await,
        "delete did not propagate to B"
    );
    assert_eq!(b.file_count().await, 0);

    // The tombstone stays causal: the file does not resurrect on reconnect.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(b.read_file("doomed.txt").is_none());
    assert!(a.read_file("doomed.txt").is_none());

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delta_edit_transfers_under_20_percent() {
    const SIZE: usize = 32 * 1024 * 1024;
    const EDIT: usize = 1024 * 1024;

    let dir = TestNode::init_dir();
    let mut data = pseudo_random(SIZE, 0xA5A5_5A5A_DEAD_BEEF);
    std::fs::write(dir.path().join("big.bin"), &data).unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;

    let big_synced = |node: &TestNode, want: &[u8]| {
        let got = node.read_file("big.bin");
        got.is_some_and(|d| d.len() == want.len() && blake3::hash(&d) == blake3::hash(want))
    };
    // On timeout, dump both nodes' full state so CI failures are diagnosable.
    async fn dump(label: &str, a: &TestNode, b: &TestNode, want_len: usize) {
        eprintln!("--- {label}: A status ---\n{}", a.status().await);
        eprintln!("--- {label}: B status ---\n{}", b.status().await);
        let b_len = b.read_file("big.bin").map(|d| d.len());
        eprintln!("--- {label}: B big.bin len {b_len:?}, want {want_len}");
    }
    let ok = wait_until(|| async { big_synced(&b, &data) }, Duration::from_secs(120)).await;
    if !ok {
        dump("initial sync timeout", &a, &b, data.len()).await;
    }
    assert!(ok, "32 MiB initial sync did not complete");

    // Byte accounting: measure the receiver's blob store before the edit.
    let before = b.blobs_dir_size();

    // In-place 1 MiB overwrite in the middle of the file.
    a.lock_ok("big.bin").await;
    let start = SIZE / 2;
    for (i, byte) in data[start..start + EDIT].iter_mut().enumerate() {
        *byte = byte.wrapping_add(i as u8).wrapping_add(1);
    }
    a.write_file("big.bin", &data);
    a.unlock_ok("big.bin").await;

    let ok = wait_until(|| async { big_synced(&b, &data) }, Duration::from_secs(120)).await;
    if !ok {
        dump("delta edit timeout", &a, &b, data.len()).await;
    }
    assert!(ok, "delta edit did not sync");

    let growth = b.blobs_dir_size().saturating_sub(before);
    let budget = (SIZE as u64) / 5;
    assert!(
        growth < budget,
        "blob store grew by {growth} bytes; delta budget is {budget} (20% of file)"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
