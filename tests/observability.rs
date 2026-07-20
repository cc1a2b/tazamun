//! P19: the daemon writes an audit trail that `tazamun log` reads back.

mod common;

use common::{TestNode, WAIT, wait_until};
use tazamun::audit::{self, Filter};

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_events_land_in_the_audit_log() {
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

    // A lock → edit → unlock produces lock, publish, and unlock events on A;
    // B records the remote apply (pull-applied) and both record the peer join.
    a.lock_ok("notes.md").await;
    a.write_file("notes.md", b"hello audit");
    a.unlock_ok("notes.md").await;

    assert!(
        wait_until(
            || async {
                let kinds: Vec<String> = audit::read(a.root(), &Filter::default())
                    .into_iter()
                    .map(|e| e.kind)
                    .collect();
                kinds.iter().any(|k| k == "lock")
                    && kinds.iter().any(|k| k == "publish")
                    && kinds.iter().any(|k| k == "unlock")
                    && kinds.iter().any(|k| k == "peer-connected")
            },
            WAIT
        )
        .await,
        "A's audit log is missing lifecycle events: {:?}",
        audit::read(a.root(), &Filter::default())
            .into_iter()
            .map(|e| e.kind)
            .collect::<Vec<_>>()
    );

    // Path filter isolates one file's trail.
    let by_path = audit::read(
        a.root(),
        &Filter {
            path: Some("notes.md".into()),
            ..Default::default()
        },
    );
    assert!(!by_path.is_empty());
    assert!(
        by_path
            .iter()
            .all(|e| e.path.as_deref() == Some("notes.md"))
    );

    // Kind filter.
    let locks = audit::read(
        a.root(),
        &Filter {
            kinds: vec!["lock".into()],
            ..Default::default()
        },
    );
    assert!(locks.iter().all(|e| e.kind == "lock") && !locks.is_empty());

    // B saw the remote change land.
    assert!(
        wait_until(
            || async {
                audit::read(b.root(), &Filter::default())
                    .iter()
                    .any(|e| e.kind == "pull-applied")
            },
            WAIT
        )
        .await,
        "B's audit log missing the pull-applied event"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
