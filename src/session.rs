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

// ─── capability grants (P17: roles ride the invite) ──────────────────────────
//
// A session has, besides the shared secret, an Ed25519 *admin* keypair (reusing
// iroh's key types — an admin key is just another ed25519 keypair). Every invite
// carries a `Grant` — the role it confers, a unique invite id, and an optional
// expiry — *signed by the admin key*. Because the signature is asymmetric, a
// member who was handed only a viewer grant (which omits the admin secret)
// cannot forge an editor grant: they lack the signing key. So an honest grantor,
// which verifies each peer's grant against the shared admin *public* key before
// granting a lease, refuses a viewer's lock even if the viewer's binary was
// modified to bypass its own local role check. A symmetric MAC could not do this
// — everyone holding the session secret could mint any grant. (Co-editors are
// co-admins by design; roles constrain viewers/archives, not fellow editors.)

/// Stable wire code for [`crate::state::NodeRole::Editor`].
pub const ROLE_EDITOR: u8 = 0;
/// Stable wire code for [`crate::state::NodeRole::Viewer`].
pub const ROLE_VIEWER: u8 = 1;
/// Stable wire code for [`crate::state::NodeRole::Archive`].
pub const ROLE_ARCHIVE: u8 = 2;

/// Whether a role code may take leases and publish edits (only `editor`).
pub fn role_can_edit(code: u8) -> bool {
    code == ROLE_EDITOR
}

/// Human name for a role code (for logs / status).
pub fn role_name(code: u8) -> &'static str {
    match code {
        ROLE_EDITOR => "editor",
        ROLE_VIEWER => "viewer",
        ROLE_ARCHIVE => "archive",
        _ => "unknown",
    }
}

/// The capability a ticket confers, bound to a unique invite id and an optional
/// expiry. Signed by the session admin key (see [`SignedGrant`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// Role code (`ROLE_EDITOR` / `ROLE_VIEWER` / `ROLE_ARCHIVE`).
    pub role: u8,
    /// 16 random bytes uniquely identifying this invite (for future single-use
    /// tracking; already carried and signed so it cannot be altered).
    pub invite_id: [u8; 16],
    /// Epoch-ms the grant was issued.
    pub issued_ms: u64,
    /// Epoch-ms after which the grant is invalid; `0` means it never expires.
    pub expiry_ms: u64,
}

impl Grant {
    /// True when `now_ms` is at or past a non-zero expiry.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expiry_ms != 0 && now_ms >= self.expiry_ms
    }

    /// Domain-separated signing message: a fixed label plus the postcard body,
    /// so a grant signature can never be reused as any other signature.
    fn signing_bytes(&self) -> Vec<u8> {
        let mut m = Vec::from(&b"tazamun/v2/grant"[..]);
        m.extend_from_slice(&postcard::to_stdvec(self).unwrap_or_default());
        m
    }
}

/// A [`Grant`] plus its Ed25519 signature by the session admin key. The
/// signature is `iroh::Signature` (which carries its own serde), so a grant
/// travels intact in a ticket and on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedGrant {
    pub grant: Grant,
    pub sig: iroh::Signature,
}

impl SignedGrant {
    /// Verifies the signature against the session admin public key bytes.
    pub fn verify(&self, admin_public: &[u8; 32]) -> bool {
        let Ok(pk) = iroh::PublicKey::from_bytes(admin_public) else {
            return false;
        };
        pk.verify(&self.grant.signing_bytes(), &self.sig).is_ok()
    }
}

/// Signs `grant` with the admin secret key, producing a verifiable capability.
pub fn sign_grant(admin_secret: &iroh::SecretKey, grant: Grant) -> SignedGrant {
    let sig = admin_secret.sign(&grant.signing_bytes());
    SignedGrant { grant, sig }
}

/// An invite ticket: the session secret plus bootstrap addresses, and — for a
/// v2 session — the admin public key, a signed role grant, and (for editor
/// invites only) the admin secret key so the invitee can invite further.
#[derive(Clone)]
pub struct Ticket {
    pub secret: SessionSecret,
    pub bootstrap: Vec<AddrWire>,
    /// P17: the shared admin public key (verify key). `None` on a v1 ticket.
    pub admin_public: Option<[u8; 32]>,
    /// P17: this invite's signed role grant. `None` on a v1 ticket.
    pub grant: Option<SignedGrant>,
    /// P17: the admin secret key, present only in editor invites so the invitee
    /// can mint further invites. Zeroized on drop.
    pub admin_secret: Option<SessionSecret>,
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("secret", &"..")
            .field("bootstrap", &self.bootstrap)
            .field("grant", &self.grant)
            .field("admin_public", &self.admin_public.map(|_| ".."))
            .field("admin_secret", &self.admin_secret.as_ref().map(|_| ".."))
            .finish()
    }
}

