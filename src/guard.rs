//! Filesystem guard rails: read-only enforcement and quarantine.
//!
//! Invariant: quarantine never deletes the offending bytes — it copies them
//! into `.tazamun/conflicts/` first, so the golden invariant ("never silently
//! delete user bytes") holds even on the violation path. Read-only bits are a
//! guard rail against accidental saves, not a security boundary; the daemon's
//! violation flow catches force-writes.

use std::path::{Path, PathBuf};

use crate::state::{AppState, RelPath, conflicts_dir};

#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("guard io on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn io_err(path: &Path, source: std::io::Error) -> GuardError {
    GuardError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Marks a file read-only (mode 0444 on Unix, the readonly attribute on
/// Windows). Missing files are ignored.
pub fn set_readonly(path: &Path) -> Result<(), GuardError> {
    apply_mode(path, true)
}

/// Makes a file writable again (mode 0644 on Unix).
pub fn set_writable(path: &Path) -> Result<(), GuardError> {
    apply_mode(path, false)
}

fn apply_mode(path: &Path, readonly: bool) -> Result<(), GuardError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(path, e)),
    };
    if !meta.is_file() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if readonly { 0o444 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| io_err(path, e))?;
    }
    #[cfg(not(unix))]
    {
        // Windows: scanners/editors race attribute changes (error 5/32), so
        // the set goes through the bounded retry.
        let mut perms = meta.permissions();
        perms.set_readonly(readonly);
        crate::win_fs::with_retry("set_attributes", path, || {
            std::fs::set_permissions(path, perms.clone())
        })
        .map_err(|e| io_err(path, e))?;
    }
    Ok(())
}

/// Copies the current bytes of `rel` into
/// `.tazamun/conflicts/<UTC-timestamp>__<name>` and returns the quarantine
/// path. The original file is left untouched. `reason` is a short stable slug
/// (`forced-write`, `offline-edit`, `concurrent-versions`, `autolock`,
/// `new-file`, `edit-vs-remote`) recorded — after the copy is safe — in the
/// advisory conflicts index so `tazamun conflicts` can say *why*; an index
/// write failure never fails the quarantine itself.
pub fn quarantine(dir: &Path, rel: &RelPath, reason: &str) -> Result<PathBuf, GuardError> {
    let src = rel.to_fs_path(dir);
    let cdir = conflicts_dir(dir);
    std::fs::create_dir_all(&cdir).map_err(|e| io_err(&cdir, e))?;
    let name = format!(
        "{}__{}",
        utc_timestamp(crate::now_ms()),
        quarantine_name(rel)
    );
    let dest = cdir.join(&name);
    let bytes = std::fs::copy(&src, &dest).map_err(|e| io_err(&src, e))?;
    crate::conflicts::append_index(dir, &name, rel.as_str(), reason, bytes);
    Ok(dest)
}

/// Encoded quarantine file name for `rel`, bounded so it always fits the
/// 255-byte per-component limit (ext4 bytes, NTFS UTF-16 units). Short paths
/// keep the fully readable percent-encoded form; long ones keep a readable
/// truncated prefix plus a 16-hex BLAKE3 of the exact relative path, so
/// distinct deep paths never collide and the original is recoverable from the
/// violation log line (which prints `rel` in full).
pub fn quarantine_name(rel: &RelPath) -> String {
    const MAX_ENCODED: usize = 180;
    let encoded = percent_encode(rel.as_str());
    if encoded.len() <= MAX_ENCODED {
        return encoded;
    }
    let hash = blake3::hash(rel.as_str().as_bytes());
    let hex = data_encoding::HEXLOWER.encode(&hash.as_bytes()[..8]);
    // Cut on a char boundary (percent-encoded output is pure ASCII, but stay
    // defensive) and mark the elision.
    let mut prefix = &encoded[..MAX_ENCODED];
    while !encoded.is_char_boundary(prefix.len()) {
        prefix = &encoded[..prefix.len() - 1];
    }
    format!("{prefix}---{hex}")
}

/// On daemon start: bring every indexed, non-deleted file to its enforced
/// permission. Strict mode (re-)applies the read-only bit (`0444`); easy mode
/// (`strict = off`) makes files writable so any editor can save them in place.
pub fn enforce_all(dir: &Path, state: &AppState, strict: bool) -> Result<(), GuardError> {
    for (rel, rec) in &state.files {
        if rec.deleted {
            continue;
        }
        let path = rel.to_fs_path(dir);
        if strict {
            set_readonly(&path)?;
        } else {
            set_writable(&path)?;
        }
    }
    Ok(())
}

