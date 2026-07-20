//! P18 conflict center: structured access to the quarantine.
//!
//! The quarantine directory (`.tazamun/conflicts/`) holds the preserved bytes
//! and is the *truth*; the sidecar index (`.tazamun/conflicts-index.jsonl`,
//! one JSON line per quarantine event) is advisory metadata adding the reason
//! and the original relative path. [`list`] joins the two: a copy missing from
//! the index (legacy, or a lost index) still lists, with the path recovered
//! from its percent-encoded filename when possible and the reason `unknown`.
//!
//! Invariant (Golden): nothing here deletes user bytes implicitly. The only
//! deleting functions are [`discard`] (one copy, an explicit resolution) and
//! [`prune`] (an explicit, interactively confirmed hygiene sweep) — both are
//! called solely from paths where the user asked for exactly that.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One quarantined copy, as shown by `tazamun conflicts list` / the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEntry {
    /// Quarantine file name (`<UTC-ts>__<encoded-rel>`) — the stable id.
    pub name: String,
    /// Original relative path, when known (index, or decodable filename).
    pub path: Option<String>,
    /// Why it was quarantined (`forced-write`, `offline-edit`,
    /// `concurrent-versions`, `autolock`, `new-file`, `edit-vs-remote`) or
    /// `None` for legacy entries recorded before reasons existed.
    pub reason: Option<String>,
    /// Epoch-ms the copy was made (index) or the file mtime (fallback).
    pub ts_ms: u64,
    pub size: u64,
}

/// One line of the sidecar index.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexLine {
    name: String,
    path: String,
    reason: String,
    ts_ms: u64,
    size: u64,
}

/// Appends one quarantine event to the sidecar index. Best-effort by design:
/// the copy in `conflicts/` is already safe on disk when this runs, and a
/// failed index write must never fail the quarantine itself.
pub fn append_index(dir: &Path, name: &str, rel: &str, reason: &str, size: u64) {
    let line = IndexLine {
        name: name.to_string(),
        path: rel.to_string(),
        reason: reason.to_string(),
        ts_ms: crate::now_ms(),
        size,
    };
    let Ok(json) = serde_json::to_string(&line) else {
        return;
    };
    let path = crate::state::conflicts_index_path(dir);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{json}"));
}

/// Reads the sidecar index (missing/corrupt lines are skipped, never fatal).
fn read_index(dir: &Path) -> Vec<IndexLine> {
    let Ok(text) = std::fs::read_to_string(crate::state::conflicts_index_path(dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Decodes a `guard::percent_encode`d string back to the original, or `None`
/// when malformed or when the name was hash-truncated (contains no full path).
pub fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hi = (hex[0] as char).to_digit(16)?;
                let lo = (hex[1] as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Splits a quarantine file name `<ts>__<encoded>` and recovers the original
/// relative path when the encoded part is complete (not `---`-truncated).
fn path_from_name(name: &str) -> Option<String> {
    let (_, encoded) = name.split_once("__")?;
    if encoded.contains("---") {
        return None; // hash-truncated: the full path only lives in the index
    }
    percent_decode(encoded)
}

/// Lists every quarantined copy, newest first, joining the dir (existence,
/// size, mtime) with the index (reason, original path). Works offline.
pub fn list(dir: &Path) -> Vec<ConflictEntry> {
    let index: std::collections::HashMap<String, IndexLine> = read_index(dir)
        .into_iter()
        .map(|l| (l.name.clone(), l))
        .collect();
    let cdir = crate::state::conflicts_dir(dir);
    let Ok(entries) = std::fs::read_dir(&cdir) else {
        return Vec::new();
    };
    let mut out: Vec<ConflictEntry> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = e.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime_ms = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            match index.get(&name) {
                Some(ix) => ConflictEntry {
                    name,
                    path: Some(ix.path.clone()),
                    reason: Some(ix.reason.clone()),
                    ts_ms: ix.ts_ms,
                    size,
                },
                None => ConflictEntry {
                    path: path_from_name(&name),
                    reason: None,
                    ts_ms: mtime_ms,
                    size,
                    name,
                },
            }
        })
        .collect();
    out.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then_with(|| b.name.cmp(&a.name)));
    out
}

/// Whether `id` is safe to join onto the conflicts dir: a plain file name with
/// no separators, no drive/ADS colon, no traversal, no leading dot. The caller
/// ([`copy_path`]) additionally asserts the joined path did not escape.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('.')
        // `:` would let `C:foo` become a Windows drive-relative path that
        // `Path::join` resolves outside the conflicts dir.
        && !id.contains(['/', '\\', ':'])
        && !id.contains("..")
        && !id.contains('\0')
}

