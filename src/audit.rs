//! P19 append-only audit log: one JSON line per meaningful event, per folder.
//!
//! `.tazamun/audit.jsonl` records who did what, when, and (for wire-driven
//! events) from which peer — lock/unlock/publish/restore, remote applies,
//! quarantines, lock denials, peer connect/disconnect. It is line-capped
//! exactly like the daemon log (reusing [`crate::service::LineCappedLog`]), so
//! it self-bounds without external rotation. It lives inside `.tazamun`, which
//! is excluded from the watcher and the sync index, so it never syncs or
//! self-triggers events. `tazamun log` reads it back with filters; nothing
//! here ever blocks the sync path (a small append on the actor, like the
//! existing `state.json` persist).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::service::LineCappedLog;

/// One audit record. `kind` is a stable slug; the rest are optional context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub ts_ms: u64,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The audit-log path for a session folder.
pub fn audit_path(dir: &Path) -> PathBuf {
    crate::state::AppState::meta_dir(dir).join("audit.jsonl")
}

/// The append side, held by the daemon actor. Cheap to write to.
pub struct AuditLog {
    sink: LineCappedLog,
}

impl AuditLog {
    /// Opens (creating the file/parents) the capped audit log for `dir`.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        let sink = LineCappedLog::open_at(audit_path(dir), crate::consts::AUDIT_MAX_LINES)?;
        Ok(Self { sink })
    }

    /// Appends one event (best-effort — a failed audit write is logged and
    /// dropped, never allowed to disturb the sync path).
    pub fn emit(
        &mut self,
        kind: &str,
        path: Option<&str>,
        peer: Option<&str>,
        detail: Option<String>,
    ) {
        let ev = AuditEvent {
            ts_ms: crate::now_ms(),
            kind: kind.to_string(),
            path: path.map(str::to_string),
            peer: peer.map(str::to_string),
            detail,
        };
        if let Ok(line) = serde_json::to_string(&ev) {
            let _ = writeln!(self.sink, "{line}");
            let _ = self.sink.flush();
        }
    }
}

/// Filters for [`read`].
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// Only events whose `path` equals this (exact match).
    pub path: Option<String>,
    /// Only events whose `peer` starts with this (id prefix).
    pub peer: Option<String>,
    /// Only events at or after this epoch-ms.
    pub since_ms: Option<u64>,
    /// Only these kinds (empty = all).
    pub kinds: Vec<String>,
}

impl Filter {
    fn matches(&self, e: &AuditEvent) -> bool {
        if let Some(p) = &self.path
            && e.path.as_deref() != Some(p.as_str())
        {
            return false;
        }
        if let Some(peer) = &self.peer
            && !e.peer.as_deref().is_some_and(|x| x.starts_with(peer))
        {
            return false;
        }
        if let Some(since) = self.since_ms
            && e.ts_ms < since
        {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| k == &e.kind) {
            return false;
        }
        true
    }
}

/// Reads and filters the audit log (oldest first, as written). Missing/corrupt
/// lines are skipped — the log is advisory and never fatal to read.
pub fn read(dir: &Path, filter: &Filter) -> Vec<AuditEvent> {
    let Ok(bytes) = std::fs::read(audit_path(dir)) else {
        return Vec::new();
    };
    // Lossy decode: one torn byte (a crash mid-append) must not hide the whole
    // trail — that line just fails to parse and is skipped.
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| serde_json::from_str::<AuditEvent>(l).ok())
        .filter(|e| filter.matches(e))
        .collect()
}

/// Byte offset of the end of the file, so `--follow` can resume from where it
/// last read without re-scanning (0 when the file is absent).
pub fn end_offset(dir: &Path) -> u64 {
    std::fs::metadata(audit_path(dir))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Reads audit lines appended after byte `from`, returning the new events and
/// the new end offset. Used by `tazamun log --follow`. If the file shrank
/// (rotation/trim), it restarts from 0.
pub fn read_since_offset(dir: &Path, from: u64, filter: &Filter) -> (Vec<AuditEvent>, u64) {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = audit_path(dir);
    let Ok(mut f) = std::fs::File::open(&path) else {
        return (Vec::new(), 0);
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    // If the file shrank (a trim/rotation dropped old lines), resume at the new
    // end rather than 0 — re-emitting the whole retained tail would flood a
    // live `--follow`. A rotation may thus skip a few just-written lines from
    // the follow view; they remain in the file for a non-follow read.
    let start = if from > len { len } else { from };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return (Vec::new(), len);
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return (Vec::new(), len);
    }
    // A trailing partial line (mid-append) is left for the next poll: only
    // parse complete lines, and advance the offset by the bytes we consumed.
    let mut consumed = start;
    let mut out = Vec::new();
    for line in buf.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break; // incomplete final line
        }
        consumed += line.len() as u64;
        if let Ok(e) = serde_json::from_str::<AuditEvent>(line.trim_end())
            && filter.matches(&e)
        {
            out.push(e);
        }
    }
    (out, consumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_read_and_filter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::state::AppState::meta_dir(dir.path())).unwrap();
        let mut log = AuditLog::open(dir.path()).unwrap();
        log.emit("lock", Some("a.txt"), None, None);
        log.emit("publish", Some("a.txt"), Some("9f2c4a7e10"), None);
        log.emit(
            "lock",
            Some("b.txt"),
            Some("deadbeef00"),
            Some("held 3s".into()),
        );

        // No filter → all three, in write order.
        let all = read(dir.path(), &Filter::default());
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].kind, "lock");
        assert_eq!(all[0].path.as_deref(), Some("a.txt"));

        // Filter by path.
        let f = Filter {
            path: Some("a.txt".into()),
            ..Default::default()
        };
        assert_eq!(read(dir.path(), &f).len(), 2);

        // Filter by peer prefix.
        let f = Filter {
            peer: Some("9f2c".into()),
            ..Default::default()
        };
        let r = read(dir.path(), &f);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, "publish");

        // Filter by kind.
        let f = Filter {
            kinds: vec!["lock".into()],
            ..Default::default()
        };
        assert_eq!(read(dir.path(), &f).len(), 2);
    }

    #[test]
    fn since_offset_returns_only_new_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::state::AppState::meta_dir(dir.path())).unwrap();
        let mut log = AuditLog::open(dir.path()).unwrap();
        log.emit("lock", Some("a.txt"), None, None);
        let off = end_offset(dir.path());
        log.emit("unlock", Some("a.txt"), None, None);

        let (new, off2) = read_since_offset(dir.path(), off, &Filter::default());
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].kind, "unlock");
        assert!(off2 >= off);
        // Nothing new since off2.
        let (new2, _) = read_since_offset(dir.path(), off2, &Filter::default());
        assert!(new2.is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::state::AppState::meta_dir(dir.path())).unwrap();
        std::fs::write(
            audit_path(dir.path()),
            b"{ not json\n{\"ts_ms\":1,\"kind\":\"lock\"}\ngarbage\n",
        )
        .unwrap();
        let all = read(dir.path(), &Filter::default());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "lock");
    }
}
