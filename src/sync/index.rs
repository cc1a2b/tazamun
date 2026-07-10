//! Remote index handling: path sanitization and reconciliation.
//!
//! Invariant: this module performs zero I/O. `sanitize_rel_path` is the only
//! construction path for [`RelPath`] from untrusted input, and it runs on
//! every remote-supplied path; a failing path drops its whole record.

use std::collections::BTreeMap;

use crate::consts::{MAX_PATH_LEN, META_DIR};
use crate::proto::FileRecord;
use crate::state::RelPath;
use crate::sync::vclock::{self, Causality};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("empty path")]
    Empty,
    #[error("path longer than {MAX_PATH_LEN} bytes")]
    TooLong,
    #[error("NUL byte in path")]
    Nul,
    #[error("backslash in path")]
    Backslash,
    #[error("absolute path")]
    Absolute,
    #[error("drive letter path")]
    DriveLetter,
    #[error("`.`, `..` or empty segment")]
    BadSegment,
    #[error("reserved `{META_DIR}` component")]
    Reserved,
}

/// Validates an untrusted relative path and returns it as a [`RelPath`].
///
/// Rejects: absolute paths, drive letters, any backslash, NUL bytes, the empty
/// string, oversized paths, `.` / `..` / empty segments, and any `.tazamun`
/// component (which would alias tazamun's own metadata).
pub fn sanitize_rel_path(input: &str) -> Result<RelPath, PathError> {
    if input.is_empty() {
        return Err(PathError::Empty);
    }
    if input.len() > MAX_PATH_LEN {
        return Err(PathError::TooLong);
    }
    if input.contains('\0') {
        return Err(PathError::Nul);
    }
    let bytes = input.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(PathError::DriveLetter);
    }
    if input.contains('\\') {
        return Err(PathError::Backslash);
    }
    if input.starts_with('/') {
        return Err(PathError::Absolute);
    }
    for seg in input.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(PathError::BadSegment);
        }
        if seg.eq_ignore_ascii_case(META_DIR) {
            return Err(PathError::Reserved);
        }
    }
    Ok(RelPath::new_unchecked(input.to_string()))
}

/// Windows reserved device names — reserved with or without an extension
/// (`aux`, `aux.txt`, `COM1.log` all name the device).
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Checks whether a (sanitizer-accepted) relative path is representable on a
/// Windows/NTFS node. Pure and platform-independent: the *decision* about a
/// violation (refuse to materialize on Windows, warn-only on Unix) belongs to
/// the caller. Returns a human-readable reason on violation.
///
/// Rules: no `< > : " | ? *` or control characters (0x00–0x1F) in any segment;
/// no reserved device names (`CON PRN AUX NUL COM1–9 LPT1–9`, case-insensitive,
/// with or without extension); no segment ending in a dot or a space. The
/// case-fold-collision rule (NTFS is case-insensitive) is stateful and checked
/// against the live index at apply time, not here.
pub fn portability_violation(rel: &str) -> Option<String> {
    for seg in rel.split('/') {
        for ch in seg.chars() {
            if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (ch as u32) < 0x20 {
                return Some(format!(
                    "segment {seg:?} contains {ch:?}, invalid in Windows file names"
                ));
            }
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return Some(format!(
                "segment {seg:?} ends with a dot or space, which Windows strips"
            ));
        }
        let stem = seg.split('.').next().unwrap_or(seg);
        if WINDOWS_RESERVED
            .iter()
            .any(|r| stem.eq_ignore_ascii_case(r))
        {
            return Some(format!(
                "segment {seg:?} is a reserved Windows device name ({})",
                stem.to_ascii_uppercase()
            ));
        }
    }
    None
}

/// Result of reconciling a remote index against the local one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Diff {
    /// Paths whose remote record should be pulled.
    pub pull: Vec<RelPath>,
    /// Paths whose clocks are concurrent — impossible under strict locking,
    /// so they signal external tampering and are quarantined by the caller.
    pub conflicts: Vec<RelPath>,
}

