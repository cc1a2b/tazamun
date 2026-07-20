//! P20 scale: prove the two limits that used to brick a large folder are gone,
//! without creating tens of thousands of real files — the split and cap cores
//! are pure, so records are built in memory.
//!
//! Old limits (pre-P20): the connect-time index was one `Msg::Index` frame, so
//! a folder past ~31k files exceeded `MAX_FRAME` and the peer could never sync;
//! `status --json` embedded every file, overflowing `IPC_LINE_MAX` at ~8.7k.

use std::collections::BTreeMap;

use tazamun::consts::{FILES_LIST_MAX, IPC_LINE_MAX, MAX_FRAME, MAX_INDEX_PARTS};
use tazamun::daemon::files_json_capped;
use tazamun::proto::{FileRecord, ManifestRef, Msg, split_index_parts};
use tazamun::state::RelPath;
use tazamun::sync::index::sanitize_rel_path;
use tazamun::sync::vclock::VClock;

fn synth(n: usize) -> BTreeMap<RelPath, FileRecord> {
    let mut m = BTreeMap::new();
    for i in 0..n {
        let rel = sanitize_rel_path(&format!("d{:03}/f{:08}.bin", i % 512, i)).unwrap();
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
fn a_50k_file_index_shards_into_writable_frames_and_reassembles() {
    let files = synth(50_000);
    // Old behavior: one Index frame would be ~6-7 MB > MAX_FRAME (unsendable).
    let one = Msg::Index {
        lamport: 1,
        files: files.iter().map(|(p, r)| (p.clone(), r.clone())).collect(),
        leases: vec![],
    };
    assert!(
        postcard::to_stdvec(&one).unwrap().len() > MAX_FRAME,
        "the regression witness: 50k files DON'T fit one frame"
    );

    // New behavior: split into parts, each a writable frame.
    let parts = split_index_parts(1, &files, vec![]);
    assert!(parts.len() > 1 && (parts.len() as u32) < MAX_INDEX_PARTS);
    let mut merged = BTreeMap::new();
    for part in &parts {
        let body = postcard::to_stdvec(part).unwrap();
        assert!(
            !body.is_empty() && body.len() <= MAX_FRAME,
            "part is {} bytes, over MAX_FRAME",
            body.len()
        );
        if let Msg::IndexPart { files, .. } = part {
            for (p, r) in files {
                merged.insert(p.clone(), r.clone());
            }
        }
    }
    assert_eq!(merged, files, "reassembled index equals the original");
}

#[test]
fn status_files_map_stays_under_the_ipc_line_at_100k_files() {
    let files = synth(100_000);

    // The uncapped map would overflow the 1 MiB IPC line (the old bug).
    let (capped, total, truncated) = files_json_capped(&files, FILES_LIST_MAX);
    assert_eq!(total, 100_000);
    assert!(truncated);
    assert_eq!(capped.len(), FILES_LIST_MAX);

    // A full status-shaped line embedding the capped map plus generous member
    // and lease placeholders must fit the IPC line cap.
    let members: Vec<serde_json::Value> = (0..128)
        .map(|i| serde_json::json!({ "id": format!("{i:064x}"), "online": true, "conn": "Direct" }))
        .collect();
    let line = serde_json::json!({
        "schema": 1,
        "members": members,
        "files": capped,
        "files_total": total,
        "files_truncated": truncated,
        "file_count": total,
    });
    let bytes = serde_json::to_vec(&line).unwrap();
    assert!(
        bytes.len() <= IPC_LINE_MAX,
        "capped status line is {} bytes, over IPC_LINE_MAX",
        bytes.len()
    );

    // Regression witness: the UNCAPPED map alone would blow the line.
    let (uncapped, ..) = files_json_capped(&files, usize::MAX);
    let uncapped_bytes = serde_json::to_vec(&serde_json::Value::Object(uncapped)).unwrap();
    assert!(
        uncapped_bytes.len() > IPC_LINE_MAX,
        "the bug the cap fixes: uncapped 100k files exceed the IPC line"
    );
}
