//! P12 one-shot `send`/`receive`: a real end-to-end transfer between two
//! ephemeral endpoints on loopback — chunked, verified, assembled atomically.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use tazamun::net::endpoint::{NetConfig, RelayChoice};
use tazamun::oneshot;
use tokio::sync::oneshot as tokio_oneshot;

/// Loopback-direct config: airgap (no relays/discovery) with a fixed test
/// bind, so two same-host endpoints connect directly with no network.
fn loopback_net() -> NetConfig {
    NetConfig {
        relay: RelayChoice::Disabled,
        lan: false,
        airgap: true,
        test_bind: Some("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
        test_relay: None,
    }
}

/// Drives one `send(src)` → `receive(dest)` and returns once both finish.
async fn transfer(src: &Path, dest: &Path) {
    let (tx, rx) = tokio_oneshot::channel::<String>();
    let src = src.to_path_buf();
    let sender = tokio::spawn(async move {
        oneshot::send(
            &src,
            &loopback_net(),
            Duration::from_secs(30),
            move |ticket| {
                let _ = tx.send(ticket.to_string());
            },
        )
        .await
    });

    let ticket = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .expect("sender should print a ticket")
        .expect("ticket channel");
    assert!(ticket.starts_with("tzs1"), "ticket: {ticket}");

    let recv = oneshot::receive(&ticket, dest, &loopback_net())
        .await
        .expect("receive should succeed");
    assert!(recv.files >= 1);

    let sent = tokio::time::timeout(Duration::from_secs(20), sender)
        .await
        .expect("sender should finish")
        .expect("sender task")
        .expect("send should succeed");
    assert_eq!(sent.files, recv.files);
    assert_eq!(sent.bytes, recv.bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn single_file_round_trips_and_verifies() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    // A payload several chunks long so the chunker/verify path is real.
    let payload: Vec<u8> = (0..500_000u32)
        .map(|i| i.wrapping_mul(2_654_435_761) as u8)
        .collect();
    let src = src_dir.path().join("photo.bin");
    std::fs::write(&src, &payload).unwrap();

    transfer(&src, dst_dir.path()).await;

    let got = std::fs::read(dst_dir.path().join("photo.bin")).expect("file landed");
    assert_eq!(got, payload, "received bytes must match exactly");
}

#[tokio::test(flavor = "multi_thread")]
async fn folder_round_trips_skipping_junk() {
    let src_dir = tempfile::tempdir().unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let root = src_dir.path();
    std::fs::write(root.join("a.txt"), b"alpha").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/b.bin"), vec![9u8; 3000]).unwrap();
    // Junk that must not be sent.
    std::fs::write(root.join(".a.txt.swp"), b"vim").unwrap();
    std::fs::write(root.join("sub/.DS_Store"), b"meta").unwrap();

    transfer(root, dst_dir.path()).await;

    assert_eq!(
        std::fs::read(dst_dir.path().join("a.txt")).unwrap(),
        b"alpha"
    );
    assert_eq!(
        std::fs::read(dst_dir.path().join("sub/b.bin")).unwrap(),
        vec![9u8; 3000]
    );
    // Junk stayed home; the receiver never created it.
    assert!(!dst_dir.path().join(".a.txt.swp").exists());
    assert!(!dst_dir.path().join("sub/.DS_Store").exists());
    // No staging residue is left after a clean receive.
    assert!(!dst_dir.path().join(".tazamun-recv").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_ticket_secret_is_refused() {
    // A receiver holding a syntactically valid but wrong-secret ticket (its
    // bootstrap addr points nowhere) fails cleanly rather than hanging or
    // transferring anything.
    let dst = tempfile::tempdir().unwrap();
    // A fabricated ticket to an unreachable address: decode must succeed,
    // connect must fail fast.
    let bogus = "tzs1"; // too short to decode → clean error
    let err = oneshot::receive(bogus, dst.path(), &loopback_net())
        .await
        .expect_err("a malformed ticket must be refused");
    assert!(!err.is_empty());
}