/// Compares a remote peer's advertised records against local state.
///
/// Pull when the path is locally missing (and the remote record is not a
/// tombstone) or when the local clock is strictly `Before` the remote one.
/// Tombstones participate causally exactly like content records.
pub fn diff(local: &BTreeMap<RelPath, FileRecord>, remote: &[(RelPath, FileRecord)]) -> Diff {
    let mut out = Diff::default();
    for (path, rec) in remote {
        match local.get(path) {
            None => {
                if !rec.deleted {
                    out.pull.push(path.clone());
                }
            }
            Some(mine) => match vclock::compare(&mine.vv, &rec.vv) {
                Causality::Before => out.pull.push(path.clone()),
                Causality::Concurrent => out.conflicts.push(path.clone()),
                Causality::Equal | Causality::After => {}
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ManifestRef;
    use crate::sync::vclock::VClock;

    #[test]
    fn sanitizer_accepts_normal_paths() {
        for ok in [
            "a.txt",
            "dir/sub/file.bin",
            "عربى/ملف.txt",
            "with space/x-y_z.1",
        ] {
            assert!(sanitize_rel_path(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn sanitizer_corpus_rejections() {
        let cases: &[(&str, PathError)] = &[
            ("../x", PathError::BadSegment),
            ("a/../b", PathError::BadSegment),
            ("./a", PathError::BadSegment),
            ("/etc/passwd", PathError::Absolute),
            ("C:\\x", PathError::DriveLetter),
            ("c:x", PathError::DriveLetter),
            ("a\\b", PathError::Backslash),
            ("a\0b", PathError::Nul),
            ("", PathError::Empty),
            ("a//b", PathError::BadSegment),
            ("a/", PathError::BadSegment),
            (".tazamun/state.json", PathError::Reserved),
            ("x/.TAZAMUN/y", PathError::Reserved),
        ];
        for (input, want) in cases {
            assert_eq!(sanitize_rel_path(input).unwrap_err(), *want, "{input:?}");
        }
        let long = "a/".repeat(2500) + "b";
        assert!(long.len() > MAX_PATH_LEN);
        assert_eq!(sanitize_rel_path(&long).unwrap_err(), PathError::TooLong);
    }

    fn rec(vv: VClock, deleted: bool) -> FileRecord {
        FileRecord {
            size: 0,
            manifest: ManifestRef::empty(),
            vv,
            deleted,
            updated_at_ms: 0,
        }
    }

    fn p(s: &str) -> RelPath {
        sanitize_rel_path(s).unwrap()
    }

    #[test]
    fn diff_pull_missing_and_stale() {
        let mut local = BTreeMap::new();
        local.insert(p("stale"), rec(VClock::from([("a".into(), 1)]), false));
        local.insert(p("same"), rec(VClock::from([("a".into(), 2)]), false));
        let remote = vec![
            (p("new"), rec(VClock::from([("b".into(), 1)]), false)),
            (p("stale"), rec(VClock::from([("a".into(), 3)]), false)),
            (p("same"), rec(VClock::from([("a".into(), 2)]), false)),
            (p("gone"), rec(VClock::from([("b".into(), 1)]), true)),
        ];
        let d = diff(&local, &remote);
        assert_eq!(d.pull, vec![p("new"), p("stale")]);
        assert!(d.conflicts.is_empty());
    }

    #[test]
    fn diff_tombstone_pulls_when_causally_newer() {
        let mut local = BTreeMap::new();
        local.insert(p("f"), rec(VClock::from([("a".into(), 1)]), false));
        let remote = vec![(
            p("f"),
            rec(VClock::from([("a".into(), 1), ("b".into(), 1)]), true),
        )];
        let d = diff(&local, &remote);
        assert_eq!(d.pull, vec![p("f")]);
    }

    #[test]
    fn diff_concurrent_is_conflict() {
        let mut local = BTreeMap::new();
        local.insert(p("f"), rec(VClock::from([("a".into(), 2)]), false));
        let remote = vec![(p("f"), rec(VClock::from([("b".into(), 2)]), false))];
        let d = diff(&local, &remote);
        assert!(d.pull.is_empty());
        assert_eq!(d.conflicts, vec![p("f")]);
    }

    #[test]
    fn portability_corpus() {
        // Clean, portable names pass.
        for ok in [
            "notes.txt",
            "a/b/c.bin",
            "COMMON.md",         // COM prefix but not a device name
            "aux2.txt",          // AUX2 is not reserved
            "consommé.txt",      // unicode fine
            "nulled/console.rs", // device names as *directory content* words
            "x.con",             // reserved stem rule applies to the stem only
        ] {
            assert_eq!(portability_violation(ok), None, "{ok} should be portable");
        }
        // Invalid characters.
        for bad in [
            "da:ta.bin",
            "que?stion.txt",
            "st*ar",
            "pi|pe",
            "quo\"te",
            "lt<.txt",
            "gt>.txt",
            "ctl\u{1}.bin",
        ] {
            assert!(
                portability_violation(bad).is_some(),
                "{bad} must be flagged"
            );
        }
        // Reserved device names, any case, with or without extension, in any
        // segment position.
        for bad in [
            "aux",
            "AUX.txt",
            "Con",
            "com1",
            "COM9.log",
            "lpt3.bak",
            "NUL",
            "prn.csv",
            "sub/aux.txt",
            "CoM2.tar.gz",
        ] {
            assert!(
                portability_violation(bad).is_some(),
                "{bad} must be flagged as reserved"
            );
        }
        // Trailing dot / space.
        for bad in ["file.", "dir./x", "name ", "sub/trail. /y"] {
            assert!(
                portability_violation(bad).is_some(),
                "{bad:?} must be flagged for trailing dot/space"
            );
        }
    }
}
