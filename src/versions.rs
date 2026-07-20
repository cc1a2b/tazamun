//! Version history over [`AppState`] (P14).
//!
//! Invariant: history is append-at-front; index 0 is always the most recently
//! replaced version of a path. Retention is the *effective depth*
//! ([`effective_depth`]) — the folder's `history-depth` config, or the role
//! default when that is `auto` — but a **pinned** entry is never pruned by
//! depth, and because pinned entries stay in `history` the GC-protect set
//! (which covers all of `history`) keeps their blobs alive for free. Tags and
//! pins are local to this node; they are metadata over the versions this node
//! happens to keep.

use crate::consts::{ARCHIVE_HISTORY_KEEP, HISTORY_KEEP};
use crate::proto::{ChunkRef, FileRecord};
use crate::state::{AppState, NodeRole, RelPath, SessionConfig, VersionEntry};

/// The number of unpinned versions this node keeps per path: the `history-depth`
/// config when set (> 0), otherwise the role default.
pub fn effective_depth(config: &SessionConfig) -> usize {
    if config.history_depth > 0 {
        config.history_depth
    } else {
        match config.role {
            NodeRole::Archive => ARCHIVE_HISTORY_KEEP,
            _ => HISTORY_KEEP,
        }
    }
}

/// Pushes the record being replaced onto the path's history, then prunes to the
/// effective depth while keeping every pinned entry. Tombstones carry no bytes
/// and are skipped.
pub fn push(state: &mut AppState, path: &RelPath, replaced: &FileRecord) {
    if replaced.deleted {
        return;
    }
    let depth = effective_depth(&state.config);
    let entries = state.history.entry(path.clone()).or_default();
    entries.insert(
        0,
        VersionEntry {
            manifest: replaced.manifest.clone(),
            vv: replaced.vv.clone(),
            ts_ms: replaced.updated_at_ms,
            size: replaced.size,
            tag: None,
            pinned: false,
        },
    );
    prune_to_depth(entries, depth);
}

/// Keeps the newest `depth` versions plus every pinned version, dropping the
/// rest. Pure so it is exhaustively testable.
pub fn prune_to_depth(entries: &mut Vec<VersionEntry>, depth: usize) {
    let mut idx = 0usize;
    entries.retain(|e| {
        let keep = idx < depth || e.pinned;
        idx += 1;
        keep
    });
}

/// Lists a path's history newest-first as `(index, timestamp_ms, size, tag,
/// pinned)`.
pub fn list(state: &AppState, path: &RelPath) -> Vec<(usize, u64, u64, Option<String>, bool)> {
    state
        .history
        .get(path)
        .map(|entries| {
            entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i, e.ts_ms, e.size, e.tag.clone(), e.pinned))
                .collect()
        })
        .unwrap_or_default()
}

/// Names version `n` of `path`. `None` clears the tag. Returns whether an entry
/// existed. Tags are unique per path (naming one clears the same name off any
/// other version) so `restore <path> <tag>` is unambiguous.
pub fn tag(state: &mut AppState, path: &RelPath, n: usize, name: Option<String>) -> bool {
    let Some(entries) = state.history.get_mut(path) else {
        return false;
    };
    if n >= entries.len() {
        return false;
    }
    if let Some(name) = &name {
        for e in entries.iter_mut() {
            if e.tag.as_deref() == Some(name.as_str()) {
                e.tag = None;
            }
        }
    }
    entries[n].tag = name;
    true
}

/// Pins or unpins version `n`. A pinned version survives depth pruning; on
/// unpin it becomes eligible again and is pruned on the next push if it now
/// sits past the depth. Returns whether the entry existed.
pub fn set_pinned(state: &mut AppState, path: &RelPath, n: usize, pinned: bool) -> bool {
    let Some(entries) = state.history.get_mut(path) else {
        return false;
    };
    let Some(entry) = entries.get_mut(n) else {
        return false;
    };
    entry.pinned = pinned;
    true
}

/// Fetches one history entry by index.
pub fn entry(state: &AppState, path: &RelPath, n: usize) -> Option<VersionEntry> {
    state.history.get(path).and_then(|e| e.get(n)).cloned()
}

/// Resolves a tag name to its version index for a path, if any.
pub fn index_of_tag(state: &AppState, path: &RelPath, name: &str) -> Option<usize> {
    state
        .history
        .get(path)?
        .iter()
        .position(|e| e.tag.as_deref() == Some(name))
}

