//! Connection-health, lock-explainability, and doctor integration tests
//! (fully offline, via the in-process TestNode harness).

mod common;

use std::time::Duration;

use common::{TestNode, WAIT, WAIT_MESH, wait_until};
use tazamun::ipc::IpcRequest;

/// The stable `status --json` schema every future consumer relies on.
const REQUIRED_TOP_KEYS: &[&str] = &[
    "schema",
    "id",
    "dir",
    "members",
    "leases",
    "pending_pulls",
    "events",
    "file_count",
    "total_bytes",
];
const REQUIRED_MEMBER_KEYS: &[&str] = &[
    "id",
    "online",
    "synced",
    "conn",
    "rtt_ms",
    "grade",
    "rtt_jitter_ms",
    "path_changes",
    "relay_url",
    "rate_rx_bps",
    "rate_tx_bps",
    "time_to_direct_ms",
];

#[tokio::test(flavor = "multi_thread")]
async fn telemetry_snapshot_after_mesh_is_direct_and_sane() {
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;

    // Wait for A to sample B as connected via a Direct path.
    let direct = wait_until(
        || async {
            let s = a.status().await;
            s["members"]
                .as_array()
                .and_then(|m| m.first())
                .is_some_and(|m| m["conn"] == "Direct")
        },
        WAIT_MESH,
    )
    .await;
    assert!(direct, "A never sampled B as Direct");

    // On a multi-homed host (several NICs) QUIC may migrate the selected path
    // a few times while the connection establishes; those early changes count
    // as flaps and can briefly grade the link Poor, then age out of the sliding
    // window. Assert the *steady state* the design promises — a loopback link
    // settles at Good — rather than the first instant; a genuinely degraded
    // link never settles.
    let settled = wait_until(
        || async { a.status().await["members"][0]["grade"] == "Good" },
        WAIT_MESH + Duration::from_secs(30),
    )
    .await;
    let member = a.status().await["members"][0].clone();
    assert!(settled, "grade never settled to Good: {member}");
    assert_eq!(member["conn"], "Direct");
    let rtt = member["rtt_ms"].as_f64().unwrap();
    assert!((0.0..1000.0).contains(&rtt), "implausible rtt {rtt}");
    assert!(member["online"].as_bool().unwrap());

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_json_schema_is_stable() {
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.status().await["members"][0]["conn"] == "Direct" },
            WAIT_MESH
        )
        .await,
        "mesh"
    );
    let snap = a.status().await;
    for key in REQUIRED_TOP_KEYS {
        assert!(
            snap.get(key).is_some(),
            "missing top-level key {key}: {snap}"
        );
    }
    assert_eq!(snap["schema"], 1);
    let member = &snap["members"][0];
    for key in REQUIRED_MEMBER_KEYS {
        assert!(
            member.get(key).is_some(),
            "missing member key {key}: {member}"
        );
    }
    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_peer_grades_offline_and_lock_diagnoses_reachability() {
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    let b_id = b.id().to_string();
    assert!(
        wait_until(|| async { a.synced_peers().await >= 1 }, WAIT_MESH).await,
        "mesh + index exchange"
    );

    // Kill B (no goodbye): A must eventually grade it Offline.
    b.handle.kill().await;
    let offline = wait_until(
        || async {
            let s = a.status().await;
            s["members"]
                .as_array()
                .and_then(|m| m.iter().find(|x| x["id"] == b_id))
                .is_some_and(|m| m["grade"] == "Offline")
        },
        Duration::from_secs(45),
    )
    .await;
    assert!(offline, "B never graded Offline after kill");

    // With its only voter gone, an acquire fails with a REACHABILITY diagnosis
    // that names the dead peer.
    let resp = a
        .handle
        .request(IpcRequest::Lock {
            path: "x.txt".into(),
        })
        .await;
    assert!(!resp.ok, "lock must fail with the voter gone");
    let data = resp.data.expect("diagnosis data present");
    let diag = &data["diagnosis"];
    assert_eq!(
        diag["precondition"], "REACHABILITY",
        "expected reachability failure: {data}"
    );
    // The failure identifies the dead peer by (short) id somewhere in the
    // message or the peer table.
    let msg = resp.error.map(|e| e.message).unwrap_or_default();
    let short_b: String = b_id.chars().take(10).collect();
    let names_peer = msg.contains(&short_b)
        || diag["peers"]
            .as_array()
            .is_some_and(|ps| ps.iter().any(|p| p["id"] == b_id));
    assert!(
        names_peer,
        "diagnosis did not name the dead peer: {data} / {msg}"
    );

    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_peer_lock_diagnoses_reachability_with_hint() {
    let a = TestNode::init().await;
    let resp = a
        .handle
        .request(IpcRequest::Lock {
            path: "solo.txt".into(),
        })
        .await;
    assert!(!resp.ok);
    let data = resp.data.expect("diagnosis present");
    assert_eq!(data["diagnosis"]["precondition"], "REACHABILITY");
    assert!(
        data["diagnosis"]["hint"]
            .as_str()
            .is_some_and(|h| !h.is_empty()),
        "hint should be actionable"
    );
    a.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_snapshot_reports_disabled_relay_and_direct_link() {
    // The harness runs with relays disabled (RelayChoice::Disabled), so the
    // daemon's doctor snapshot must report the relay policy as disabled and a
    // Direct connectivity link — never an error.
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    assert!(
        wait_until(
            || async { a.status().await["members"][0]["conn"] == "Direct" },
            WAIT_MESH
        )
        .await,
        "mesh"
    );

    let resp = a.handle.request(IpcRequest::Doctor).await;
    assert!(resp.ok, "doctor query failed: {resp:?}");
    let d = resp.data.expect("doctor data");
    assert!(
        d["relay_policy"].as_str().unwrap().starts_with("disabled"),
        "relay policy should be disabled: {}",
        d["relay_policy"]
    );
    assert!(d["home_relay"].is_null(), "no relay when disabled");
    let peers = d["peers"].as_array().expect("peers array");
    assert_eq!(peers.len(), 1);
    assert_eq!(
        peers[0]["conn"], "Direct",
        "hole-punched direct link expected"
    );
    assert!(!d["bound_sockets"].as_array().unwrap().is_empty());

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_event_appears_in_status_ring() {
    // A connect event lands in the ring as soon as the mesh forms.
    let a = TestNode::init().await;
    let b = TestNode::join(&a.invite().await).await;
    let saw_event = wait_until(
        || async {
            a.status().await["events"].as_array().is_some_and(|e| {
                e.iter()
                    .any(|x| x["text"].as_str().is_some_and(|t| t.contains("connected")))
            })
        },
        WAIT,
    )
    .await;
    assert!(saw_event, "no connect event in the status ring");
    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