#[derive(Serialize, Deserialize, Zeroize)]
struct TicketWireV1 {
    version: u8,
    secret: [u8; 32],
    #[zeroize(skip)]
    bootstrap: Vec<AddrWire>,
}

#[derive(Serialize, Deserialize, Zeroize)]
struct TicketWireV2 {
    version: u8,
    secret: [u8; 32],
    admin_public: [u8; 32],
    #[zeroize(skip)]
    grant: SignedGrant,
    admin_secret: Option<[u8; 32]>,
    #[zeroize(skip)]
    bootstrap: Vec<AddrWire>,
}

impl Ticket {
    /// A v1 ticket: session secret + bootstrap only (legacy / no roles).
    pub fn new(secret: SessionSecret, bootstrap: Vec<AddrWire>) -> Self {
        Self {
            secret,
            bootstrap,
            admin_public: None,
            grant: None,
            admin_secret: None,
        }
    }

    pub fn encode(&self) -> String {
        let bytes = match (&self.admin_public, &self.grant) {
            (Some(admin_public), Some(grant)) => {
                let mut wire = TicketWireV2 {
                    version: 2,
                    secret: self.secret.0,
                    admin_public: *admin_public,
                    grant: grant.clone(),
                    admin_secret: self.admin_secret.as_ref().map(|s| s.0),
                    bootstrap: self.bootstrap.clone(),
                };
                let bytes = postcard::to_stdvec(&wire).unwrap_or_default();
                wire.zeroize();
                bytes
            }
            _ => {
                let mut wire = TicketWireV1 {
                    version: 1,
                    secret: self.secret.0,
                    bootstrap: self.bootstrap.clone(),
                };
                let bytes = postcard::to_stdvec(&wire).unwrap_or_default();
                wire.zeroize();
                bytes
            }
        };
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
        // Postcard encodes the leading `version: u8` as its first byte, so we can
        // dispatch on it before committing to a struct shape (append-only wire).
        match bytes.first() {
            Some(1) => {
                let wire: TicketWireV1 =
                    postcard::from_bytes(&bytes).map_err(|_| TicketError::Malformed)?;
                Ok(Self {
                    secret: SessionSecret(wire.secret),
                    bootstrap: wire.bootstrap,
                    admin_public: None,
                    grant: None,
                    admin_secret: None,
                })
            }
            Some(2) => {
                let wire: TicketWireV2 =
                    postcard::from_bytes(&bytes).map_err(|_| TicketError::Malformed)?;
                Ok(Self {
                    secret: SessionSecret(wire.secret),
                    bootstrap: wire.bootstrap,
                    admin_public: Some(wire.admin_public),
                    grant: Some(wire.grant),
                    admin_secret: wire.admin_secret.map(SessionSecret),
                })
            }
            Some(v) => Err(TicketError::BadVersion(*v)),
            None => Err(TicketError::Malformed),
        }
    }
}

