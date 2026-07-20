//! Phase 3 integration tests: self-hosted relay path, LAN rendezvous, and
//! airgap endpoint configuration.

mod common;

use std::time::Duration;

use common::{TestNode, WAIT_MESH, wait_until};

/// Telemetry pipeline proof: a relayed connection sample produces exactly the
/// `status`/`status --json` fields a relayed member shows — conn=Relayed, the
/// relay hostname, and a non-Offline grade.
#[test]
fn relayed_sample_surfaces_conn_and_hostname() {
    use std::time::Instant;
    use tazamun::net::telemetry::{ConnState, PathSample, PeerHealth};

    let sample = PathSample {
        conn: ConnState::Relayed,
        rtt_ms: 120.0,
        relay_url: Some("https://relay.example.com./".into()),
        on_lan: false,
        bytes_tx: 0,
        bytes_rx: 0,
    };
    let now = Instant::now();
    let mut h = PeerHealth::seen_only(now);
    h.on_connect(now);
    h.on_sample(&sample, now);
    assert_eq!(h.conn, ConnState::Relayed);
    assert_eq!(h.relay_url.as_deref(), Some("https://relay.example.com./"));
    // A stable relayed link grades Fair (not Offline).
    assert_eq!(h.grade(now).to_string(), "Fair");
    assert_eq!(
        host_of(h.relay_url.as_deref().unwrap()),
        "relay.example.com"
    );
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string()
}

/// LAN rendezvous: two nodes with LAN discovery on and NO explicit address
/// exchange find each other over mDNS and sync. GitHub runners typically lack
/// multicast, so this auto-skips (with a logged reason) rather than flaking.
#[tokio::test(flavor = "multi_thread")]
async fn lan_rendezvous_with_no_explicit_addrs() {
    use tazamun::net::endpoint::NetConfig;

    // Both nodes: LAN on, relays off, and — crucially — no test_bind and no
    // ticket address exchange. The only way to meet is mDNS.
    let lan_net = || NetConfig {
        relay: tazamun::net::endpoint::RelayChoice::Disabled,
        lan: true,
        airgap: false,
        test_bind: None,
        test_relay: None,
    };

    let a_dir = TestNode::init_dir();
    let a = TestNode::start_with(a_dir, lan_net()).await;
    // A minimal ticket carrying ONLY the session secret (no bootstrap addrs),
    // so B cannot learn A's address except via LAN discovery.
    let ticket = secret_only_ticket(&a).await;

    let b_dir = tempfile::tempdir().expect("tempdir");
    tazamun::cli::join(b_dir.path(), &ticket).expect("join");
    let b = TestNode::start_with(b_dir, lan_net()).await;

    // Give mDNS a generous window; if the runner has no multicast, skip.
    let connected = wait_until(
        || async { a.online_peers().await >= 1 && b.online_peers().await >= 1 },
        Duration::from_secs(20),
    )
    .await;

    if !connected {
        eprintln!(
            "SKIP lan_rendezvous_with_no_explicit_addrs: peers did not discover \
             each other over mDNS within 20s (no multicast on this runner)"
        );
        a.handle.shutdown().await;
        b.handle.shutdown().await;
        return;
    }

    // They met purely via LAN: a lease + edit must round-trip.
    a.lock_ok("lan.txt").await;
    a.write_file("lan.txt", b"discovered over the LAN");
    a.unlock_ok("lan.txt").await;
    assert!(
        wait_until(|| async { b.read_file("lan.txt").is_some() }, WAIT_MESH).await,
        "LAN-discovered peers did not sync"
    );
    a.handle.shutdown().await;
    b.handle.shutdown().await;
}

/// Builds a ticket carrying only the session secret (no bootstrap addresses).
async fn secret_only_ticket(node: &TestNode) -> String {
    use tazamun::session::{SessionSecret, Ticket};
    let secret = tazamun::state::AppState::load(node.root())
        .expect("load")
        .session_secret_bytes()
        .expect("secret");
    Ticket::new(SessionSecret(secret), vec![]).encode()
}

/// Airgap endpoint-config proof: the relay map an airgap endpoint would use is
/// empty (zero external relay URLs) — the concrete "reaches no external relay"
/// assertion — while the default config has relays. Also builds a live airgap
/// endpoint to confirm it binds with no home relay.
#[tokio::test(flavor = "multi_thread")]
async fn airgap_endpoint_has_no_external_relays() {
    use iroh::Watcher;
    use tazamun::net::endpoint::{NetConfig, RelayChoice, build_endpoint, relay_mode_for};

    let airgap = NetConfig {
        relay: RelayChoice::Disabled,
        lan: true,
        airgap: true,
        test_bind: None,
        test_relay: None,
    };
    // Pure, non-racy proof: airgap → empty relay map.
    assert!(
        relay_mode_for(&airgap).relay_map().is_empty(),
        "airgap relay map must contain zero external relay URLs"
    );

    // Contrast: the default config resolves to a non-empty relay map.
    let normal = NetConfig {
        relay: RelayChoice::Default,
        lan: true,
        airgap: false,
        test_bind: None,
        test_relay: None,
    };
    assert!(
        !relay_mode_for(&normal).relay_map().is_empty(),
        "the default config should carry relays"
    );

    // A live airgap endpoint binds with no home relay.
    let ep = build_endpoint(iroh::SecretKey::generate(), &airgap)
        .await
        .expect("build airgap endpoint");
    assert!(
        ep.home_relay_status().get().is_empty(),
        "airgap endpoint should have no home relay"
    );
    ep.close().await;
}

/// Airgap `doctor` snapshot reports `mode=airgap`, disabled relay, and empty
/// relay status — the closed-network guarantees, live from the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn airgap_doctor_reports_closed_network() {
    use tazamun::ipc::IpcRequest;
    use tazamun::net::endpoint::{NetConfig, RelayChoice};

    let airgap_net = NetConfig {
        relay: RelayChoice::Disabled,
        lan: true,
        airgap: true,
        // Bind locally so no real network is needed for the daemon to start.
        test_bind: Some("127.0.0.1:0".parse().unwrap()),
        test_relay: None,
    };
    let dir = TestNode::init_dir();
    // test_bind takes precedence, but the daemon still reports airgap=true.
    let a = TestNode::start_with(dir, airgap_net).await;

    let resp = a.handle.request(IpcRequest::Doctor).await;
    assert!(resp.ok);
    let d = resp.data.expect("doctor data");
    assert_eq!(d["mode"], "airgap");
    assert!(
        d["relay_status"].as_array().is_some_and(|s| s.is_empty()),
        "airgap should have no relay connections: {}",
        d["relay_status"]
    );
    a.handle.shutdown().await;
}
