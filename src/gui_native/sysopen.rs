//! P24 "Open folder": reveal a session directory in the OS file manager.
//!
//! One button, one job — hand a path to the platform's file explorer and return
//! immediately. The window that opens belongs to the user, not to us: the helper
//! is spawned fully detached (no inherited stdio, no `wait`) so a slow or chatty
//! file manager never blocks the GUI thread, and `explorer`'s habit of exiting
//! with code 1 even on success is simply never observed. The platform *decision*
//! lives in the pure [`command_for`] so every branch is unit-testable on any host
//! without spawning a thing; only [`open_folder`] touches the process table.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// Which file-manager convention applies. Windows and macOS are settled at
/// compile time; Linux splits at runtime into WSL (bridge to Windows Explorer)
/// and everything else (`xdg-open`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Os {
    Windows,
    Mac,
    LinuxWsl,
    LinuxOther,
}

/// Opens `path` in the platform file manager, detached and non-blocking.
pub fn open_folder(path: &Path) -> io::Result<()> {
    let os = detect_os();
    let wsl_windows_path = if os == Os::LinuxWsl {
        wslpath_to_windows(path)
    } else {
        None
    };
    // Only a *successful* conversion routes through explorer.exe; keep that fact
    // so a failed explorer.exe spawn can still fall back to xdg-open below.
    let bridged = os == Os::LinuxWsl && wsl_windows_path.is_some();
    let (program, args) = command_for(path, os, wsl_windows_path);
    match spawn_detached(&program, &args) {
        Err(_) if bridged => {
            let (program, args) = command_for(path, Os::LinuxOther, None);
            spawn_detached(&program, &args)
        }
        other => other,
    }
}

/// Pure platform mapping: given the OS (and, for WSL, the already-converted
/// Windows path) return the `(program, args)` to spawn. No I/O, no spawning —
/// this is the whole unit-test surface.
fn command_for(path: &Path, os: Os, wsl_windows_path: Option<String>) -> (String, Vec<String>) {
    let as_str = || path.to_string_lossy().into_owned();
    match os {
        // Pass the path exactly as given — never canonicalize into `\\?\`
        // extended-length form, which explorer rejects.
        Os::Windows => ("explorer".to_string(), vec![as_str()]),
        Os::Mac => ("open".to_string(), vec![as_str()]),
        Os::LinuxWsl => match wsl_windows_path {
            Some(win) => ("explorer.exe".to_string(), vec![win]),
            None => ("xdg-open".to_string(), vec![as_str()]),
        },
        Os::LinuxOther => ("xdg-open".to_string(), vec![as_str()]),
    }
}

/// Runtime OS classification. Matching the `std::env::consts::OS` string (rather
/// than `cfg`) keeps every arm — and thus every `Os` variant and helper —
/// reachable on all targets, so the module needs no per-platform `cfg` blocks and
/// trips no dead-code lint on a single-target build.
fn detect_os() -> Os {
    match std::env::consts::OS {
        "windows" => Os::Windows,
        "macos" => Os::Mac,
        _ if is_wsl() => Os::LinuxWsl,
        _ => Os::LinuxOther,
    }
}

/// WSL iff the kernel advertises "microsoft" in `/proc/version` (case-insensitive)
/// AND `wslpath` resolves on `PATH` — both must hold before trusting the bridge.
/// Shared with `folderpick`, so the picker and the reveal agree on what WSL is.
pub(super) fn is_wsl() -> bool {
    let kernel_is_microsoft = std::fs::read_to_string("/proc/version")
        .map(|v| v.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false);
    kernel_is_microsoft && which_on_path("wslpath")
}

/// Converts a Linux path to its `C:\…` Windows form via `wslpath -w`. Runs and
/// waits (the call is instant); any failure — missing tool, non-zero exit,
/// non-UTF8 or empty output — yields `None` so the caller falls back to
/// `xdg-open`.
fn wslpath_to_windows(path: &Path) -> Option<String> {
    let out = Command::new("wslpath")
        .arg("-w")
        .arg(path)
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let win = String::from_utf8(out.stdout).ok()?;
    let win = win.trim_end_matches(['\r', '\n']).to_string();
    if win.is_empty() { None } else { Some(win) }
}

/// True if `name` resolves against `PATH` (each entry joined with `name`); the
/// executable bit is not checked — presence is enough to decide whether the WSL
/// bridge is even worth attempting.
fn which_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Spawns `program` fully detached: no inherited stdio, no `wait`, status ignored.
/// Returns the spawn error verbatim (a missing helper surfaces as `NotFound`).
fn spawn_detached(program: &str, args: &[String]) -> io::Result<()> {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map(|_child| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_uses_explorer_with_raw_path() {
        let (prog, args) = command_for(Path::new(r"C:\Users\me\proj"), Os::Windows, None);
        assert_eq!(prog, "explorer");
        assert_eq!(args, vec![r"C:\Users\me\proj".to_string()]);
    }

    #[test]
    fn macos_uses_open() {
        let (prog, args) = command_for(Path::new("/Users/me/proj"), Os::Mac, None);
        assert_eq!(prog, "open");
        assert_eq!(args, vec!["/Users/me/proj".to_string()]);
    }

    #[test]
    fn wsl_with_conversion_bridges_to_explorer_exe() {
        let win = r"C:\Users\me\proj".to_string();
        let (prog, args) = command_for(Path::new("/home/me/proj"), Os::LinuxWsl, Some(win.clone()));
        assert_eq!(prog, "explorer.exe");
        assert_eq!(args, vec![win]);

        // A failed/absent conversion falls back to xdg-open with the raw path.
        let (prog, args) = command_for(Path::new("/home/me/proj"), Os::LinuxWsl, None);
        assert_eq!(prog, "xdg-open");
        assert_eq!(args, vec!["/home/me/proj".to_string()]);
    }

    #[test]
    fn linux_other_uses_xdg_open() {
        let (prog, args) = command_for(Path::new("/home/me/proj"), Os::LinuxOther, None);
        assert_eq!(prog, "xdg-open");
        assert_eq!(args, vec!["/home/me/proj".to_string()]);
    }
}
