//! Authenticated control plane: mutual proof-of-secret handshake and peer
//! handles.
//!
//! Invariant: knowing the gossip topic (or any public listing) must never
//! suffice to join a session — every control connection proves knowledge of
//! the session secret in both directions, inside a hard deadline, before a
//! single application message is processed. Failures close the connection
//! with one generic reason and no oracle detail.

use hmac::{Hmac, KeyInit, Mac};
use iroh::EndpointId;
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use sha2::Sha256;
use tokio::sync::mpsc;
use tracing::{debug, instrument, warn};

use crate::consts::HANDSHAKE_DEADLINE;
use crate::proto::{self, Msg, ProtoError};
use crate::session::SessionKeys;

const LABEL_RESP: &[u8] = b"resp";
const LABEL_INIT: &[u8] = b"init";

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("handshake failed")]
    Failed,
    #[error("handshake timed out")]
    Timeout,
    #[error("handshake io: {0}")]
    Proto(#[from] ProtoError),
    #[error("connection: {0}")]
    Connection(String),
}

/// HMAC-SHA256(auth_key, label ‖ id_min ‖ id_max ‖ nonce_a ‖ nonce_b).
///
/// `id_min`/`id_max` are the two 32-byte endpoint public keys in byte order,
/// which makes the proof symmetric in the two identities while the label
/// separates the two directions.
pub fn proof(
    auth_key: &[u8; 32],
    label: &[u8],
    a: &EndpointId,
    b: &EndpointId,
    nonce_a: &[u8; 16],
    nonce_b: &[u8; 16],
) -> [u8; 32] {
    let (lo, hi) = if a.as_bytes() <= b.as_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    // HMAC accepts keys of any length; 32 bytes can never fail.
    let mut mac = Hmac::<Sha256>::new_from_slice(auth_key)
        .unwrap_or_else(|_| unreachable!("HMAC accepts 32-byte keys"));
    mac.update(label);
    mac.update(lo.as_bytes());
    mac.update(hi.as_bytes());
    mac.update(nonce_a);
    mac.update(nonce_b);
    mac.finalize().into_bytes().into()
}

fn verify(expected: &[u8; 32], got: &[u8; 32]) -> bool {
    // Constant-time comparison via subtle.
    use subtle::ConstantTimeEq;
    expected.ct_eq(got).into()
}

/// Runs the initiator side of the handshake on a fresh bidirectional stream
/// of `conn` and returns the authenticated stream pair.
#[instrument(skip_all, fields(peer = %conn.remote_id().fmt_short()))]
pub async fn handshake_initiator(
    conn: &Connection,
    keys: &SessionKeys,
    me: EndpointId,
) -> Result<(SendStream, RecvStream), HandshakeError> {
    let remote = conn.remote_id();
    let fut = async {
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| HandshakeError::Connection(e.to_string()))?;
        let nonce_a: [u8; 16] = rand::random();
        proto::write_msg(&mut send, &Msg::Hello { nonce: nonce_a }).await?;
        let Msg::HelloAck {
            nonce: nonce_b,
            proof: their_proof,
        } = proto::read_msg(&mut recv).await?
        else {
            return Err(HandshakeError::Failed);
        };
        let expected = proof(&keys.auth, LABEL_RESP, &me, &remote, &nonce_a, &nonce_b);
        if !verify(&expected, &their_proof) {
            return Err(HandshakeError::Failed);
        }
        let mine = proof(&keys.auth, LABEL_INIT, &me, &remote, &nonce_a, &nonce_b);
        proto::write_msg(&mut send, &Msg::Proof { proof: mine }).await?;
        Ok((send, recv))
    };
    match tokio::time::timeout(HANDSHAKE_DEADLINE, fut).await {
        Ok(Ok(streams)) => {
            debug!("handshake ok (initiator)");
            Ok(streams)
        }
        Ok(Err(e)) => {
            warn!("handshake failed");
            conn.close(VarInt::from_u32(1), b"handshake failed");
            Err(e)
        }
        Err(_) => {
            warn!("handshake failed");
            conn.close(VarInt::from_u32(1), b"handshake failed");
            Err(HandshakeError::Timeout)
        }
    }
}

/// Runs the acceptor side of the handshake.
#[instrument(skip_all, fields(peer = %conn.remote_id().fmt_short()))]
pub async fn handshake_acceptor(
    conn: &Connection,
    keys: &SessionKeys,
    me: EndpointId,
) -> Result<(SendStream, RecvStream), HandshakeError> {
    let remote = conn.remote_id();
    let fut = async {
        let (mut send, mut recv) = conn
            .accept_bi()
            .await
            .map_err(|e| HandshakeError::Connection(e.to_string()))?;
        let Msg::Hello { nonce: nonce_a } = proto::read_msg(&mut recv).await? else {
            return Err(HandshakeError::Failed);
        };
        let nonce_b: [u8; 16] = rand::random();
        let mine = proof(&keys.auth, LABEL_RESP, &me, &remote, &nonce_a, &nonce_b);
        proto::write_msg(
            &mut send,
            &Msg::HelloAck {
                nonce: nonce_b,
                proof: mine,
            },
        )
        .await?;
        let Msg::Proof { proof: their_proof } = proto::read_msg(&mut recv).await? else {
            return Err(HandshakeError::Failed);
        };
        let expected = proof(&keys.auth, LABEL_INIT, &me, &remote, &nonce_a, &nonce_b);
        if !verify(&expected, &their_proof) {
            return Err(HandshakeError::Failed);
        }
        Ok((send, recv))
    };
    match tokio::time::timeout(HANDSHAKE_DEADLINE, fut).await {
        Ok(Ok(streams)) => {
            debug!("handshake ok (acceptor)");
            Ok(streams)
        }
        Ok(Err(e)) => {
            warn!("handshake failed");
            conn.close(VarInt::from_u32(1), b"handshake failed");
            Err(e)
        }
        Err(_) => {
            warn!("handshake failed");
            conn.close(VarInt::from_u32(1), b"handshake failed");
            Err(HandshakeError::Timeout)
        }
    }
}

