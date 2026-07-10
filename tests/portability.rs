//! Cross-platform path portability: records whose paths the sanitizer accepts
//! but Windows cannot represent are injected via the index path (exactly how a
//! Linux peer would advertise them). A Windows node must hold them "unapplied"
//! and stay healthy; a Unix node applies them with a warning.

mod common;

use std::time::Duration;

use common::{RawPeer, TestNode, WAIT, wait_until};
use tazamun::proto::{FileRecord, ManifestRef, Msg};
use tazamun::sync::index::sanitize_rel_path;
use tazamun::sync::vclock::VClock;

/// An empty-file record (zero chunks): applying it needs no blob fetch, so a
/// RawPeer (control-plane only) can inject it end to end.
fn empty_record(peer: &str) -> FileRecord {
    FileRecord {
        size: 0,
        manifest: ManifestRef::Inline(vec![]),
        vv: VClock::from([(peer.to_string(), 1u64)]),
        deleted: false,
        updated_at_ms: tazamun::now_ms(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn non_portable_records_are_held_unapplied_on_windows() {
    // Genesis README.md so the case-fold collision has something to hit.
    let dir = TestNode::init_dir();
    std::fs::write(dir.path().join("README.md"), b"docs").unwrap();
    let a = TestNode::start(dir).await;
    assert!(
        wait_until(|| async { a.file_count().await == 1 }, WAIT).await,
        "genesis import"
    );

    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    let me = raw.endpoint.id().to_string();

    // The sanitizer permits all of these; Windows cannot represent them.
    for path in ["aux.txt", "da:ta.bin", "readme.md"] {
        raw.send_msg(&Msg::FileMeta {
            path: sanitize_rel_path(path).expect("sanitizer permits it"),
            record: empty_record(&me),
            lamport: 2,
        })
        .await;
    }
    // A portable control file proves the node keeps syncing afterwards.
    raw.send_msg(&Msg::FileMeta {
        path: sanitize_rel_path("healthy.txt").expect("sanitizer permits it"),
        record: empty_record(&me),
        lamport: 3,
    })
    .await;

    if cfg!(windows) {
        // All three non-portable records end up unapplied, none materialized.
        let marked = wait_until(
            || async {
                a.status().await["unapplied"]
                    .as_array()
                    .map(|u| u.len() >= 3)
                    .unwrap_or(false)
            },
            WAIT,
        )
        .await;
        let st = a.status().await;
        assert!(marked, "expected 3 unapplied entries: {}", st["unapplied"]);
        for p in ["aux.txt", "da:ta.bin", "readme.md"] {
            assert!(
                st["unapplied"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|u| u["path"] == p),
                "{p} missing from unapplied: {}",
                st["unapplied"]
            );
            assert!(
                a.read_file(p).is_none() || p == "readme.md",
                "{p} must not be materialized"
            );
        }
        // The daemon reports the count through doctor.
        let doc = a.handle.request(tazamun::ipc::IpcRequest::Doctor).await;
        assert!(doc.ok);
        assert!(
            doc.data.unwrap()["unapplied_count"].as_u64().unwrap_or(0) >= 3,
            "doctor must count unapplied paths"
        );
    } else {
        // Unix: warn-only — the records apply (empty files land on disk).
        assert!(
            wait_until(|| async { a.read_file("aux.txt").is_some() }, WAIT).await,
            "unix applies aux.txt (warn-only)"
        );
        assert!(
            a.status().await["unapplied"]
                .as_array()
                .is_some_and(|u| u.is_empty()),
            "unix never marks unapplied"
        );
    }

    // Health: the portable record synced regardless of platform.
    assert!(
        wait_until(|| async { a.read_file("healthy.txt").is_some() }, WAIT).await,
        "the node must keep syncing portable paths"
    );

    raw.close();
    a.handle.shutdown().await;
}

/// A tombstone for an unapplied path clears the marker (Windows) and never
/// wedges the loop anywhere.
#[tokio::test(flavor = "multi_thread")]
async fn tombstone_clears_an_unapplied_marker() {
    let a = TestNode::init().await;
    let mut raw = RawPeer::connect_authed(&a.invite().await).await;
    let me = raw.endpoint.id().to_string();

    raw.send_msg(&Msg::FileMeta {
        path: sanitize_rel_path("da:ta.bin").expect("sanitizer permits it"),
        record: empty_record(&me),
        lamport: 2,
    })
    .await;
    if cfg!(windows) {
        assert!(
            wait_until(
                || async {
                    a.status().await["unapplied"]
                        .as_array()
                        .map(|u| !u.is_empty())
                        .unwrap_or(false)
                },
                WAIT,
            )
            .await,
            "marker appears first"
        );
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let mut vv = VClock::from([(me.clone(), 2u64)]);
    let _ = &mut vv;
    raw.send_msg(&Msg::FileMeta {
        path: sanitize_rel_path("da:ta.bin").expect("sanitizer permits it"),
        record: FileRecord::tombstone(vv, tazamun::now_ms()),
        lamport: 3,
    })
    .await;

    assert!(
        wait_until(
            || async {
                a.status().await["unapplied"]
                    .as_array()
                    .map(|u| u.is_empty())
                    .unwrap_or(false)
            },
            WAIT,
        )
        .await,
        "tombstone must clear the unapplied marker"
    );

    raw.close();
    a.handle.shutdown().await;
}
