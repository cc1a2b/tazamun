//! Windows extended-length path support.
//!
//! Invariant: every absolute path that reaches a filesystem syscall on Windows
//! goes through [`to_extended`] first, so operations keep working past the
//! legacy 260-character `MAX_PATH` limit **regardless** of the OS-side
//! `LongPathsEnabled` registry switch (the embedded `longPathAware` manifest
//! only helps when that switch is on; `\\?\` always works).
//!
//! `\\?\` caveats, honored by construction here:
//! - **No normalization**: the kernel does not resolve `.`/`..` or slashes in
//!   extended-length paths. Callers only pass absolute paths built from an
//!   absolutized session root (`GetFullPathNameW` semantics via
//!   `std::path::absolute`) joined with sanitized relative segments (the
//!   sanitizer rejects dot segments), so nothing needs normalizing.
//! - **Backslashes only**: `/` is not a separator under `\\?\`. Paths are
//!   built with `PathBuf::push`, which uses `\` on Windows; the string form is
//!   additionally converted defensively.
//! - **Drive and UNC forms differ**: `C:\x` becomes `\\?\C:\x`, while
//!   `\\server\share\x` becomes `\\?\UNC\server\share\x`.
//!
//! On non-Windows targets [`to_extended`] is the identity function.

use std::path::{Path, PathBuf};

/// Converts an absolute Windows path to `\\?\` extended-length form.
/// Relative paths, already-extended paths, and non-Windows targets pass
/// through unchanged.
#[cfg(windows)]
pub fn to_extended(path: &Path) -> PathBuf {
    match extended_form(&path.to_string_lossy()) {
        Some(s) => PathBuf::from(s),
        None => path.to_path_buf(),
    }
}

/// Non-Windows: identity (extended-length form is a Windows concept).
#[cfg(not(windows))]
pub fn to_extended(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// The pure string transformation behind [`to_extended`], separated so the
/// mapping is unit-testable on every host OS. Returns `None` when the input
/// needs no conversion (already extended, relative, or not a Windows absolute
/// path).
fn extended_form(s: &str) -> Option<String> {
    // Already extended (`\\?\…`) or a device path (`\\.\…`): leave untouched.
    if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") {
        return None;
    }
    let backslashed = s.replace('/', r"\");
    let bytes = backslashed.as_bytes();
    // Drive-absolute: `C:\…`.
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        return Some(format!(r"\\?\{backslashed}"));
    }
    // UNC: `\\server\share\…` → `\\?\UNC\server\share\…`.
    if let Some(rest) = backslashed.strip_prefix(r"\\")
        && !rest.is_empty()
        && !rest.starts_with('\\')
    {
        return Some(format!(r"\\?\UNC\{rest}"));
    }
    // Relative or non-Windows-style (e.g. `/tmp/x`): no conversion.
    None
}

// ── Windows file-op resilience ──────────────────────────────────────────────
//
// Antivirus scanners, indexers, and editors briefly hold handles on files we
// rename over, delete, or re-attribute. Windows surfaces that as
// ERROR_SHARING_VIOLATION (32) — or ERROR_ACCESS_DENIED (5) in the
// set-attributes race — where Unix would just succeed. Every such op goes
// through a bounded exponential retry: 6 attempts, 50 ms → 1.6 s doubling with
// ±20% deterministic-jitter, ≤ 3.5 s worst-case total, `debug!` per retry, the
// original error surfaced on final failure. On non-Windows the wrappers are
// zero-retry pass-throughs.
//
// Ordering rule for read-only files (Windows refuses to delete or rename over
// a file with the read-only attribute): **clear read-only → mutate → re-apply
// read-only when the survivor should be guarded.** Every delete/rename-over
// site follows it.

use std::time::Duration;

/// Max attempts for a retryable Windows file op (initial try + 5 retries).
pub const RETRY_ATTEMPTS: u32 = 6;

/// Deterministically jittered exponential backoff before retry `attempt`
/// (1-based: the wait after the attempt-th failure). 50 ms → 1.6 s doubling,
/// ±20% jitter derived from the attempt number, ≤ 3.5 s summed.
pub fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = 50u64 << (attempt.saturating_sub(1)).min(5);
    // ±20% jitter without an RNG: spread by attempt parity/step so concurrent
    // retriers de-synchronize while the total stays provably bounded.
    let jitter = base_ms / 5;
    let ms = match attempt % 3 {
        0 => base_ms - jitter,
        1 => base_ms + jitter,
        _ => base_ms,
    };
    Duration::from_millis(ms)
}

/// Whether a raw OS error code is worth retrying on Windows.
/// 32 = ERROR_SHARING_VIOLATION (another process holds the file);
/// 5 = ERROR_ACCESS_DENIED, which Windows also returns for the transient
/// attribute race on files being concurrently re-attributed. A genuine ACL
/// denial also matches 5 and simply costs one bounded (≤ 3.5 s) retry cycle
/// before surfacing unchanged.
pub fn is_retryable_code(code: i32) -> bool {
    code == 32 || code == 5
}