/// Resolves a user-supplied id (a full quarantine file name or a unique
/// prefix — names start with the timestamp, so prefixes are natural) against
/// the live listing.
pub fn resolve_id<'a>(entries: &'a [ConflictEntry], id: &str) -> Result<&'a ConflictEntry, String> {
    if !valid_id(id) {
        return Err(format!("invalid conflict id {id:?}"));
    }
    let matches: Vec<&ConflictEntry> = entries
        .iter()
        .filter(|e| e.name == id || e.name.starts_with(id))
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(format!("no quarantined copy matches {id:?}")),
        n => Err(format!(
            "{id:?} is ambiguous ({n} copies match — use more characters)"
        )),
    }
}

/// The path of one quarantined copy, containment-checked: `name` must be a
/// valid id, the join must not escape the conflicts dir (belt-and-suspenders
/// against a platform where `join` could reparent — e.g. a Windows drive
/// prefix), AND it must be an existing plain file there.
pub fn copy_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_id(name) {
        return Err(format!("invalid conflict id {name:?}"));
    }
    let cdir = crate::state::conflicts_dir(dir);
    let p = cdir.join(name);
    // The joined path's parent must be exactly the conflicts dir; any escape
    // (drive-relative, absolute) reparents it and is rejected here.
    if p.parent() != Some(cdir.as_path()) {
        return Err(format!("conflict id {name:?} escapes the quarantine dir"));
    }
    if !p.is_file() {
        return Err(format!("no quarantined copy named {name:?}"));
    }
    Ok(p)
}

/// Deletes ONE quarantined copy (the explicit `keep theirs` / post-resolve
/// step) and drops its index line. Returns the freed byte count.
pub fn discard(dir: &Path, name: &str) -> Result<u64, String> {
    let p = copy_path(dir, name)?;
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    std::fs::remove_file(&p).map_err(|e| format!("could not delete {name}: {e}"))?;
    rewrite_index_without(dir, &[name.to_string()]);
    Ok(size)
}

/// Pure cutoff selection for `conflicts prune --older-than`: entries whose
/// timestamp is strictly older than `now_ms - older_than_ms`.
pub fn select_prunable(
    entries: &[ConflictEntry],
    now_ms: u64,
    older_than_ms: u64,
) -> Vec<ConflictEntry> {
    let cutoff = now_ms.saturating_sub(older_than_ms);
    entries
        .iter()
        .filter(|e| e.ts_ms < cutoff)
        .cloned()
        .collect()
}