/// Mints an invite. When `admin` is `Some((signer, admin_public))` the session
/// is v2: a signed role grant is embedded, and the admin secret rides along only
/// for `editor` invites (so an editor can invite/rekey; a viewer cannot sign, so
/// cannot forge an editor grant). When `admin` is `None` the session is legacy
/// v1 and a plain secret+bootstrap ticket is produced (role/ttl ignored).
#[allow(clippy::too_many_arguments)]
pub fn mint_ticket(
    session_secret: [u8; 32],
    admin: Option<(&iroh::SecretKey, [u8; 32])>,
    role_code: u8,
    invite_id: [u8; 16],
    issued_ms: u64,
    ttl_ms: u64,
    bootstrap: Vec<AddrWire>,
) -> Ticket {
    let Some((signer, admin_public)) = admin else {
        return Ticket::new(SessionSecret(session_secret), bootstrap);
    };
    let expiry_ms = if ttl_ms == 0 {
        0
    } else {
        issued_ms.saturating_add(ttl_ms)
    };
    let grant = sign_grant(
        signer,
        Grant {
            role: role_code,
            invite_id,
            issued_ms,
            expiry_ms,
        },
    );
    // Editors carry the admin secret so they can invite further; viewers and
    // archives do not — that omission is what makes their role unforgeable.
    let admin_secret = if role_can_edit(role_code) {
        Some(SessionSecret(signer.to_bytes()))
    } else {
        None
    };
    Ticket {
        secret: SessionSecret(session_secret),
        bootstrap,
        admin_public: Some(admin_public),
        grant: Some(grant),
        admin_secret,
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
        let wire = TicketWireV1 {
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

    fn admin_keypair(seed: u8) -> (iroh::SecretKey, [u8; 32]) {
        let sk = iroh::SecretKey::from_bytes(&[seed; 32]);
        let pk = *sk.public().as_bytes();
        (sk, pk)
    }

    #[test]
    fn grant_signature_verifies_and_forgery_is_rejected() {
        let (admin, admin_pub) = admin_keypair(11);
        let grant = Grant {
            role: ROLE_VIEWER,
            invite_id: [9u8; 16],
            issued_ms: 1000,
            expiry_ms: 0,
        };
        let signed = sign_grant(&admin, grant.clone());
        assert!(signed.verify(&admin_pub), "honest signature must verify");

        // A different admin key cannot have produced this signature.
        let (_, other_pub) = admin_keypair(22);
        assert!(!signed.verify(&other_pub), "wrong admin key must reject");

        // Tampering with the role (viewer→editor) breaks the signature: a
        // modified binary cannot self-elevate without the admin secret.
        let mut forged = signed.clone();
        forged.grant.role = ROLE_EDITOR;
        assert!(!forged.verify(&admin_pub), "tampered role must reject");
    }

    #[test]
    fn grant_expiry() {
        let g = Grant {
            role: ROLE_VIEWER,
            invite_id: [0u8; 16],
            issued_ms: 1000,
            expiry_ms: 5000,
        };
        assert!(!g.is_expired(4999));
        assert!(g.is_expired(5000));
        assert!(g.is_expired(9999));
        let never = Grant {
            expiry_ms: 0,
            ..g.clone()
        };
        assert!(!never.is_expired(u64::MAX), "expiry 0 = never");
    }

    #[test]
    fn v2_ticket_roundtrip_editor_carries_admin_secret() {
        let (admin, admin_pub) = admin_keypair(7);
        let boot = vec![AddrWire {
            id: [5u8; 32],
            relay: None,
            direct: vec![],
        }];
        // Editor invite: admin secret rides along; role verifies.
        let t = mint_ticket(
            [3u8; 32],
            Some((&admin, admin_pub)),
            ROLE_EDITOR,
            [1u8; 16],
            2000,
            3_600_000,
            boot.clone(),
        );
        let back = Ticket::decode(&t.encode()).unwrap();
        assert_eq!(back.secret.0, [3u8; 32]);
        assert_eq!(back.admin_public, Some(admin_pub));
        let g = back.grant.as_ref().expect("v2 grant");
        assert!(g.verify(&admin_pub));
        assert_eq!(g.grant.role, ROLE_EDITOR);
        assert_eq!(g.grant.expiry_ms, 2000 + 3_600_000);
        assert!(
            back.admin_secret.is_some(),
            "editor invite carries the admin secret"
        );
    }

    #[test]
    fn v2_viewer_invite_omits_admin_secret() {
        let (admin, admin_pub) = admin_keypair(4);
        let t = mint_ticket(
            [3u8; 32],
            Some((&admin, admin_pub)),
            ROLE_VIEWER,
            [2u8; 16],
            0,
            0, // no expiry
            vec![],
        );
        let back = Ticket::decode(&t.encode()).unwrap();
        assert_eq!(back.grant.as_ref().unwrap().grant.role, ROLE_VIEWER);
        assert_eq!(back.grant.as_ref().unwrap().grant.expiry_ms, 0);
        assert!(
            back.admin_secret.is_none(),
            "a viewer must not receive the admin secret — that is what makes the role unforgeable"
        );
    }

    #[test]
    fn mint_without_admin_is_legacy_v1() {
        let t = mint_ticket([3u8; 32], None, ROLE_VIEWER, [0u8; 16], 0, 999, vec![]);
        assert!(t.grant.is_none() && t.admin_public.is_none());
        // Encodes as a v1 ticket that a v1 decoder accepts.
        let back = Ticket::decode(&t.encode()).unwrap();
        assert!(back.grant.is_none());
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