/// Events a peer's reader/writer tasks feed back into the daemon.
#[derive(Debug)]
pub enum PeerEvent {
    Msg { id: EndpointId, msg: Msg },
    Gone { id: EndpointId, conn_id: usize },
}

/// A live authenticated peer: cloneable sender plus the underlying connection.
#[derive(Debug)]
pub struct PeerHandle {
    pub id: EndpointId,
    pub conn: Connection,
    pub initiated_by_me: bool,
    tx: mpsc::Sender<Msg>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
}

impl PeerHandle {
    /// Spawns reader and writer tasks over an authenticated stream pair.
    /// Either task ending reports `PeerEvent::Gone` and tears the peer down.
    pub fn spawn(
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        initiated_by_me: bool,
        events: mpsc::Sender<PeerEvent>,
    ) -> Self {
        let id = conn.remote_id();
        let conn_id = conn.stable_id();
        let (tx, mut out_rx) = mpsc::channel::<Msg>(1024);

        let ev_r = events.clone();
        let mut recv = recv;
        let reader = tokio::spawn(async move {
            loop {
                match proto::read_msg(&mut recv).await {
                    Ok(msg) => {
                        if ev_r.send(PeerEvent::Msg { id, msg }).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        debug!(peer = %id.fmt_short(), "control read ended: {e}");
                        break;
                    }
                }
            }
            let _ = ev_r.send(PeerEvent::Gone { id, conn_id }).await;
        });

        let ev_w = events;
        let mut send = send;
        let writer = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if let Err(e) = proto::write_msg(&mut send, &msg).await {
                    debug!(peer = %id.fmt_short(), "control write ended: {e}");
                    break;
                }
            }
            let _ = ev_w.send(PeerEvent::Gone { id, conn_id }).await;
        });

        Self {
            id,
            conn,
            initiated_by_me,
            tx,
            reader,
            writer,
        }
    }

    /// The endpoint id that opened this connection (for duplicate dedup).
    pub fn initiator_id(&self, me: EndpointId) -> EndpointId {
        if self.initiated_by_me { me } else { self.id }
    }

    pub fn conn_id(&self) -> usize {
        self.conn.stable_id()
    }

    /// Queues a message; drops it (returning false) if the writer is saturated
    /// or gone — the connection teardown path will follow shortly after.
    pub fn send(&self, msg: Msg) -> bool {
        self.tx.try_send(msg).is_ok()
    }

    pub fn close(&self) {
        self.reader.abort();
        self.writer.abort();
        self.conn.close(VarInt::from_u32(0), b"bye");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_from(byte: u8) -> EndpointId {
        // Derive a valid public key from a deterministic secret.
        iroh::SecretKey::from_bytes(&[byte; 32]).public()
    }

    #[test]
    fn proof_vectors_match_manual_construction() {
        let key = [0x42u8; 32];
        let a = id_from(1);
        let b = id_from(2);
        let na = [0x0Au8; 16];
        let nb = [0x0Bu8; 16];

        // Independent construction: raw HMAC over the concatenated fields in
        // sorted-id order, which is what the wire spec defines.
        let (lo, hi) = if a.as_bytes() <= b.as_bytes() {
            (a, b)
        } else {
            (b, a)
        };
        let mut manual = Vec::new();
        manual.extend_from_slice(b"resp");
        manual.extend_from_slice(lo.as_bytes());
        manual.extend_from_slice(hi.as_bytes());
        manual.extend_from_slice(&na);
        manual.extend_from_slice(&nb);
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(&manual);
        let expected: [u8; 32] = mac.finalize().into_bytes().into();

        assert_eq!(proof(&key, LABEL_RESP, &a, &b, &na, &nb), expected);
    }

    #[test]
    fn proof_is_symmetric_in_ids_and_separated_by_label() {
        let key = [7u8; 32];
        let a = id_from(3);
        let b = id_from(4);
        let na = [1u8; 16];
        let nb = [2u8; 16];
        // Same proof regardless of argument order (ids are sorted inside).
        assert_eq!(
            proof(&key, LABEL_RESP, &a, &b, &na, &nb),
            proof(&key, LABEL_RESP, &b, &a, &na, &nb)
        );
        // Different labels, keys, or nonces change the proof.
        assert_ne!(
            proof(&key, LABEL_RESP, &a, &b, &na, &nb),
            proof(&key, LABEL_INIT, &a, &b, &na, &nb)
        );
        assert_ne!(
            proof(&key, LABEL_RESP, &a, &b, &na, &nb),
            proof(&[8u8; 32], LABEL_RESP, &a, &b, &na, &nb)
        );
        assert_ne!(
            proof(&key, LABEL_RESP, &a, &b, &na, &nb),
            proof(&key, LABEL_RESP, &a, &b, &nb, &na)
        );
    }
}
