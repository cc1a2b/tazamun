//! Control-plane wire protocol: framing and message types.
//!
//! Invariant: a frame is `u32` big-endian length followed by a postcard body;
//! length 0 and length > [`MAX_FRAME`] are rejected before any allocation, and
//! any decode error is fatal for the connection that produced it.

use std::collections::BTreeMap;

use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

use crate::consts::MAX_FRAME;
use crate::session::SignedGrant;
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
    Held {
        by: String,
    },
    TieLost,
    // ── Protocol minor 3 (P6): appended after `TieLost` so `Held` = 0 and
    // `TieLost` = 1 keep their postcard discriminants (append-only wire compat).
    /// The responder is at its tracked-lease capacity (a DoS bound), so it
    /// declines to track a new lease rather than grow without limit.
    Unavailable,
    // ── Protocol minor 4 (P17): appended after `Unavailable`. ──
    /// The requester's authenticated role may not take leases (viewer/archive),
    /// so an honest grantor refuses even a modified binary's lock request.
    RoleForbidden,
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
    // ── Protocol minor 2 (P4): waitlist. Appended after `Bye` so every prior
    // variant keeps its postcard discriminant (append-only wire compat). ──
    /// "I want this path when you release it" — sent by a waiter to the holder.
    LockInterest {
        path: RelPath,
    },
    /// "This path is now free" — broadcast on release/expiry so waiters retry.
    LockFreed {
        path: RelPath,
    },
    // ── Protocol minor 4 (P17): appended after `LockFreed`. ──
    /// This peer's signed role grant, sent right after the handshake so the
    /// receiver records and enforces this peer's role (verified against the
    /// shared admin public key). Absent on a legacy (v1) session.
    Identity {
        grant: SignedGrant,
    },
    // ── Protocol minor 5 (P20): appended after `Identity`. ──
    /// One shard of an index too large to fit a single [`MAX_FRAME`] frame. A
    /// peer sends parts as an ordered run `seq = 0,1,2,…` with `last = true` on
    /// the final part; the receiver stages files and only commits them (and
    /// reconciles) once `last` arrives, so freshness/voter logic still sees a
    /// *complete* index. `leases` are carried on the final part only. An index
    /// that fits still ships as a single [`Msg::Index`].
    IndexPart {
        seq: u32,
        last: bool,
        lamport: u64,
        files: Vec<(RelPath, FileRecord)>,
        leases: Vec<LeaseInfo>,
    },
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
            Msg::LockInterest { .. } => "lock_interest",
            Msg::LockFreed { .. } => "lock_freed",
            Msg::Identity { .. } => "identity",
            Msg::IndexPart { .. } => "index_part",
        }
    }

    /// The relative-path strings this message carries. These are untrusted
    /// until they pass [`crate::sync::index::sanitize_rel_path`]; the daemon
    /// runs every one through that gate at the wire boundary, and the fuzz
    /// harness (`fuzz_msg`) uses this to exercise the sanitizer on decoded
    /// messages.
    pub fn wire_paths(&self) -> Vec<&str> {
        match self {
            Msg::Index { files, leases, .. } | Msg::IndexPart { files, leases, .. } => files
                .iter()
                .map(|(p, _)| p.as_str())
                .chain(leases.iter().map(|l| l.path.as_str()))
                .collect(),
            Msg::FileMeta { path, .. }
            | Msg::LockReq { path, .. }
            | Msg::LockGrant { path }
            | Msg::LockDeny { path, .. }
            | Msg::LockRelease { path }
            | Msg::LockRenew { path, .. }
            | Msg::LockInterest { path }
            | Msg::LockFreed { path } => vec![path.as_str()],
            Msg::Hello { .. }
            | Msg::HelloAck { .. }
            | Msg::Proof { .. }
            | Msg::Bye
            | Msg::Identity { .. } => vec![],
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

/// Encoded postcard length of a message, or `usize::MAX` if it somehow fails to
/// encode (treated as "too big" by the splitter's budget checks).
fn encoded_len(msg: &Msg) -> usize {
    postcard::to_stdvec(msg)
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
}

/// P20: splits an index into wire frames each provably `< MAX_FRAME`.
///
/// If the whole index fits one [`Msg::Index`], returns exactly that — preserving
/// the pre-P20 wire for the common small-folder case. Otherwise it size-aware
/// greedy-batches entries into ordered [`Msg::IndexPart`]s (`seq = 0,1,2,…`,
/// `last` on the final one). Leases ride the final part when they fit, else a
/// trailing files-empty final part, so no frame ever exceeds `MAX_FRAME`
/// (leases alone are bounded by `MAX_TRACKED_LEASES` and always fit one frame).
/// Pure and unit-tested; no I/O.
pub fn split_index_parts(
    lamport: u64,
    files: &BTreeMap<RelPath, FileRecord>,
    leases: Vec<LeaseInfo>,
) -> Vec<Msg> {
    // Fast path: the whole thing in one Index frame (unchanged wire).
    let whole = Msg::Index {
        lamport,
        files: files.iter().map(|(p, r)| (p.clone(), r.clone())).collect(),
        leases: leases.clone(),
    };
    if encoded_len(&whole) <= MAX_FRAME {
        return vec![whole];
    }

    // Overflow: greedy size-aware batching. The 256-byte seed + the generous
    // (MAX_FRAME − 256 KiB) budget leave ample room for the part envelope and
    // one worst-case entry above the budget line.
    let budget = crate::consts::INDEX_PART_BUDGET;
    let mut batches: Vec<Vec<(RelPath, FileRecord)>> = Vec::new();
    let mut cur: Vec<(RelPath, FileRecord)> = Vec::new();
    let mut cur_bytes = 256usize;
    for (p, r) in files {
        let entry = (p.clone(), r.clone());
        // +2 for the postcard length/framing slack of one more Vec element.
        let elen = postcard::to_stdvec(&entry).map(|b| b.len()).unwrap_or(0) + 2;
        if !cur.is_empty() && cur_bytes + elen > budget {
            batches.push(std::mem::take(&mut cur));
            cur_bytes = 256;
        }
        // Checked once `cur` is a fresh batch: a single entry that alone cannot
        // fit ANY frame is unsyncable — skip it rather than emit an oversized
        // part that would brick the whole index. (Ingest caps the vv, so this is
        // defensive belt-and-suspenders.)
        if cur.is_empty() && 256 + elen > MAX_FRAME {
            continue;
        }
        cur_bytes += elen;
        cur.push(entry);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }

    let mut parts: Vec<Msg> = Vec::new();
    // Pull the final batch out so `leases` moves exactly once (after the loop).
    let last_batch = batches.pop();
    for (i, batch) in batches.into_iter().enumerate() {
        parts.push(Msg::IndexPart {
            seq: i as u32,
            last: false,
            lamport,
            files: batch,
            leases: vec![],
        });
    }
    match last_batch {
        Some(batch) => {
            let seq = parts.len() as u32;
            // Attach leases to the final file part if the frame still fits, else
            // spill them into a trailing files-empty final part.
            let with_leases = Msg::IndexPart {
                seq,
                last: true,
                lamport,
                files: batch.clone(),
                leases: leases.clone(),
            };
            if encoded_len(&with_leases) <= MAX_FRAME {
                parts.push(with_leases);
            } else {
                parts.push(Msg::IndexPart {
                    seq,
                    last: false,
                    lamport,
                    files: batch,
                    leases: vec![],
                });
                parts.push(leases_only_part(seq + 1, lamport, leases));
            }
        }
        // No file batches (only leases): one final part.
        None => parts.push(leases_only_part(0, lamport, leases)),
    }
    parts
}

/// Builds a final `last=true` files-empty `IndexPart` carrying `leases`,
/// truncating the list until the frame fits `MAX_FRAME`. Leases are advisory
/// hints (re-advertised), so dropping trailing ones on a pathological
/// long-path/huge-count set is safe degradation, and guarantees the part is
/// sendable — no leases-only frame can ever be oversized.
fn leases_only_part(seq: u32, lamport: u64, mut leases: Vec<LeaseInfo>) -> Msg {
    loop {
        let part = Msg::IndexPart {
            seq,
            last: true,
            lamport,
            files: vec![],
            leases: leases.clone(),
        };
        if leases.is_empty() || encoded_len(&part) <= MAX_FRAME {
            return part;
        }
        // Drop ~10% (at least one) and retry.
        let drop = (leases.len() / 10).max(1);
        leases.truncate(leases.len() - drop);
    }
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

/// Decodes one complete framed message from an in-memory buffer, applying the
/// exact rules of [`read_msg`]: a `u32` big-endian length prefix, reject length
/// `0` or `> MAX_FRAME`, then postcard-decode exactly that many body bytes.
///
/// Pure and allocation-bounded (it never allocates more than the caller already
/// holds), so the fuzz harness (`fuzz/fuzz_targets/fuzz_frame.rs`) and the
/// framing regression tests exercise the decoder without a live QUIC stream.
pub fn decode_frame(buf: &[u8]) -> Result<Msg, ProtoError> {
    let len_bytes: [u8; 4] = buf
        .get(..4)
        .and_then(|s| s.try_into().ok())
        .ok_or(ProtoError::BadLength(buf.len()))?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(ProtoError::BadLength(len));
    }
    let body = buf.get(4..4 + len).ok_or(ProtoError::BadLength(len))?;
    postcard::from_bytes(body).map_err(ProtoError::Decode)
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
    use crate::sync::vclock::VClock;

    /// A synthetic file map of `n` entries with a Blob manifest and 1-entry
    /// vclock (the common per-entry shape).
    fn synth(n: usize) -> BTreeMap<RelPath, FileRecord> {
        let mut m = BTreeMap::new();
        for i in 0..n {
            let rel = RelPath::new_unchecked(format!("d{:03}/f{:08}.bin", i % 512, i));
            m.insert(
                rel,
                FileRecord {
                    size: i as u64,
                    manifest: ManifestRef::Blob { hash: [7u8; 32] },
                    vv: VClock::from([("a".repeat(64), i as u64)]),
                    deleted: i % 7 == 0,
                    updated_at_ms: 1,
                },
            );
        }
        m
    }

    #[test]
    fn split_fast_path_small_index_is_one_frame() {
        let files = synth(3);
        let parts = split_index_parts(9, &files, vec![]);
        assert_eq!(parts.len(), 1);
        assert!(matches!(parts[0], Msg::Index { .. }));
        let body = postcard::to_stdvec(&parts[0]).unwrap();
        assert!(!body.is_empty() && body.len() <= MAX_FRAME);
    }

    #[test]
    fn split_large_index_shards_and_reassembles() {
        // 50k entries far exceeds MAX_FRAME as one Index (~6.7 MB), so it must
        // shard. Each part must be a writable frame, seqs contiguous, exactly
        // one `last`, and the union of files must equal the input.
        let files = synth(50_000);
        let parts = split_index_parts(1, &files, vec![]);
        assert!(parts.len() > 1, "50k entries must shard into >1 part");

        let mut merged: BTreeMap<RelPath, FileRecord> = BTreeMap::new();
        let mut last_count = 0;
        for (i, part) in parts.iter().enumerate() {
            // Every part fits one frame — the whole point.
            let body = postcard::to_stdvec(part).unwrap();
            assert!(
                !body.is_empty() && body.len() <= MAX_FRAME,
                "part {i} is {} bytes",
                body.len()
            );
            let Msg::IndexPart {
                seq, last, files, ..
            } = part
            else {
                panic!("expected IndexPart, got {}", part.kind());
            };
            assert_eq!(*seq as usize, i, "seqs must be contiguous 0..n");
            if *last {
                last_count += 1;
                assert_eq!(i, parts.len() - 1, "last must be the final part");
            }
            for (p, r) in files {
                merged.insert(p.clone(), r.clone());
            }
        }
        assert_eq!(last_count, 1, "exactly one final part");
        assert_eq!(merged.len(), files.len(), "reassembly drops/dupes nothing");
        assert_eq!(merged, files, "reassembled map equals the input");
        assert!((parts.len() as u32) < crate::consts::MAX_INDEX_PARTS);
    }

    #[test]
    fn split_never_emits_an_oversized_part_even_with_a_pathological_entry() {
        // A single entry whose version vector alone exceeds MAX_FRAME must be
        // skipped, never emitted — otherwise the sender bricks itself on write.
        let mut files = synth(10);
        let mut huge = VClock::new();
        // ~80k 64-byte keys ⇒ the single entry encodes to > MAX_FRAME (4 MiB).
        for i in 0..80_000u64 {
            huge.insert(format!("{i:064x}"), i);
        }
        files.insert(
            RelPath::new_unchecked("huge.bin".into()),
            FileRecord {
                size: 1,
                manifest: ManifestRef::Blob { hash: [0u8; 32] },
                vv: huge,
                deleted: false,
                updated_at_ms: 1,
            },
        );
        let parts = split_index_parts(1, &files, vec![]);
        // EVERY emitted frame is writable.
        for part in &parts {
            let body = postcard::to_stdvec(part).unwrap();
            assert!(
                body.len() <= MAX_FRAME,
                "emitted a {} byte part (> MAX_FRAME)",
                body.len()
            );
        }
        // The unframeable entry is dropped; the normal entries still ship.
        let mut merged = BTreeMap::new();
        for part in &parts {
            if let Msg::IndexPart { files, .. } = part {
                for (p, r) in files {
                    merged.insert(p.clone(), r.clone());
                }
            }
        }
        assert!(
            !merged.contains_key(&RelPath::new_unchecked("huge.bin".into())),
            "the pathological entry must be skipped"
        );
        assert_eq!(merged.len(), 10, "the 10 normal entries still sync");
    }

    #[test]
    fn split_carries_leases_on_the_final_part_only() {
        let files = synth(50_000);
        let leases = vec![LeaseInfo {
            path: RelPath::new_unchecked("d000/f00000000.bin".into()),
            holder: "b".repeat(64),
            lamport: 3,
            expires_in_ms: 1000,
        }];
        let parts = split_index_parts(1, &files, leases.clone());
        let with_leases: Vec<&Msg> = parts
            .iter()
            .filter(|p| matches!(p, Msg::IndexPart { leases, .. } if !leases.is_empty()))
            .collect();
        assert_eq!(with_leases.len(), 1, "leases on exactly one part");
        assert!(
            matches!(with_leases[0], Msg::IndexPart { last: true, .. }),
            "leases ride the final part"
        );
    }

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
