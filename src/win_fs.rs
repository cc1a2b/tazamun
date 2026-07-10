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
    if let Some(rest) = backslashed.strip_prefix(r"\\") {
        if !rest.is_empty() && !rest.starts_with('\\') {
            return Some(format!(r"\\?\UNC\{rest}"));
        }
    }
    // Relative or non-Windows-style (e.g. `/tmp/x`): no conversion.
    None
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
}