fn should_retry(e: &std::io::Error) -> bool {
    cfg!(windows) && e.raw_os_error().is_some_and(is_retryable_code)
}

/// Runs a fallible file op with the bounded retry policy. The closure owns all
/// path state; the final error is the original last failure.
pub fn with_retry<T>(
    op: &str,
    path: &Path,
    mut f: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut attempt = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if attempt < RETRY_ATTEMPTS && should_retry(&e) => {
                let delay = backoff_delay(attempt);
                tracing::debug!(
                    op,
                    path = %path.display(),
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "transient Windows file-op failure; retrying"
                );
                std::thread::sleep(delay);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// `std::fs::remove_file` with the retry policy. Missing files are success
/// (delete is idempotent at every call site).
pub fn remove_file(path: &Path) -> std::io::Result<()> {
    with_retry("remove_file", path, || match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    })
}

/// Persists a staged temp file over `dest` (atomic rename) with the retry
/// policy. `TempPath::persist` consumes the handle but hands it back inside
/// the error, so retries re-drive the same temp file.
pub fn persist_temp(mut temp: tempfile::TempPath, dest: &Path) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match temp.persist(dest) {
            Ok(()) => return Ok(()),
            Err(pe) => {
                if attempt < RETRY_ATTEMPTS && should_retry(&pe.error) {
                    let delay = backoff_delay(attempt);
                    tracing::debug!(
                        op = "persist",
                        path = %dest.display(),
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %pe.error,
                        "transient Windows rename-over failure; retrying"
                    );
                    temp = pe.path;
                    std::thread::sleep(delay);
                    attempt += 1;
                } else {
                    return Err(pe.error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_paths_gain_the_extended_prefix() {
        assert_eq!(
            extended_form(r"C:\deep\tree\file.bin").as_deref(),
            Some(r"\\?\C:\deep\tree\file.bin")
        );
        // Forward slashes are converted — `/` is not a separator under `\\?\`.
        assert_eq!(
            extended_form("E:/mixed/sep\\file").as_deref(),
            Some(r"\\?\E:\mixed\sep\file")
        );
    }

    #[test]
    fn unc_paths_use_the_unc_form() {
        assert_eq!(
            extended_form(r"\\server\share\dir\f.txt").as_deref(),
            Some(r"\\?\UNC\server\share\dir\f.txt")
        );
    }

    #[test]
    fn already_extended_and_device_paths_are_untouched() {
        assert_eq!(extended_form(r"\\?\C:\already\ext"), None);
        assert_eq!(extended_form(r"\\.\pipe\tazamun-abc"), None);
    }

    #[test]
    fn relative_and_unix_paths_are_untouched() {
        assert_eq!(extended_form(r"relative\path"), None);
        assert_eq!(extended_form("/tmp/session/file"), None);
        assert_eq!(extended_form(""), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_to_extended_is_identity() {
        let p = Path::new("/some/unix/path");
        assert_eq!(to_extended(p), p);
    }

    #[cfg(windows)]
    #[test]
    fn windows_to_extended_converts_absolute() {
        let p = Path::new(r"C:\a\b");
        assert_eq!(to_extended(p), Path::new(r"\\?\C:\a\b"));
    }

    #[test]
    fn backoff_doubles_with_bounded_jitter_and_total() {
        // Base sequence 50,100,200,400,800,1600 with ±20% jitter per step.
        let mut total = Duration::ZERO;
        for attempt in 1..RETRY_ATTEMPTS {
            let base = 50u64 << (attempt - 1).min(5);
            let d = backoff_delay(attempt).as_millis() as u64;
            let lo = base - base / 5;
            let hi = base + base / 5;
            assert!(
                (lo..=hi).contains(&d),
                "attempt {attempt}: {d} ∉ [{lo},{hi}]"
            );
            total += backoff_delay(attempt);
        }
        assert!(
            total <= Duration::from_millis(3_500),
            "worst-case retry wait must stay under 3.5s, got {total:?}"
        );
        // Deterministic (no RNG): same attempt, same delay.
        assert_eq!(backoff_delay(3), backoff_delay(3));
    }

    #[test]
    fn retryable_codes_are_exactly_sharing_and_access() {
        assert!(is_retryable_code(32), "ERROR_SHARING_VIOLATION");
        assert!(is_retryable_code(5), "ERROR_ACCESS_DENIED attribute race");
        assert!(!is_retryable_code(2), "FILE_NOT_FOUND is not transient");
        assert!(!is_retryable_code(0));
    }

    #[test]
    fn remove_file_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        std::fs::write(&f, b"x").unwrap();
        remove_file(&f).unwrap();
        // Second delete of a missing file is success, not NotFound.
        remove_file(&f).unwrap();
    }

    #[test]
    fn persist_temp_lands_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let named = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        std::fs::write(named.path(), b"staged").unwrap();
        let dest = dir.path().join("final.bin");
        persist_temp(named.into_temp_path(), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"staged");
    }
}