/// Total bytes of kept history for a path (sum of version sizes). Blob dedup
/// means the on-disk cost is usually less, but this is the honest upper bound
/// on what history is holding onto.
pub fn disk_bytes(state: &AppState, path: &RelPath) -> u64 {
    state
        .history
        .get(path)
        .map(|e| e.iter().map(|v| v.size).sum())
        .unwrap_or(0)
}

/// Whole-session history footprint: `(paths_with_history, total_versions,
/// total_bytes)`.
pub fn footprint(state: &AppState) -> (usize, usize, u64) {
    let mut paths = 0;
    let mut versions = 0;
    let mut bytes = 0u64;
    for entries in state.history.values() {
        if entries.is_empty() {
            continue;
        }
        paths += 1;
        versions += entries.len();
        bytes += entries.iter().map(|e| e.size).sum::<u64>();
    }
    (paths, versions, bytes)
}

/// A content-defined-chunk comparison between two versions of a file — honest
/// about binaries (it compares chunk hashes, never pretends bytes are lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStat {
    pub old_chunks: usize,
    pub new_chunks: usize,
    /// New chunks whose content already exists in the old version (by hash).
    pub identical: usize,
    /// New chunks whose content is not in the old version — what a sync moves.
    pub added: usize,
    /// Old chunks whose content is gone in the new version.
    pub removed: usize,
    /// Chunks whose content is in both but at a different ordinal position.
    pub moved: usize,
    pub old_bytes: u64,
    pub new_bytes: u64,
    /// Unique bytes a receiver holding `old` would fetch to get `new`.
    pub transfer_bytes: u64,
}

impl DiffStat {
    /// Fraction of the new version's bytes that are new content (0.0–100.0).
    pub fn changed_pct(&self) -> f64 {
        if self.new_bytes == 0 {
            return if self.old_bytes == 0 { 0.0 } else { 100.0 };
        }
        (self.transfer_bytes as f64 / self.new_bytes as f64) * 100.0
    }

    pub fn identical_content(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.old_chunks == self.new_chunks
    }
}

