//! Session identity: key derivation and invite tickets.
//!
//! Invariant: everything a stranger needs to join a session is exactly one
//! ticket string; everything derived from the session secret (topic id,
//! handshake auth key, gossip encryption key) is reproducible on every member
//! from that secret alone, and all secret material is zeroized on drop.

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

const TICKET_PREFIX: &str = "tzm1";
const INFO_TOPIC: &[u8] = b"tazamun/v1/topic";
const INFO_AUTH: &[u8] = b"tazamun/v1/auth";
const INFO_GOSSIP: &[u8] = b"tazamun/v1/gossip";

/// The 32-byte shared session secret. Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionSecret(pub [u8; 32]);

impl std::fmt::Debug for SessionSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionSecret(..)")
    }
}

/// Keys derived from the session secret via HKDF-SHA256.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    /// Gossip topic id bytes (`tazamun/v1/topic`).
    pub topic: [u8; 32],
    /// Handshake HMAC key (`tazamun/v1/auth`).
    pub auth: [u8; 32],
    /// XChaCha20-Poly1305 key for gossip payloads (`tazamun/v1/gossip`).
    pub gossip: [u8; 32],
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKeys(..)")
    }
}

impl SessionKeys {
    pub fn derive(secret: &SessionSecret) -> Self {
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, &secret.0);
        let mut topic = [0u8; 32];
        let mut auth = [0u8; 32];
        let mut gossip = [0u8; 32];
        // 32-byte outputs from HKDF-SHA256 cannot exceed the 255*32 limit.
        for (info, out) in [
            (INFO_TOPIC, &mut topic),
            (INFO_AUTH, &mut auth),
            (INFO_GOSSIP, &mut gossip),
        ] {
            if hk.expand(info, out).is_err() {
                unreachable!("HKDF expand of 32 bytes is always valid");
            }
        }
        Self {
            topic,
            auth,
            gossip,
        }
    }
}

/// tazamun's own stable wire form of an [`EndpointAddr`], so ticket and state
/// formats do not depend on iroh's internal serde representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddrWire {
    pub id: [u8; 32],
    pub relay: Option<String>,
    pub direct: Vec<std::net::SocketAddr>,
}

#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("ticket does not start with `{TICKET_PREFIX}`")]
    BadPrefix,
    #[error("ticket body is not valid base32")]
    BadEncoding,
    #[error("unsupported ticket version {0}")]
    BadVersion(u8),
    #[error("ticket payload is malformed")]
    Malformed,
}

impl AddrWire {
    pub fn from_endpoint_addr(addr: &EndpointAddr) -> Self {
        let mut relay = None;
        let mut direct = Vec::new();
        for t in &addr.addrs {
            match t {
                TransportAddr::Relay(url) => {
                    relay.get_or_insert_with(|| url.to_string());
                }
                TransportAddr::Ip(sock) => direct.push(*sock),
                _ => {}
            }
        }
        Self {
            id: *addr.id.as_bytes(),
            relay,
            direct,
        }
    }

    /// Converts back into an iroh [`EndpointAddr`]; `None` when the embedded
    /// endpoint id is not a valid public key.
    pub fn to_endpoint_addr(&self) -> Option<EndpointAddr> {
        let id = EndpointId::from_bytes(&self.id).ok()?;
        let mut addrs = std::collections::BTreeSet::new();
        if let Some(url) = &self.relay
            && let Ok(url) = url.parse::<RelayUrl>()
        {
            addrs.insert(TransportAddr::Relay(url));
        }
        for sock in &self.direct {
            addrs.insert(TransportAddr::Ip(*sock));
        }
        Some(EndpointAddr { id, addrs })
    }

    pub fn endpoint_id(&self) -> Option<EndpointId> {
        EndpointId::from_bytes(&self.id).ok()
    }
}

/// An invite ticket: the session secret plus bootstrap addresses.
#[derive(Clone)]
pub struct Ticket {
    pub secret: SessionSecret,
    pub bootstrap: Vec<AddrWire>,
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("secret", &"..")
            .field("bootstrap", &self.bootstrap)
            .finish()
    }
}

