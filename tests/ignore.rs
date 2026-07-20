//! P11 sync-scope semantics across real nodes: junk is left alone (not
//! quarantined, not synced), `.tazamunignore` syncs and governs every member,
//! and selective-sync / size holds keep records acknowledged but never
//! materialized.

mod common;

use std::time::Duration;

use common::{TestNode, WAIT, wait_until};

/// A pause long enough for the debounced watcher to have fired if it was
/// going to (debounce is 250ms): used to prove a negative.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn junk_files_are_left_alone_not_quarantined_not_synced() {
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("real.txt"), b"content").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("real.txt").is_some() }, WAIT).await,
        "baseline sync"
    );

    // The exact junk from field testing: a vim dot-swap. Pre-P11 this was
    // quarantined-and-removed with a VIOLATION warning; now it is simply not
    // session content.
    a.write_file(".real.txt.swp", b"vim scratch");
    settle().await;
    assert_eq!(
        a.read_file(".real.txt.swp").as_deref(),
        Some(&b"vim scratch"[..]),
        "junk file must stay on disk untouched"
    );
    assert_eq!(a.conflict_count(), 0, "junk must not be quarantined");
    assert!(
        b.read_file(".real.txt.swp").is_none(),
        "junk must not reach the peer"
    );
    // It is visible, not invisible: status lists it as held with a reason.
    let held = a.status().await["held_local"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        held.iter()
            .any(|h| h["path"] == ".real.txt.swp"
                && h["reason"].as_str().unwrap_or("").contains("junk")),
        "held_local should list the junk file: {held:?}"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tazamunignore_syncs_and_governs_every_member() {
    // Genesis includes the shared contract itself.
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join(".tazamunignore"), b"*.log\n").unwrap();
    std::fs::write(dir.path().join("keep.txt"), b"keep").unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file(".tazamunignore").is_some() }, WAIT).await,
        "the ignore file itself must sync"
    );

    // An explicit lease publishes even an ignored pattern on the origin
    // (explicit intent wins locally) — but the receiver's own scope, built
    // from the synced ignore file, holds it: acknowledged, never materialized.
    a.lock_ok("app.log").await;
    a.write_file("app.log", b"log bytes");
    a.unlock_ok("app.log").await;
    assert!(
        wait_until(
            || async {
                a.status().await["unapplied"] == serde_json::json!([])
                    && b.status().await["unapplied"]
                        .as_array()
                        .is_some_and(|u| u.iter().any(|e| e["path"] == "app.log"))
            },
            WAIT
        )
        .await,
        "receiver should hold app.log under its synced ignore rules"
    );
    assert!(
        b.read_file("app.log").is_none(),
        "held record must never be materialized"
    );
    let held = b.status().await["unapplied"].clone();
    assert!(
        held.as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "app.log"
                && e["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains(".tazamunignore")),
        "hold reason should name the ignore file: {held}"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn selective_sync_and_size_ceiling_hold_new_remote_paths() {
    let adir = TestNode::init_dir();
    let a = TestNode::start(adir).await;

    // B skips renders/ and caps new files at 1KB — set before its daemon starts.
    let bdir = tempfile::tempdir().unwrap();
    tazamun::cli::join(bdir.path(), &a.invite().await).expect("join");
    let mut st = tazamun::state::AppState::load(bdir.path()).unwrap();
    st.config.set_value("sync-skip", "renders").unwrap();
    st.config.set_value("max-file-size", "1KB").unwrap();
    st.save(bdir.path()).unwrap();
    let b = TestNode::start(bdir).await;
    assert!(
        wait_until(
            || async { a.synced_peers().await >= 1 && b.synced_peers().await >= 1 },
            WAIT
        )
        .await,
        "nodes did not sync indexes"
    );

    // Three publishes from A: in-scope, skipped subtree, over the ceiling.
    for (path, data) in [
        ("docs/ok.txt", b"fits".to_vec()),
        ("renders/frame.exr", b"render".to_vec()),
        ("big.bin", vec![7u8; 4096]),
    ] {
        a.lock_ok(path).await;
        a.write_file(path, &data);
        a.unlock_ok(path).await;
    }

    assert!(
        wait_until(|| async { b.read_file("docs/ok.txt").is_some() }, WAIT).await,
        "in-scope file should sync"
    );
    assert!(
        wait_until(
            || async {
                let held = b.status().await["unapplied"].clone();
                let held = held.as_array().cloned().unwrap_or_default();
                held.iter().any(|e| e["path"] == "renders/frame.exr")
                    && held.iter().any(|e| e["path"] == "big.bin")
            },
            WAIT
        )
        .await,
        "skipped subtree and oversize file should both be held"
    );
    assert!(b.read_file("renders/frame.exr").is_none());
    assert!(b.read_file("big.bin").is_none());
    // A — with no such scope — carries everything it published.
    assert!(a.read_file("renders/frame.exr").is_some());
    assert!(a.read_file("big.bin").is_some());

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