/// Pure chunk-set diff of `old` → `new`. Deduplicates by hash for the
/// content questions (identical/added/removed/transfer) and compares ordinal
/// positions for `moved`.
pub fn diff_chunks(old: &[ChunkRef], new: &[ChunkRef]) -> DiffStat {
    use std::collections::HashSet;
    let old_hashes: HashSet<[u8; 32]> = old.iter().map(|c| c.hash).collect();
    let new_hashes: HashSet<[u8; 32]> = new.iter().map(|c| c.hash).collect();

    let mut identical = 0usize;
    let mut transfer_bytes = 0u64;
    // `identical` counts every new chunk position whose content already exists;
    // `transfer_bytes` counts each new-content chunk once (dedup by hash).
    let mut counted_added: HashSet<[u8; 32]> = HashSet::new();
    for c in new {
        if old_hashes.contains(&c.hash) {
            identical += 1;
        } else if counted_added.insert(c.hash) {
            transfer_bytes += u64::from(c.len);
        }
    }
    let added = new_hashes.difference(&old_hashes).count();
    let removed = old_hashes.difference(&new_hashes).count();
    // Moved: content in both, but the chunk at the same ordinal differs.
    let common = old.len().min(new.len());
    let same_pos = (0..common).filter(|&i| old[i].hash == new[i].hash).count();
    let both_content = new.iter().filter(|c| old_hashes.contains(&c.hash)).count();
    let moved = both_content.saturating_sub(same_pos);

    DiffStat {
        old_chunks: old.len(),
        new_chunks: new.len(),
        identical,
        added,
        removed,
        moved,
        old_bytes: old.iter().map(|c| u64::from(c.len)).sum(),
        new_bytes: new.iter().map(|c| u64::from(c.len)).sum(),
        transfer_bytes,
    }
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

    fn cref(hash: u8, len: u32) -> ChunkRef {
        ChunkRef {
            hash: [hash; 32],
            len,
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
        // Newest replaced version first: (index, ts, size, tag, pinned).
        assert_eq!(
            listed[0],
            (
                0,
                1000 + HISTORY_KEEP as u64 + 2,
                HISTORY_KEEP as u64 + 2,
                None,
                false
            )
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

    #[test]
    fn archive_role_keeps_deeper_history() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        st.config.role = NodeRole::Archive;
        let path = sanitize_rel_path("f").unwrap();
        for i in 0..(ARCHIVE_HISTORY_KEEP as u64 + 3) {
            push(&mut st, &path, &rec(i, 1000 + i));
        }
        assert_eq!(list(&st, &path).len(), ARCHIVE_HISTORY_KEEP);
        const { assert!(ARCHIVE_HISTORY_KEEP > HISTORY_KEEP) };
    }

    #[test]
    fn configured_depth_overrides_the_role_default() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        st.config.history_depth = 2;
        let path = sanitize_rel_path("f").unwrap();
        for i in 0..6 {
            push(&mut st, &path, &rec(i, 1000 + i));
        }
        assert_eq!(list(&st, &path).len(), 2);
        // 0 = auto → back to the role default.
        st.config.history_depth = 0;
        assert_eq!(effective_depth(&st.config), HISTORY_KEEP);
    }

    #[test]
    fn pinned_versions_survive_pruning() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        st.config.history_depth = 2;
        let path = sanitize_rel_path("f").unwrap();
        for i in 0..3 {
            push(&mut st, &path, &rec(i, 1000 + i));
        }
        // Pin the oldest kept entry (index 1 of the 2 kept), then push many more.
        assert!(set_pinned(&mut st, &path, 1, true));
        let pinned_ts = entry(&st, &path, 1).unwrap().ts_ms;
        for i in 3..10 {
            push(&mut st, &path, &rec(i, 1000 + i));
        }
        // Depth is 2, but the pinned entry is still present beyond it.
        let listed = list(&st, &path);
        assert!(
            listed.len() > 2,
            "pinned kept beyond depth: {}",
            listed.len()
        );
        assert!(
            listed
                .iter()
                .any(|(_, ts, _, _, pinned)| *ts == pinned_ts && *pinned),
            "the pinned version must survive"
        );
        // Unpin it and one more push drops it (now past depth).
        let idx = listed
            .iter()
            .find(|(_, ts, ..)| *ts == pinned_ts)
            .unwrap()
            .0;
        assert!(set_pinned(&mut st, &path, idx, false));
        push(&mut st, &path, &rec(99, 9999));
        assert!(!list(&st, &path).iter().any(|(_, ts, ..)| *ts == pinned_ts));
    }

    #[test]
    fn tags_are_unique_per_path_and_resolvable() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        let path = sanitize_rel_path("f").unwrap();
        for i in 0..3 {
            push(&mut st, &path, &rec(i, 1000 + i));
        }
        assert!(tag(&mut st, &path, 2, Some("approved".to_string())));
        assert_eq!(index_of_tag(&st, &path, "approved"), Some(2));
        // Re-tagging a different version moves the name (unique per path).
        assert!(tag(&mut st, &path, 0, Some("approved".to_string())));
        assert_eq!(index_of_tag(&st, &path, "approved"), Some(0));
        assert!(entry(&st, &path, 2).unwrap().tag.is_none());
        // Out-of-range n is a clean false, never a panic.
        assert!(!tag(&mut st, &path, 99, Some("x".to_string())));
    }

    #[test]
    fn diff_is_binary_honest_chunk_math() {
        // old: [A,B,C]  new: [A,C,D]  → B removed, D added, A/C identical, C moved.
        let old = vec![cref(1, 100), cref(2, 100), cref(3, 100)];
        let new = vec![cref(1, 100), cref(3, 100), cref(4, 100)];
        let d = diff_chunks(&old, &new);
        assert_eq!(d.old_chunks, 3);
        assert_eq!(d.new_chunks, 3);
        assert_eq!(d.identical, 2, "A and C carry over");
        assert_eq!(d.added, 1, "D is new");
        assert_eq!(d.removed, 1, "B is gone");
        assert_eq!(d.transfer_bytes, 100, "only D's bytes move");
        assert!((d.changed_pct() - (100.0 / 300.0 * 100.0)).abs() < 0.001);
        assert!(d.moved >= 1, "C shifted position");
        // Identical files report no change.
        let same = diff_chunks(&old, &old);
        assert!(same.identical_content());
        assert_eq!(same.transfer_bytes, 0);
        assert_eq!(same.changed_pct(), 0.0);
    }

    #[test]
    fn footprint_sums_versions_and_bytes() {
        let mut st = AppState::new(encode_hex32(&[0; 32]), encode_hex32(&[0; 32]));
        let a = sanitize_rel_path("a").unwrap();
        push(&mut st, &a, &rec(10, 1));
        push(&mut st, &a, &rec(20, 2));
        let (paths, versions, bytes) = footprint(&st);
        assert_eq!((paths, versions, bytes), (1, 2, 30));
        assert_eq!(disk_bytes(&st, &a), 30);
    }
}