/// Deletes the given quarantined copies (the explicit, confirmed prune) and
/// rewrites the index without them. Returns (deleted names, freed bytes);
/// entries that fail to delete are reported, not fatal.
pub fn prune(dir: &Path, names: &[String]) -> (Vec<String>, u64, Vec<String>) {
    let mut removed = Vec::new();
    let mut errors = Vec::new();
    let mut bytes = 0u64;
    for name in names {
        match copy_path(dir, name) {
            Ok(p) => {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                match std::fs::remove_file(&p) {
                    Ok(()) => {
                        bytes += size;
                        removed.push(name.clone());
                    }
                    Err(e) => errors.push(format!("{name}: {e}")),
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if !removed.is_empty() {
        rewrite_index_without(dir, &removed);
    }
    (removed, bytes, errors)
}

/// Rewrites the sidecar index dropping the given names (atomic tmp + rename;
/// best-effort — the index is advisory). The tmp name is unique per call so two
/// concurrent rewrites (CLI prune vs daemon discard) never collide on it.
fn rewrite_index_without(dir: &Path, names: &[String]) {
    let path = crate::state::conflicts_index_path(dir);
    let kept: Vec<String> = read_index(dir)
        .into_iter()
        .filter(|l| !names.contains(&l.name))
        .filter_map(|l| serde_json::to_string(&l).ok())
        .collect();
    let tmp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    let body = if kept.is_empty() {
        String::new()
    } else {
        kept.join("\n") + "\n"
    };
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// A free working-tree name for `keep both`: the original path with
/// `.conflict-<compact-ts>` inserted before the extension, suffixed `-2`,
/// `-3`, … until `taken` says it is free. Bounded so a pathological `taken`
/// (or an un-nameable path) can never loop forever — it returns the base after
/// the cap, and the downstream lease/sanitize gate rejects an unusable name
/// cleanly instead of hanging the caller.
pub fn both_name(rel: &str, ts_ms: u64, taken: impl Fn(&str) -> bool) -> String {
    let ts = crate::guard::utc_timestamp(ts_ms);
    let ts = ts.trim_end_matches('Z');
    let (stem, ext) = match rel.rsplit_once('.') {
        // Only treat it as an extension when the dot is inside the last
        // component (not "dir.v2/file") and not a leading dot-file.
        Some((s, e)) if !e.contains('/') && !s.is_empty() && !s.ends_with('/') && !e.is_empty() => {
            (s.to_string(), format!(".{e}"))
        }
        _ => (rel.to_string(), String::new()),
    };
    let base = format!("{stem}.conflict-{ts}{ext}");
    if !taken(&base) {
        return base;
    }
    for n in 2..=10_000 {
        let candidate = format!("{stem}.conflict-{ts}-{n}{ext}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_inverts_encode() {
        for s in ["a/b c.txt", "عربي/ملف.bin", "plain.txt", "x%y"] {
            let enc = crate::guard::percent_encode(s);
            assert_eq!(percent_decode(&enc).as_deref(), Some(s), "{s}");
        }
        assert_eq!(percent_decode("%zz"), None, "bad hex rejected");
        assert_eq!(percent_decode("%2"), None, "truncated escape rejected");
    }

    #[test]
    fn path_recovery_from_filename() {
        assert_eq!(
            path_from_name("20260714T010203004Z__docs%2Fplan.md").as_deref(),
            Some("docs/plan.md")
        );
        // Hash-truncated names carry no recoverable path.
        assert_eq!(path_from_name("20260714T010203004Z__abc---deadbeef"), None);
        assert_eq!(path_from_name("no-separator"), None);
    }

    #[test]
    fn id_validation_rejects_traversal() {
        assert!(valid_id("20260714T010203004Z__a.txt"));
        // Traversal, separators, the Windows drive/ADS colon, NUL, leading dot.
        for bad in [
            "", "../x", "a/b", "a\\b", ".hidden", "a..b", "C:foo", "a:b", "a\0b",
        ] {
            assert!(!valid_id(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn copy_path_contained_and_requires_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cdir = crate::state::conflicts_dir(dir.path());
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::write(cdir.join("20260714T010000000Z__a.txt"), b"x").unwrap();
        assert!(copy_path(dir.path(), "20260714T010000000Z__a.txt").is_ok());
        // Escapes and non-existent ids are refused, never panic.
        assert!(copy_path(dir.path(), "../../etc/passwd").is_err());
        assert!(copy_path(dir.path(), "C:evil").is_err());
        assert!(copy_path(dir.path(), "does-not-exist").is_err());
    }

    #[test]
    fn both_name_terminates_when_everything_is_taken() {
        // A pathological `taken` (always true) must not loop forever (S2).
        let n = both_name("a.txt", 0, |_| true);
        assert!(n.starts_with("a.conflict-") && n.ends_with(".txt"), "{n}");
    }

    #[test]
    fn resolve_id_prefix_and_ambiguity() {
        let mk = |name: &str| ConflictEntry {
            name: name.into(),
            path: None,
            reason: None,
            ts_ms: 0,
            size: 0,
        };
        let entries = vec![mk("20260714T010000000Z__a"), mk("20260714T020000000Z__b")];
        assert_eq!(
            resolve_id(&entries, "20260714T02").unwrap().name,
            "20260714T020000000Z__b"
        );
        assert!(resolve_id(&entries, "20260714T").is_err(), "ambiguous");
        assert!(resolve_id(&entries, "nope").is_err(), "no match");
        assert!(resolve_id(&entries, "../x").is_err(), "invalid id");
    }

    #[test]
    fn prune_selection_is_a_strict_age_cutoff() {
        let mk = |name: &str, ts: u64| ConflictEntry {
            name: name.into(),
            path: None,
            reason: None,
            ts_ms: ts,
            size: 1,
        };
        let entries = vec![mk("old", 1_000), mk("edge", 5_000), mk("new", 9_000)];
        // now=10_000, older-than=5_000 → cutoff 5_000; strictly-older only.
        let sel = select_prunable(&entries, 10_000, 5_000);
        let names: Vec<&str> = sel.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["old"], "edge (== cutoff) is kept");
    }

    #[test]
    fn both_name_inserts_before_extension_and_avoids_collisions() {
        let free = |_: &str| false;
        let n = both_name("assets/logo.png", 0, free);
        assert!(
            n.starts_with("assets/logo.conflict-") && n.ends_with(".png"),
            "{n}"
        );
        // No extension → suffix at the end.
        let n = both_name("Makefile", 0, free);
        assert!(n.starts_with("Makefile.conflict-"), "{n}");
        // Dot in a directory, not the file → not an extension.
        let n = both_name("dir.v2/file", 0, free);
        assert!(n.starts_with("dir.v2/file.conflict-"), "{n}");
        // Collision → -2.
        let base = both_name("a.txt", 0, free);
        let n = both_name("a.txt", 0, |c: &str| c == base);
        assert!(n.ends_with("-2.txt"), "{n}");
    }

    #[test]
    fn index_and_dir_join_with_legacy_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let cdir = crate::state::conflicts_dir(dir.path());
        std::fs::create_dir_all(&cdir).unwrap();
        // An indexed entry and a legacy (index-less) entry.
        std::fs::write(cdir.join("20260714T010000000Z__docs%2Fa.md"), b"12345").unwrap();
        std::fs::write(cdir.join("20260714T020000000Z__b.txt"), b"xyz").unwrap();
        append_index(
            dir.path(),
            "20260714T010000000Z__docs%2Fa.md",
            "docs/a.md",
            "forced-write",
            5,
        );
        let l = list(dir.path());
        assert_eq!(l.len(), 2);
        let a = l.iter().find(|e| e.name.ends_with("a.md")).unwrap();
        assert_eq!(a.path.as_deref(), Some("docs/a.md"));
        assert_eq!(a.reason.as_deref(), Some("forced-write"));
        assert_eq!(a.size, 5);
        let b = l.iter().find(|e| e.name.ends_with("b.txt")).unwrap();
        assert_eq!(b.path.as_deref(), Some("b.txt"), "decoded from filename");
        assert_eq!(b.reason, None, "legacy entries have no recorded reason");

        // Discard removes the copy and its index line.
        let freed = discard(dir.path(), "20260714T010000000Z__docs%2Fa.md").unwrap();
        assert_eq!(freed, 5);
        let l = list(dir.path());
        assert_eq!(l.len(), 1);
        assert!(discard(dir.path(), "20260714T010000000Z__docs%2Fa.md").is_err());

        // Prune deletes the selected names and reports freed bytes.
        let (removed, bytes, errors) =
            prune(dir.path(), &["20260714T020000000Z__b.txt".to_string()]);
        assert_eq!(removed.len(), 1);
        assert_eq!(bytes, 3);
        assert!(errors.is_empty());
        assert!(list(dir.path()).is_empty());
    }
}
