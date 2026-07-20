//! P17 named peers: a local label for a peer id, resolved from a short prefix,
//! surfaced in `status` and usable to clear.

mod common;

use common::{TestNode, WAIT, wait_until};
use tazamun::ipc::IpcRequest;

#[tokio::test(flavor = "multi_thread")]
async fn peer_can_be_named_by_prefix_and_shows_in_status() {
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.online_peers().await >= 1 && b.online_peers().await >= 1 },
            WAIT
        )
        .await,
        "nodes did not connect"
    );

    let b_id = b.id().to_string();
    let prefix = &b_id[..10];

    // Name B by a short prefix.
    let resp = a
        .handle
        .request(IpcRequest::PeerName {
            id: prefix.to_string(),
            name: Some("render-box".into()),
        })
        .await;
    assert!(resp.ok, "naming failed: {resp:?}");
    assert_eq!(
        resp.data.as_ref().unwrap()["id"].as_str().unwrap(),
        b_id,
        "the prefix resolved to B's full id"
    );

    // Status shows the name, both on B's member row and in the names map.
    let status = a.status().await;
    let member = status["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == Some(&b_id))
        .expect("B in members");
    assert_eq!(member["name"].as_str(), Some("render-box"));
    assert_eq!(status["names"][&b_id].as_str(), Some("render-box"));

    // An ambiguous or unknown id is refused (a single hex char matches many/none).
    let bad = a
        .handle
        .request(IpcRequest::PeerName {
            id: "zzzz".into(),
            name: Some("x".into()),
        })
        .await;
    assert!(!bad.ok, "an unknown peer id must be refused");

    // Clearing the name removes it.
    let cleared = a
        .handle
        .request(IpcRequest::PeerName {
            id: prefix.to_string(),
            name: None,
        })
        .await;
    assert!(cleared.ok && cleared.data.as_ref().unwrap()["cleared"] == true);
    let status = a.status().await;
    assert!(status["names"].get(&b_id).is_none(), "name was cleared");

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
