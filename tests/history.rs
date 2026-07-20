//! P14 history v2 across real nodes: editing a file builds history, and tag /
//! pin / diff operate on it end-to-end through the daemon.

mod common;

use common::{TestNode, WAIT, wait_until};
use tazamun::ipc::IpcRequest;

#[tokio::test(flavor = "multi_thread")]
async fn edits_build_history_then_tag_pin_and_diff() {
    // Two nodes so the strict lease preconditions hold and edits publish.
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("doc.txt"), vec![1u8; 40_000]).unwrap();
    let a = TestNode::start(dir).await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(|| async { b.read_file("doc.txt").is_some() }, WAIT).await,
        "initial sync"
    );

    // Two edits → two replaced versions in A's history.
    for v in [2u8, 3u8] {
        a.lock_ok("doc.txt").await;
        a.write_file("doc.txt", &vec![v; 40_000]);
        a.unlock_ok("doc.txt").await;
        assert!(
            wait_until(
                || async { b.read_file("doc.txt").as_deref() == Some(&vec![v; 40_000][..]) },
                WAIT
            )
            .await,
            "edit {v} synced"
        );
    }

    async fn versions(node: &TestNode) -> tazamun::ipc::IpcResponse {
        node.handle
            .request(IpcRequest::Versions {
                path: "doc.txt".into(),
            })
            .await
    }

    // A has ≥2 kept versions.
    let vr = versions(&a).await;
    let list = vr.data.as_ref().unwrap()["versions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(list.len() >= 2, "history built: {}", list.len());

    // Tag version 1, pin it, and confirm both show in the listing.
    let tagged = a
        .handle
        .request(IpcRequest::Tag {
            path: "doc.txt".into(),
            n: 1,
            name: Some("client-approved".into()),
        })
        .await;
    assert!(tagged.ok, "{tagged:?}");
    let pinned = a
        .handle
        .request(IpcRequest::Pin {
            path: "doc.txt".into(),
            n: 1,
            pinned: true,
        })
        .await;
    assert!(pinned.ok, "{pinned:?}");

    let vr = versions(&a).await;
    let v1 = vr.data.as_ref().unwrap()["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["n"] == 1)
        .cloned()
        .expect("version 1");
    assert_eq!(v1["tag"], "client-approved");
    assert_eq!(v1["pinned"], true);
    // History byte accounting is present and non-zero.
    assert!(vr.data.as_ref().unwrap()["history_bytes"].as_u64().unwrap() > 0);

    // Diff current vs version 1: same-size full-rewrite → ~100% changed, all
    // chunks added, none identical (every byte differs between the versions).
    let diff = a
        .handle
        .request(IpcRequest::Diff {
            path: "doc.txt".into(),
            n: 1,
        })
        .await;
    assert!(diff.ok, "{diff:?}");
    let d = diff.data.unwrap();
    assert_eq!(d["version_tag"], "client-approved");
    assert_eq!(d["identical"], 0, "a full rewrite shares no chunks");
    assert!(d["added"].as_u64().unwrap() >= 1);
    assert!(d["changed_pct"].as_f64().unwrap() > 90.0, "{d}");
    assert!(d["transfer_bytes"].as_u64().unwrap() > 0);

    // A viewer/offline node can still tag/pin/diff its own history — these are
    // local metadata ops, no lease or peer required. Unpin then restore-depth
    // pruning is exercised by the unit tests.
    let unpin = a
        .handle
        .request(IpcRequest::Pin {
            path: "doc.txt".into(),
            n: 1,
            pinned: false,
        })
        .await;
    assert!(unpin.ok);

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