/// Percent-encodes everything outside `[A-Za-z0-9._-]` (including `/`).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// Formats epoch milliseconds as `YYYYMMDDTHHMMSSmmmZ` without external date
/// crates (civil-from-days algorithm).
pub fn utc_timestamp(epoch_ms: u64) -> String {
    let secs = epoch_ms / 1000;
    let ms = epoch_ms % 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}{ms:03}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::index::sanitize_rel_path;

    #[test]
    fn readonly_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.txt");
        std::fs::write(&f, b"hi").unwrap();
        set_readonly(&f).unwrap();
        assert!(std::fs::metadata(&f).unwrap().permissions().readonly());
        set_writable(&f).unwrap();
        assert!(!std::fs::metadata(&f).unwrap().permissions().readonly());
        // Missing files are a no-op, not an error.
        set_readonly(&dir.path().join("missing")).unwrap();
    }

    #[test]
    fn enforce_all_honors_strict_mode() {
        use crate::proto::{FileRecord, ManifestRef};
        let dir = tempfile::tempdir().unwrap();
        let rel = sanitize_rel_path("a.txt").unwrap();
        let abs = rel.to_fs_path(dir.path());
        std::fs::write(&abs, b"data").unwrap();
        let mut state = AppState::new("00".repeat(32), "11".repeat(32));
        state.files.insert(
            rel.clone(),
            FileRecord {
                size: 4,
                manifest: ManifestRef::Inline(vec![]),
                vv: Default::default(),
                deleted: false,
                updated_at_ms: 0,
            },
        );
        // Strict mode clamps the file read-only.
        enforce_all(dir.path(), &state, true).unwrap();
        assert!(std::fs::metadata(&abs).unwrap().permissions().readonly());
        // Easy mode makes it writable again so an editor can save in place.
        enforce_all(dir.path(), &state, false).unwrap();
        assert!(!std::fs::metadata(&abs).unwrap().permissions().readonly());
    }

    #[test]
    fn quarantine_copies_and_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let rel = sanitize_rel_path("sub/data.bin").unwrap();
        let abs = rel.to_fs_path(dir.path());
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b"evidence").unwrap();
        let q = quarantine(dir.path(), &rel, "forced-write").unwrap();
        assert_eq!(std::fs::read(&q).unwrap(), b"evidence");
        assert_eq!(std::fs::read(&abs).unwrap(), b"evidence");
        let name = q.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.contains("__sub%2Fdata.bin"), "{name}");
    }

    #[test]
    fn timestamp_format() {
        // 2026-07-08 12:34:56.789 UTC
        assert_eq!(utc_timestamp(1_783_514_096_789), "20260708T123456789Z");
        assert_eq!(utc_timestamp(0), "19700101T000000000Z");
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode("a/b c.txt"), "a%2Fb%20c.txt");
        assert_eq!(percent_encode("safe-1_2.bin"), "safe-1_2.bin");
    }

    #[test]
    fn quarantine_name_is_bounded_and_collision_free() {
        // Short rels keep the readable encoded form.
        let short = sanitize_rel_path("sub/data.bin").unwrap();
        assert_eq!(quarantine_name(&short), "sub%2Fdata.bin");

        // Deep rels (>255-byte encoded names would exceed the per-component
        // filesystem limit) are truncated + hash-disambiguated.
        let seg = "d".repeat(40);
        let deep_a =
            sanitize_rel_path(&format!("{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/a.bin")).unwrap();
        let deep_b =
            sanitize_rel_path(&format!("{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/b.bin")).unwrap();
        let (na, nb) = (quarantine_name(&deep_a), quarantine_name(&deep_b));
        // Timestamp prefix (20 chars + "__") still fits under 255 with these.
        assert!(na.len() <= 200, "bounded: {} chars", na.len());
        assert_ne!(na, nb, "distinct deep paths must not collide");
        assert!(na.contains("---"), "elision marker present: {na}");
        // Deterministic.
        assert_eq!(na, quarantine_name(&deep_a));
    }

    #[test]
    fn quarantine_works_on_a_deep_path() {
        let dir = tempfile::tempdir().unwrap();
        let seg = "d".repeat(40);
        let rel =
            sanitize_rel_path(&format!("{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/f.bin")).unwrap();
        let abs = rel.to_fs_path(dir.path());
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b"deep evidence").unwrap();
        let q = quarantine(dir.path(), &rel, "forced-write").unwrap();
        assert_eq!(std::fs::read(&q).unwrap(), b"deep evidence");
    }
}
