//! Long-path support: the full sync/lease/violation/restore cycle on a
//! relative path far past the legacy Windows 260-char MAX_PATH. Runs on every
//! OS; on the Windows self-hosted runner it proves the `\\?\` boundary
//! conversion (and the embedded longPathAware manifest) end-to-end.

mod common;

use common::{TestNode, WAIT, wait_until};

/// A >300-char relative path: 7 nested 40-char segments + a file name.
fn deep_rel() -> String {
    let seg = "d".repeat(40);
    let mut parts = vec![seg.clone(); 7];
    parts.push("payload-file.bin".to_string());
    let rel = parts.join("/");
    assert!(rel.len() > 300, "test path must exceed 300 chars");
    rel
}

#[tokio::test(flavor = "multi_thread")]
async fn full_cycle_on_a_path_past_max_path() {
    let rel = deep_rel();

    // Genesis import of the deep path on A.
    let dir = TestNode::init_dir();
    let abs = dir
        .path()
        .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    std::fs::create_dir_all(abs.parent().unwrap()).expect("mkdir deep tree");
    std::fs::write(&abs, b"v1-deep").expect("write deep genesis");
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;

    // Publish + sync to the second node, byte-exact and read-only.
    assert!(
        wait_until(
            || async { b.read_file(&rel).as_deref() == Some(b"v1-deep") },
            WAIT
        )
        .await,
        "deep path did not sync to B"
    );
    assert!(b.is_readonly(&rel), "synced deep file must be read-only");

    // Lease cycle on the deep path: B locks, edits, unlocks; A receives.
    b.lock_ok(&rel).await;
    b.write_file(&rel, b"v2-by-B");
    b.unlock_ok(&rel).await;
    assert!(
        wait_until(
            || async { a.read_file(&rel).as_deref() == Some(b"v2-by-B") },
            WAIT
        )
        .await,
        "deep-path edit did not round-trip to A"
    );
    assert!(
        a.is_readonly(&rel),
        "read-only re-applied after remote edit"
    );

    // Violation on the deep path: un-leased force-write on A → quarantined,
    // indexed version restored read-only. Wait out the daemon's MUTE_WINDOW
    // first — A just applied B's edit, and its own writes suppress watcher
    // events for 2s (the documented force-write caveat from the P0 smoke).
    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    let before = a.conflict_count();
    a.force_write(&rel, b"forced-unleased");
    assert!(
        wait_until(
            || async {
                a.conflict_count() > before && a.read_file(&rel).as_deref() == Some(b"v2-by-B")
            },
            WAIT,
        )
        .await,
        "deep-path violation was not quarantined + restored"
    );
    assert!(a.is_readonly(&rel), "restored deep file must be read-only");
    // The quarantine copy holds the forced bytes.
    assert!(
        a.conflict_contents()
            .iter()
            .any(|c| c.as_slice() == b"forced-unleased"),
        "forced bytes preserved in quarantine"
    );

    // Restore an old version under a lease (needs history pushed by the edit).
    b.lock_ok(&rel).await;
    let resp = b
        .handle
        .request(tazamun::ipc::IpcRequest::Restore {
            path: rel.clone(),
            n: 0,
        })
        .await;
    assert!(resp.ok, "restore on deep path failed: {resp:?}");
    b.unlock_ok(&rel).await;
    assert!(
        wait_until(
            || async { a.read_file(&rel).as_deref() == Some(b"v1-deep") },
            WAIT
        )
        .await,
        "restored deep version did not propagate"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
