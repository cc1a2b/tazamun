//! P17: roles are enforced on the wire, by the grantor.
//!
//! The security property: an honest grantor refuses a lease to a peer whose
//! authenticated role may not edit — even if that peer's binary was modified to
//! send a `LockReq` its own local role check would have blocked. We prove it
//! with `RawPeer`, a manual protocol peer that completes the real handshake and
//! then sends exactly the messages a modified binary would: a signed role
//! grant (or none) followed by a raw `LockReq`.

mod common;

use std::time::Duration;

use common::{RawPeer, TestNode};
use tazamun::proto::{DenyReason, Msg};
use tazamun::session::{self, Grant, Ticket};
use tazamun::sync::index::sanitize_rel_path;

/// Connects a raw peer with the session secret, optionally advertises `grant`
/// as its `Identity`, then sends a `LockReq` for `path` and returns the
/// grantor's verdict: `Ok(())` on `LockGrant`, `Err(reason)` on `LockDeny`.
async fn probe_lock(
    ticket: &str,
    grant: Option<session::SignedGrant>,
    path: &str,
) -> Result<(), DenyReason> {
    let mut raw = RawPeer::connect_authed(ticket).await;
    if let Some(g) = grant {
        raw.send_msg(&Msg::Identity { grant: g }).await;
    }
    raw.send_msg(&Msg::LockReq {
        path: sanitize_rel_path(path).unwrap(),
        lamport: 7,
        ttl_ms: 90_000,
    })
    .await;
    // Skip the index/identity the grantor sends us; wait for its lock verdict.
    loop {
        match raw.recv_msg(Duration::from_secs(5)).await {
            Some(Msg::LockGrant { .. }) => return Ok(()),
            Some(Msg::LockDeny { reason, .. }) => return Err(reason),
            Some(_) => continue,
            None => panic!("grantor closed before replying to the lock request"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn grantor_enforces_role_on_the_wire() {
    // A founds a v2 (role-enforcing) session and is an editor/admin.
    let a = TestNode::init().await;
    // A live editor invite carries A's address, the session secret, the admin
    // public key, and — because it is an editor invite — the admin secret, so
    // the test can mint grants exactly as A would.
    let invite = a.invite().await;
    let ticket = Ticket::decode(&invite).unwrap();
    let admin_secret =
        iroh::SecretKey::from_bytes(&ticket.admin_secret.as_ref().expect("editor invite").0);

    let mk = |role: u8, expiry_ms: u64| {
        session::sign_grant(
            &admin_secret,
            Grant {
                role,
                invite_id: [role; 16],
                issued_ms: 1,
                expiry_ms,
            },
        )
    };

    // An editor peer is granted a free path.
    assert!(
        probe_lock(&invite, Some(mk(session::ROLE_EDITOR, 0)), "editor.txt")
            .await
            .is_ok(),
        "an authenticated editor must be granted a free lease"
    );

    // A viewer peer — even one whose binary skips its own local check and sends
    // a raw LockReq — is refused by the grantor.
    assert_eq!(
        probe_lock(&invite, Some(mk(session::ROLE_VIEWER, 0)), "viewer.txt").await,
        Err(DenyReason::RoleForbidden),
        "a viewer's lock must be refused on the wire"
    );

    // An archive peer likewise may not edit.
    assert_eq!(
        probe_lock(&invite, Some(mk(session::ROLE_ARCHIVE, 0)), "archive.txt").await,
        Err(DenyReason::RoleForbidden),
        "an archive's lock must be refused on the wire"
    );

    // A peer that advertises NO grant at all (a modified binary withholding its
    // identity) is fail-closed: unknown role → refused.
    assert_eq!(
        probe_lock(&invite, None, "silent.txt").await,
        Err(DenyReason::RoleForbidden),
        "a peer with no advertised role must be refused (fail-closed)"
    );

    // An EXPIRED editor grant is refused too — a leaked-but-expired invite is a
    // dead invite (expiry_ms=1 is far in the past).
    assert_eq!(
        probe_lock(&invite, Some(mk(session::ROLE_EDITOR, 1)), "expired.txt").await,
        Err(DenyReason::RoleForbidden),
        "an expired grant must not confer edit rights"
    );

    a.handle.shutdown().await;
}
