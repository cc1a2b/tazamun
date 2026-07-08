//! Version history over [`AppState`].
//!
//! Invariant: history is append-at-front and truncated to
//! [`HISTORY_KEEP`](crate::consts::HISTORY_KEEP) entries; index 0 is always
//! the most recently replaced version of a path.

use crate::consts::HISTORY_KEEP;
use crate::proto::FileRecord;
use crate::state::{AppState, RelPath, VersionEntry};

/// Pushes the record being replaced onto the path's history, truncating to
/// the configured retention. Tombstone records carry no bytes and are skipped.
pub fn push(state: &mut AppState, path: &RelPath, replaced: &FileRecord) {
    if replaced.deleted {
        return;
    }
    let entries = state.history.entry(path.clone()).or_default();
    entries.insert(
        0,
        VersionEntry {
            manifest: replaced.manifest.clone(),
            vv: replaced.vv.clone(),
            ts_ms: replaced.updated_at_ms,
            size: replaced.size,
        },
    );
    entries.truncate(HISTORY_KEEP);
}

/// Lists a path's history newest-first as `(index, timestamp_ms, size)`.
pub fn list(state: &AppState, path: &RelPath) -> Vec<(usize, u64, u64)> {
    state
        .history
        .get(path)
        .map(|entries| {
            entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i, e.ts_ms, e.size))
                .collect()
        })
        .unwrap_or_default()
}

/// Fetches one history entry by index.
pub fn entry(state: &AppState, path: &RelPath, n: usize) -> Option<VersionEntry> {
    state.history.get(path).and_then(|e| e.get(n)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ManifestRef;
    use crate::state::encode_hex32;
    use crate::sync::index::sanitize_rel_path;
    use crate::sync::vclock::VClock;

    fn rec(size: u64, ts: u64) -> FileRecord {
        FileRecord {
            size,
            manifest: ManifestRef::empty(),
            vv: VClock::new(),
            deleted: false,
            updated_at_ms: ts,
        }
    }

    #[test]
    fn push_truncates_to_keep_and_lists_newest_first() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        let path = sanitize_rel_path("f").unwrap();
        for i in 0..(HISTORY_KEEP as u64 + 3) {
            push(&mut st, &path, &rec(i, 1000 + i));
        }
        let listed = list(&st, &path);
        assert_eq!(listed.len(), HISTORY_KEEP);
        // Newest replaced version first.
        assert_eq!(
            listed[0],
            (0, 1000 + HISTORY_KEEP as u64 + 2, HISTORY_KEEP as u64 + 2)
        );
        assert!(entry(&st, &path, HISTORY_KEEP).is_none());
        assert!(entry(&st, &path, 0).is_some());
    }

    #[test]
    fn tombstones_are_not_recorded() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        let path = sanitize_rel_path("f").unwrap();
        let mut r = rec(0, 1);
        r.deleted = true;
        push(&mut st, &path, &r);
        assert!(list(&st, &path).is_empty());
    }
}
