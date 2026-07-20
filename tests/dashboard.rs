//! Web dashboard integration: the HTTP layer over a real daemon on an
//! ephemeral loopback port. Asserts the API mirrors IPC, mutations are
//! token-guarded and reach the daemon, DNS-rebinding is refused, and a
//! restore driven through the API actually lands the bytes.

mod common;

use std::time::Duration;

use common::{TestNode, WAIT, set_dashboard_port, wait_until};
use tazamun::ipc::IpcRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Starts a fresh single-node session with the dashboard on an ephemeral port.
async fn node_with_dashboard() -> TestNode {
    let dir = TestNode::init_dir();
    set_dashboard_port(dir.path(), 0);
    TestNode::start(dir).await
}

/// Reads the daemon's dashboard port + token, waiting for the ephemeral bind.
async fn dash_info(node: &TestNode) -> (u16, String) {
    // The dashboard is started on demand now — ask the daemon to bind it, then
    // wait for the ephemeral port to be published via DashboardInfo.
    let _ = node.handle.request(IpcRequest::DashboardStart).await;
    for _ in 0..80 {
        let r = node.handle.request(IpcRequest::DashboardInfo).await;
        if let Some(d) = r.data.as_ref() {
            let port = d["port"].as_u64().unwrap_or(0) as u16;
            let token = d["token"].as_str().unwrap_or_default().to_string();
            if port != 0 && !token.is_empty() {
                return (port, token);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("dashboard never bound an ephemeral port");
}

/// A minimal loopback HTTP/1.1 client: returns `(status_code, json_body)`.
async fn http(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
    host: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect dashboard");
    let host = host
        .map(String::from)
        .unwrap_or_else(|| format!("127.0.0.1:{port}"));
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(t) = token {
        req.push_str(&format!("X-Tazamun-Token: {t}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    s.write_all(req.as_bytes()).await.expect("write request");
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.expect("read response");
    let text = String::from_utf8_lossy(&buf);
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text
        .find("\r\n\r\n")
        .map(|i| text[i + 4..].trim())
        .and_then(|b| serde_json::from_str(b).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn api_state_mirrors_ipc_status() {
    let node = node_with_dashboard().await;
    let (port, _token) = dash_info(&node).await;

    let (status, body) = http(port, "GET", "/api/state", None, None, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["api"], 1);
    assert_eq!(body["ok"], true);

    // The snapshot mirrors the IPC status contract and adds the api:1 extras.
    let ipc = node.status().await;
    assert_eq!(body["data"]["id"], ipc["id"]);
    assert_eq!(body["data"]["file_count"], ipc["file_count"]);
    assert!(body["data"]["config"].is_object(), "config summary missing");
    assert!(body["data"]["conflicts"].is_array(), "conflicts missing");
    assert!(body["data"]["versions"].is_object(), "versions missing");
    assert!(body["data"]["mode"].is_string(), "mode missing");

    node.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mutations_require_the_token() {
    let node = node_with_dashboard().await;
    let (port, token) = dash_info(&node).await;
    let body = r#"{"path":"x.txt"}"#;

    // No token → 401.
    let (status, resp) = http(port, "POST", "/api/lock", None, Some(body), None).await;
    assert_eq!(status, 401);
    assert_eq!(resp["error"]["code"], "unauthorized");

    // Wrong token → 401.
    let (status, _) = http(
        port,
        "POST",
        "/api/lock",
        Some("deadbeef00"),
        Some(body),
        None,
    )
    .await;
    assert_eq!(status, 401);

    // Correct token → reaches the daemon (which refuses with strict-offline,
    // since this single node has no peer — the point is it is NOT a 401).
    let (status, resp) = http(port, "POST", "/api/lock", Some(&token), Some(body), None).await;
    assert_ne!(status, 401, "valid token was rejected");
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "strict_offline");

    node.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn non_loopback_host_is_refused() {
    let node = node_with_dashboard().await;
    let (port, _token) = dash_info(&node).await;

    // A DNS-rebinding page would send its own Host — refuse it, reads included.
    let (status, body) = http(
        port,
        "GET",
        "/api/state",
        None,
        None,
        Some("evil.example.com"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["error"]["code"], "bad_host");

    node.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn config_via_api_applies_live() {
    let node = node_with_dashboard().await;
    let (port, token) = dash_info(&node).await;

    // autolock defaults off; flip it on through the API.
    let (status, resp) = http(
        port,
        "POST",
        "/api/config",
        Some(&token),
        Some(r#"{"key":"autolock","value":"on"}"#),
        None,
    )
    .await;
    assert_eq!(status, 200, "config set failed: {resp}");
    assert_eq!(resp["ok"], true);

    // The change is visible in the next snapshot.
    let (_, state) = http(port, "GET", "/api/state", None, None, None).await;
    assert_eq!(state["data"]["config"]["autolock"], true);

    // A network key is refused live (needs a restart).
    let (_, resp) = http(
        port,
        "POST",
        "/api/config",
        Some(&token),
        Some(r#"{"key":"airgap","value":"on"}"#),
        None,
    )
    .await;
    assert_eq!(resp["ok"], false);

    node.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_and_restore_through_the_api() {
    // Two real nodes so REACHABILITY/FRESHNESS hold and B grants the lease.
    let adir = TestNode::init_dir();
    std::fs::write(adir.path().join("doc.txt"), b"one").unwrap();
    set_dashboard_port(adir.path(), 0);
    let a = TestNode::start(adir).await;
    assert!(
        wait_until(|| async { a.file_count().await == 1 }, WAIT).await,
        "genesis import did not finish"
    );

    let bdir = tempfile::tempdir().unwrap();
    tazamun::cli::join(bdir.path(), &a.invite().await).expect("join");
    set_dashboard_port(bdir.path(), 0); // avoid the fixed-port clash with A
    let b = TestNode::start(bdir).await;

    assert!(
        wait_until(
            || async { a.synced_peers().await >= 1 && b.synced_peers().await >= 1 },
            WAIT
        )
        .await,
        "nodes did not sync indexes"
    );
    assert!(
        wait_until(
            || async { b.read_file("doc.txt").as_deref() == Some(b"one") },
            WAIT
        )
        .await,
        "B did not receive doc.txt"
    );

    // B edits doc.txt → "two" under a lease; A gains a history entry for "one".
    b.lock_ok("doc.txt").await;
    b.write_file("doc.txt", b"two");
    b.unlock_ok("doc.txt").await;
    assert!(
        wait_until(
            || async { a.read_file("doc.txt").as_deref() == Some(b"two") },
            WAIT
        )
        .await,
        "A did not receive the edit"
    );

    let (port, token) = dash_info(&a).await;

    // Lock doc.txt through the API (B grants), then restore version #n of "one".
    let (status, resp) = http(
        port,
        "POST",
        "/api/lock",
        Some(&token),
        Some(r#"{"path":"doc.txt"}"#),
        None,
    )
    .await;
    assert_eq!(resp["ok"], true, "lock via API failed: {resp}");
    assert_eq!(status, 200);
    // The IPC status confirms the API mutation created a real self-held lease.
    let held = a.status().await["leases"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .any(|l| l["path"] == "doc.txt" && l["mine"] == true)
        })
        .unwrap_or(false);
    assert!(held, "API lock did not create a self-held lease");

    // Find the kept version of "one" from the snapshot and restore it.
    let (_, state) = http(port, "GET", "/api/state", None, None, None).await;
    let n = state["data"]["versions"]["doc.txt"][0]["n"]
        .as_u64()
        .expect("a kept version of doc.txt") as usize;
    let body = format!(r#"{{"path":"doc.txt","n":{n}}}"#);
    let (status, resp) = http(
        port,
        "POST",
        "/api/restore",
        Some(&token),
        Some(&body),
        None,
    )
    .await;
    assert_eq!(status, 200, "restore via API failed: {resp}");
    assert_eq!(resp["ok"], true);

    // The bytes actually landed on disk.
    assert!(
        wait_until(
            || async { a.read_file("doc.txt").as_deref() == Some(b"one") },
            WAIT
        )
        .await,
        "restore via API did not land the bytes"
    );

    a.handle.shutdown().await;
    b.handle.shutdown().await;
}