#[derive(Serialize, Deserialize, Zeroize)]
struct TicketWire {
    version: u8,
    secret: [u8; 32],
    #[zeroize(skip)]
    bootstrap: Vec<AddrWire>,
}

impl Ticket {
    pub fn new(secret: SessionSecret, bootstrap: Vec<AddrWire>) -> Self {
        Self { secret, bootstrap }
    }

    pub fn encode(&self) -> String {
        let mut wire = TicketWire {
            version: 1,
            secret: self.secret.0,
            bootstrap: self.bootstrap.clone(),
        };
        // Postcard serialization of this closed struct cannot fail.
        let bytes = postcard::to_stdvec(&wire).unwrap_or_default();
        wire.zeroize();
        let mut out = String::from(TICKET_PREFIX);
        out.push_str(&data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase());
        out
    }

    pub fn decode(s: &str) -> Result<Self, TicketError> {
        let s = s.trim();
        let body = s
            .strip_prefix(TICKET_PREFIX)
            .ok_or(TicketError::BadPrefix)?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(|_| TicketError::BadEncoding)?;
        let wire: TicketWire = postcard::from_bytes(&bytes).map_err(|_| TicketError::Malformed)?;
        if wire.version != 1 {
            return Err(TicketError::BadVersion(wire.version));
        }
        Ok(Self {
            secret: SessionSecret(wire.secret),
            bootstrap: wire.bootstrap,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ticket {
        Ticket::new(
            SessionSecret([3u8; 32]),
            vec![AddrWire {
                id: [5u8; 32],
                relay: Some("https://relay.example.com./".to_string()),
                direct: vec!["127.0.0.1:4433".parse().unwrap()],
            }],
        )
    }

    #[test]
    fn ticket_roundtrip() {
        let t = sample();
        let s = t.encode();
        assert!(s.starts_with(TICKET_PREFIX));
        assert_eq!(s, s.to_lowercase());
        let back = Ticket::decode(&s).unwrap();
        assert_eq!(back.secret.0, t.secret.0);
        assert_eq!(back.bootstrap, t.bootstrap);
    }

    #[test]
    fn wrong_prefix_rejected() {
        let s = sample().encode();
        let bad = format!("xzm1{}", &s[4..]);
        assert!(matches!(Ticket::decode(&bad), Err(TicketError::BadPrefix)));
    }

    #[test]
    fn tampered_body_rejected() {
        let s = sample().encode();
        // Truncation breaks either base32 grouping or postcard decoding.
        let bad = &s[..s.len() - 3];
        assert!(matches!(
            Ticket::decode(bad),
            Err(TicketError::BadEncoding) | Err(TicketError::Malformed)
        ));
        // An illegal base32 character is a distinct encoding error.
        let bad = format!("{}!", &s[..s.len() - 1]);
        assert!(matches!(
            Ticket::decode(&bad),
            Err(TicketError::BadEncoding)
        ));
    }

    #[test]
    fn bad_version_rejected() {
        let wire = TicketWire {
            version: 9,
            secret: [0u8; 32],
            bootstrap: vec![],
        };
        let bytes = postcard::to_stdvec(&wire).unwrap();
        let s = format!(
            "{TICKET_PREFIX}{}",
            data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
        );
        assert!(matches!(
            Ticket::decode(&s),
            Err(TicketError::BadVersion(9))
        ));
    }

    #[test]
    fn keys_derivation_is_deterministic_and_distinct() {
        let k1 = SessionKeys::derive(&SessionSecret([1u8; 32]));
        let k2 = SessionKeys::derive(&SessionSecret([1u8; 32]));
        let k3 = SessionKeys::derive(&SessionSecret([2u8; 32]));
        assert_eq!(k1.topic, k2.topic);
        assert_eq!(k1.auth, k2.auth);
        assert_eq!(k1.gossip, k2.gossip);
        assert_ne!(k1.topic, k3.topic);
        assert_ne!(k1.topic, k1.auth);
        assert_ne!(k1.auth, k1.gossip);
    }
}
