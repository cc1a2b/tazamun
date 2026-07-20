//! Generates seed corpora for the cargo-fuzz targets from real, valid encoded
//! artifacts — framed messages, `tzm1…` tickets, and chunk manifests. Run once
//! before fuzzing:
//!
//! ```text
//! cargo run --example gen_seeds
//! ```
//!
//! Seeds land under `fuzz/corpus/<target>/`. libFuzzer synthesizes inputs on
//! its own too, so seeds only accelerate coverage — they are not required.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tazamun::proto::{ChunkRef, DenyReason, FileRecord, LeaseInfo, ManifestRef, Msg};
use tazamun::session::{AddrWire, SessionSecret, Ticket};
use tazamun::state::RelPath;
use tazamun::sync::index::sanitize_rel_path;

fn corpus_dir(target: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("corpus")
        .join(target)
}

fn write_seed(target: &str, name: &str, bytes: &[u8]) {
    let dir = corpus_dir(target);
    std::fs::create_dir_all(&dir).expect("create corpus dir");
    std::fs::write(dir.join(name), bytes).expect("write seed");
}

/// A length-prefixed control frame: `u32` big-endian length + postcard body.
fn frame(msg: &Msg) -> Vec<u8> {
    let body = postcard::to_stdvec(msg).expect("encode msg");
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

fn sample_vv() -> BTreeMap<String, u64> {
    let mut vv = BTreeMap::new();
    vv.insert("aaaaaaaa".to_string(), 3);
    vv.insert("bbbbbbbb".to_string(), 1);
    vv
}

fn rel(s: &str) -> RelPath {
    sanitize_rel_path(s).expect("sample path is valid")
}

fn sample_record() -> FileRecord {
    FileRecord {
        size: 5,
        manifest: ManifestRef::Inline(vec![ChunkRef {
            hash: [7u8; 32],
            len: 5,
        }]),
        vv: sample_vv(),
        deleted: false,
        updated_at_ms: 1234,
    }
}

fn sample_messages() -> Vec<(&'static str, Msg)> {
    let files = vec![
        (rel("notes/todo.txt"), sample_record()),
        (rel("gone.bin"), FileRecord::tombstone(sample_vv(), 9999)),
    ];
    let leases = vec![LeaseInfo {
        path: rel("notes/todo.txt"),
        holder: "aaaaaaaa".to_string(),
        lamport: 4,
        expires_in_ms: 90_000,
    }];
    vec![
        ("hello", Msg::Hello { nonce: [1u8; 16] }),
        (
            "index",
            Msg::Index {
                lamport: 7,
                files,
                leases,
            },
        ),
        (
            "file_meta",
            Msg::FileMeta {
                path: rel("a/b.txt"),
                record: sample_record(),
                lamport: 8,
            },
        ),
        (
            "lock_req",
            Msg::LockReq {
                path: rel("a/b.txt"),
                lamport: 9,
                ttl_ms: 90_000,
            },
        ),
        (
            "lock_deny",
            Msg::LockDeny {
                path: rel("a/b.txt"),
                reason: DenyReason::Held {
                    by: "bbbbbbbb".to_string(),
                },
            },
        ),
        ("bye", Msg::Bye),
    ]
}

fn main() {
    // fuzz_frame (framed) + fuzz_msg (raw postcard) from the same messages.
    for (name, msg) in sample_messages() {
        write_seed("fuzz_frame", &format!("{name}.bin"), &frame(&msg));
        write_seed(
            "fuzz_msg",
            &format!("{name}.bin"),
            &postcard::to_stdvec(&msg).expect("encode msg"),
        );
    }

    // fuzz_ticket: real tzm1 tickets (with and without bootstrap addresses).
    let ticket = Ticket::new(
        SessionSecret([3u8; 32]),
        vec![AddrWire {
            id: [5u8; 32],
            relay: Some("https://relay.example.com./".to_string()),
            direct: vec!["127.0.0.1:4433".parse().expect("valid socket addr")],
        }],
    );
    write_seed("fuzz_ticket", "valid.txt", ticket.encode().as_bytes());
    let empty = Ticket::new(SessionSecret([0u8; 32]), vec![]);
    write_seed("fuzz_ticket", "no_bootstrap.txt", empty.encode().as_bytes());

    // fuzz_manifest: postcard Vec<ChunkRef> of a few shapes.
    let small: Vec<ChunkRef> = (0..4u32)
        .map(|i| ChunkRef {
            hash: [i as u8; 32],
            len: i + 1,
        })
        .collect();
    write_seed(
        "fuzz_manifest",
        "small.bin",
        &postcard::to_stdvec(&small).expect("encode manifest"),
    );
    let empty_manifest: Vec<ChunkRef> = vec![];
    write_seed(
        "fuzz_manifest",
        "empty.bin",
        &postcard::to_stdvec(&empty_manifest).expect("encode manifest"),
    );

    println!("seed corpora written under fuzz/corpus/");
}
