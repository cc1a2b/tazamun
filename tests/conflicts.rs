//! P18 conflict center: a forced write is quarantined with a reason, listed
//! with its original path, and resolvable (keep mine / keep theirs) end to end.

mod common;

use std::time::Duration;

use common::{TestNode, WAIT, assert_converged, wait_until};
use tazamun::ipc::IpcRequest;

/// Sleeps just past the watcher's self-write mute window, so a forced write
/// after a fresh materialize is seen as a real edit (not swallowed as ours).
async fn past_mute() {
    tokio::time::sleep(tazamun::consts::MUTE_WINDOW + Duration::from_millis(400)).await;
}

/// Reads the daemon's structured conflict list.
async fn conflicts(node: &TestNode) -> Vec<serde_json::Value> {
    let r = node.handle.request(IpcRequest::Conflicts).await;
    assert!(r.ok, "conflicts query failed: {r:?}");
    r.data.unwrap()["conflicts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn forced_write_is_quarantined_with_reason_then_resolved() {
    // A and B share one file.
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.online_peers().await >= 1 && b.online_peers().await >= 1 },
            WAIT
        )
        .await,
        "no connect"
    );
    a.lock_ok("doc.txt").await;
    a.write_file("doc.txt", b"original");
    a.unlock_ok("doc.txt").await;
    assert_converged(&a, &b).await;
    assert!(b.is_readonly("doc.txt"));
    past_mute().await;

    // B force-writes the read-only file: an un-leased edit → quarantined and
    // the indexed version restored.
    b.force_write("doc.txt", b"scribbled over it");
    assert!(
        wait_until(|| async { !conflicts(&b).await.is_empty() }, WAIT).await,
        "the forced write was not quarantined"
    );
    let list = conflicts(&b).await;
    assert_eq!(list.len(), 1);
    let c = &list[0];
    assert_eq!(
        c["path"].as_str(),
        Some("doc.txt"),
        "original path recorded"
    );
    assert_eq!(
        c["reason"].as_str(),
        Some("forced-write"),
        "reason recorded in the index"
    );
    assert!(
        c["both_name"].as_str().is_some(),
        "a keep-both name is suggested"
    );
    // The indexed bytes were restored on disk.
    assert_eq!(b.read_file("doc.txt").unwrap(), b"original");
    let id = c["name"].as_str().unwrap().to_string();

    // Resolve "keep mine": lock → apply quarantined bytes → publish → discard.
    b.lock_ok("doc.txt").await;
    let ap = b
        .handle
        .request(IpcRequest::ConflictApply {
            id: id.clone(),
            target: "doc.txt".into(),
        })
        .await;
    assert!(ap.ok, "apply failed: {ap:?}");
    b.unlock_ok("doc.txt").await;
    let disc = b
        .handle
        .request(IpcRequest::ConflictDiscard { id: id.clone() })
        .await;
    assert!(disc.ok, "discard failed: {disc:?}");

    // B's bytes won and propagated to A; the quarantine is empty again.
    assert!(
        wait_until(
            || async { a.read_file("doc.txt").as_deref() == Some(b"scribbled over it") },
            WAIT
        )
        .await,
        "the kept-mine bytes did not sync to A"
    );
    assert!(
        conflicts(&b).await.is_empty(),
        "copy discarded after resolve"
    );

    // Apply/discard of a bogus id is refused, not a panic.
    let bad = b
        .handle
        .request(IpcRequest::ConflictDiscard {
            id: "../etc/passwd".into(),
        })
        .await;
    assert!(!bad.ok, "path traversal id must be refused");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_both_restores_as_a_new_file() {
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.online_peers().await >= 1 && b.online_peers().await >= 1 },
            WAIT
        )
        .await,
        "no connect"
    );
    a.lock_ok("doc.txt").await;
    a.write_file("doc.txt", b"original");
    a.unlock_ok("doc.txt").await;
    assert_converged(&a, &b).await;
    past_mute().await;

    b.force_write("doc.txt", b"my rival edit");
    assert!(
        wait_until(|| async { !conflicts(&b).await.is_empty() }, WAIT).await,
        "not quarantined"
    );
    let c = conflicts(&b).await;
    let id = c[0]["name"].as_str().unwrap().to_string();
    let both = c[0]["both_name"]
        .as_str()
        .expect("a keep-both name")
        .to_string();
    assert!(
        both.starts_with("doc.conflict-") && both.ends_with(".txt"),
        "{both}"
    );

    // keep both: lock the fresh name, apply the quarantined bytes, publish.
    b.lock_ok(&both).await;
    let ap = b
        .handle
        .request(IpcRequest::ConflictApply {
            id: id.clone(),
            target: both.clone(),
        })
        .await;
    assert!(ap.ok, "apply failed: {ap:?}");
    b.unlock_ok(&both).await;

    // The new file carries the quarantined bytes; the original is untouched;
    // both propagate to A.
    assert_eq!(b.read_file("doc.txt").unwrap(), b"original");
    assert_eq!(b.read_file(&both).unwrap(), b"my rival edit");
    assert!(
        wait_until(
            || async { a.read_file(&both).as_deref() == Some(b"my rival edit") },
            WAIT
        )
        .await,
        "the keep-both file did not sync to A"
    );

    // Applying into a path that exists but ISN'T a synced file is refused
    // (Golden Invariant: never overwrite unrecoverable bytes). doc.txt IS
    // indexed, so a lease + apply there is allowed; but a bogus id is refused.
    let refused = b
        .handle
        .request(IpcRequest::ConflictApply {
            id: "C:evil".into(),
            target: "doc.txt".into(),
        })
        .await;
    assert!(!refused.ok, "a containment-escaping id must be refused");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_theirs_discards_the_copy() {
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.online_peers().await >= 1 && b.online_peers().await >= 1 },
            WAIT
        )
        .await,
        "no connect"
    );
    a.lock_ok("f.txt").await;
    a.write_file("f.txt", b"canon");
    a.unlock_ok("f.txt").await;
    assert_converged(&a, &b).await;
    past_mute().await;

    b.force_write("f.txt", b"junk");
    assert!(
        wait_until(|| async { !conflicts(&b).await.is_empty() }, WAIT).await,
        "not quarantined"
    );
    let id = conflicts(&b).await[0]["name"].as_str().unwrap().to_string();

    // Keep theirs = discard the quarantined copy; the synced bytes stay.
    let d = b.handle.request(IpcRequest::ConflictDiscard { id }).await;
    assert!(d.ok);
    assert!(conflicts(&b).await.is_empty());
    assert_eq!(b.read_file("f.txt").unwrap(), b"canon");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
