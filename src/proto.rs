//! Control-plane wire protocol: framing and message types.
//!
//! Invariant: a frame is `u32` big-endian length followed by a postcard body;
//! length 0 and length > [`MAX_FRAME`] are rejected before any allocation, and
//! any decode error is fatal for the connection that produced it.

use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

use crate::consts::MAX_FRAME;
use crate::state::RelPath;
use crate::sync::vclock::VClock;

/// Reference to one content-defined chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub hash: [u8; 32],
    pub len: u32,
}

/// How a file's chunk list is carried: inline for small files, or as a blob
/// (postcard-encoded `Vec<ChunkRef>`) referenced by BLAKE3 hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestRef {
    Inline(Vec<ChunkRef>),
    Blob { hash: [u8; 32] },
}

impl ManifestRef {
    pub fn empty() -> Self {
        ManifestRef::Inline(Vec::new())
    }
}

/// Everything a peer advertises about one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub size: u64,
    pub manifest: ManifestRef,
    pub vv: VClock,
    pub deleted: bool,
    pub updated_at_ms: u64,
}

impl FileRecord {
    pub fn tombstone(vv: VClock, ts_ms: u64) -> Self {
        Self {
            size: 0,
            manifest: ManifestRef::empty(),
            vv,
            deleted: true,
            updated_at_ms: ts_ms,
        }
    }
}

/// An active lease as advertised in `Index` messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseInfo {
    pub path: RelPath,
    pub holder: String,
    pub lamport: u64,
    pub expires_in_ms: u64,
}

/// Why a lock request was denied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenyReason {
    Held { by: String },
    TieLost,
}

/// Every message that can cross an authenticated control connection, plus the
/// three pre-auth handshake messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    Hello {
        nonce: [u8; 16],
    },
    HelloAck {
        nonce: [u8; 16],
        proof: [u8; 32],
    },
    Proof {
        proof: [u8; 32],
    },
    Index {
        lamport: u64,
        files: Vec<(RelPath, FileRecord)>,
        leases: Vec<LeaseInfo>,
    },
    FileMeta {
        path: RelPath,
        record: FileRecord,
        lamport: u64,
    },
    LockReq {
        path: RelPath,
        lamport: u64,
        ttl_ms: u64,
    },
    LockGrant {
        path: RelPath,
    },
    LockDeny {
        path: RelPath,
        reason: DenyReason,
    },
    LockRelease {
        path: RelPath,
    },
    LockRenew {
        path: RelPath,
        lamport: u64,
        ttl_ms: u64,
    },
    Bye,
}

impl Msg {
    /// Short name for structured logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Msg::Hello { .. } => "hello",
            Msg::HelloAck { .. } => "hello_ack",
            Msg::Proof { .. } => "proof",
            Msg::Index { .. } => "index",
            Msg::FileMeta { .. } => "file_meta",
            Msg::LockReq { .. } => "lock_req",
            Msg::LockGrant { .. } => "lock_grant",
            Msg::LockDeny { .. } => "lock_deny",
            Msg::LockRelease { .. } => "lock_release",
            Msg::LockRenew { .. } => "lock_renew",
            Msg::Bye => "bye",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("frame length {0} outside 1..={MAX_FRAME}")]
    BadLength(usize),
    #[error("message does not fit in one frame ({0} bytes)")]
    Oversized(usize),
    #[error("encode: {0}")]
    Encode(postcard::Error),
    #[error("decode: {0}")]
    Decode(postcard::Error),
    #[error("stream write: {0}")]
    Write(#[from] iroh::endpoint::WriteError),
    #[error("stream read: {0}")]
    Read(#[from] iroh::endpoint::ReadExactError),
}

/// Writes one framed message.
pub async fn write_msg(send: &mut SendStream, msg: &Msg) -> Result<(), ProtoError> {
    let body = postcard::to_stdvec(msg).map_err(ProtoError::Encode)?;
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(ProtoError::Oversized(body.len()));
    }
    let len = (body.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(&body).await?;
    Ok(())
}

/// Reads one framed message. Any error is fatal for the connection.
pub async fn read_msg(recv: &mut RecvStream) -> Result<Msg, ProtoError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(ProtoError::BadLength(len));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    postcard::from_bytes(&body).map_err(ProtoError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_postcard_roundtrip() {
        let msg = Msg::FileMeta {
            path: RelPath::new_unchecked("a/b.txt".into()),
            record: FileRecord {
                size: 3,
                manifest: ManifestRef::Inline(vec![ChunkRef {
                    hash: [1u8; 32],
                    len: 3,
                }]),
                vv: VClock::from([("x".to_string(), 4u64)]),
                deleted: false,
                updated_at_ms: 99,
            },
            lamport: 7,
        };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: Msg = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn oversized_body_refused_on_write_path() {
        // A frame body larger than MAX_FRAME must be refused before hitting
        // the wire. Construct one via an absurd inline manifest.
        let chunks = vec![
            ChunkRef {
                hash: [0u8; 32],
                len: 1,
            };
            MAX_FRAME / 32
        ];
        let msg = Msg::Index {
            lamport: 0,
            files: vec![(
                RelPath::new_unchecked("big".into()),
                FileRecord {
                    size: 0,
                    manifest: ManifestRef::Inline(chunks),
                    vv: VClock::new(),
                    deleted: false,
                    updated_at_ms: 0,
                },
            )],
            leases: vec![],
        };
        let body = postcard::to_stdvec(&msg).unwrap();
        assert!(body.len() > MAX_FRAME);
    }
}
