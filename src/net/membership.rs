//! Gossip membership: encrypted presence beacons over the session topic.
//!
//! Invariant: nothing readable leaves this module unencrypted — presence
//! payloads are XChaCha20-Poly1305 sealed under the session gossip key with
//! the topic id as AAD, and undecryptable gossip is ignored silently, so
//! membership metadata is unreadable without the session secret.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::Event as GossipEvent;
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, instrument, trace, warn};

use crate::consts::PRESENCE_INTERVAL;
use crate::session::{AddrWire, SessionKeys};

const XNONCE_LEN: usize = 24;

/// Decrypted presence beacon.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Presence {
    id: [u8; 32],
    addr: AddrWire,
    ts_ms: u64,
}

/// What the membership task reports back to the daemon.
#[derive(Debug)]
pub enum MemberEvent {
    Seen {
        id: EndpointId,
        addr: EndpointAddr,
        ts_ms: u64,
    },
    NeighborUp(EndpointId),
    NeighborDown(EndpointId),
}

/// Commands the daemon can push into the membership task.
#[derive(Debug)]
pub enum MemberCmd {
    /// Ask the gossip overlay to connect to these peers.
    JoinPeers(Vec<EndpointId>),
}

/// Seals a presence payload: random 24-byte nonce ‖ ciphertext, AAD = topic.
fn seal(keys: &SessionKeys, topic: &TopicId, presence: &Presence) -> Option<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new((&keys.gossip).into());
    let nonce_bytes: [u8; XNONCE_LEN] = rand::random();
    let nonce = XNonce::from(nonce_bytes);
    let msg = postcard::to_stdvec(presence).ok()?;
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &msg,
                aad: topic.as_bytes(),
            },
        )
        .ok()?;
    let mut out = Vec::with_capacity(XNONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Some(out)
}

/// Opens a presence payload; any failure yields `None` (ignored silently).
fn open(keys: &SessionKeys, topic: &TopicId, data: &[u8]) -> Option<Presence> {
    if data.len() <= XNONCE_LEN {
        return None;
    }
    let cipher = XChaCha20Poly1305::new((&keys.gossip).into());
    let nonce_bytes: [u8; XNONCE_LEN] = data[..XNONCE_LEN].try_into().ok()?;
    let nonce = XNonce::from(nonce_bytes);
    let pt = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &data[XNONCE_LEN..],
                aad: topic.as_bytes(),
            },
        )
        .ok()?;
    postcard::from_bytes(&pt).ok()
}

/// Joins the session gossip topic and runs the presence loop until the daemon
/// drops the command channel.
#[instrument(skip_all, fields(topic = %topic))]
pub async fn run(
    gossip: Gossip,
    endpoint: Endpoint,
    keys: SessionKeys,
    topic: TopicId,
    bootstrap: Vec<EndpointId>,
    events: mpsc::Sender<MemberEvent>,
    mut cmds: mpsc::Receiver<MemberCmd>,
) {
    let sub = match gossip.subscribe(topic, bootstrap).await {
        Ok(sub) => sub,
        Err(e) => {
            warn!("gossip subscribe failed: {e}");
            return;
        }
    };
    let (sender, mut receiver) = sub.split();
    let mut tick = tokio::time::interval(PRESENCE_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let me = endpoint.id();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let presence = Presence {
                    id: *me.as_bytes(),
                    addr: AddrWire::from_endpoint_addr(&endpoint.addr()),
                    ts_ms: crate::now_ms(),
                };
                if let Some(sealed) = seal(&keys, &topic, &presence)
                    && let Err(e) = sender.broadcast(sealed.into()).await
                {
                    debug!("presence broadcast failed: {e}");
                }
            }
            cmd = cmds.recv() => {
                match cmd {
                    Some(MemberCmd::JoinPeers(ids)) => {
                        if let Err(e) = sender.join_peers(ids).await {
                            debug!("gossip join_peers failed: {e}");
                        }
                    }
                    None => break,
                }
            }
            ev = receiver.next() => {
                match ev {
                    Some(Ok(GossipEvent::Received(msg))) => {
                        let Some(p) = open(&keys, &topic, &msg.content) else {
                            trace!("undecryptable gossip ignored");
                            continue;
                        };
                        let Some(id) = EndpointId::from_bytes(&p.id).ok() else {
                            continue;
                        };
                        if id == me {
                            continue;
                        }
                        let Some(addr) = p.addr.to_endpoint_addr() else {
                            continue;
                        };
                        if events
                            .send(MemberEvent::Seen { id, addr, ts_ms: p.ts_ms })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(GossipEvent::NeighborUp(id))) => {
                        if events.send(MemberEvent::NeighborUp(id)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(GossipEvent::NeighborDown(id))) => {
                        if events.send(MemberEvent::NeighborDown(id)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(GossipEvent::Lagged)) => {
                        debug!("gossip receiver lagged");
                    }
                    Some(Err(e)) => {
                        warn!("gossip receiver error: {e}");
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    debug!("membership task ended");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionKeys, SessionSecret};

    #[test]
    fn seal_open_roundtrip_and_wrong_key_rejected() {
        let keys = SessionKeys::derive(&SessionSecret([1u8; 32]));
        let other = SessionKeys::derive(&SessionSecret([2u8; 32]));
        let topic = TopicId::from_bytes([9u8; 32]);
        let p = Presence {
            id: [5u8; 32],
            addr: AddrWire {
                id: [5u8; 32],
                relay: None,
                direct: vec!["127.0.0.1:1".parse().unwrap()],
            },
            ts_ms: 123,
        };
        let sealed = seal(&keys, &topic, &p).unwrap();
        let back = open(&keys, &topic, &sealed).unwrap();
        assert_eq!(back.id, p.id);
        assert_eq!(back.ts_ms, 123);
        assert!(open(&other, &topic, &sealed).is_none());
        // Wrong AAD (different topic) must also fail.
        let wrong_topic = TopicId::from_bytes([8u8; 32]);
        assert!(open(&keys, &wrong_topic, &sealed).is_none());
        // Truncated payloads are ignored.
        assert!(open(&keys, &topic, &sealed[..10]).is_none());
    }
}
