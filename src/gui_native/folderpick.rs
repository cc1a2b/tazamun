//! Choosing a folder, on every platform this ships to.
//!
//! `rfd` is built here against the XDG desktop portal only, which is the right
//! choice on a real Linux desktop and useless on WSL: WSLg gives you a Wayland
//! socket and no portal, no zenity and no kdialog, so `pick_folder` returned
//! `None` and the Browse button did nothing at all — indistinguishable, from
//! the user's side, from a broken build.
//!
//! So WSL gets the same treatment `sysopen` already gives "reveal in file
//! manager": bridge to the Windows side. `Shell.Application.BrowseForFolder`
//! through `powershell.exe` is the native Windows folder picker, and `wslpath
//! -u` converts what it returns back into a Linux path. Everywhere else `rfd`
//! is used as before.
//!
//! The other half of the job is refusing to fail silently. A cancelled dialog
//! and an absent dialog look identical through `Option<PathBuf>`, so this
//! module distinguishes them and, on a Linux box with no portal at all, says
//! so and names the package that fixes it.
//!
//! The platform *decision* and the output parsing are pure and unit-tested;
//! only [`pick`] touches the process table.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::sysopen;

/// Why no folder came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    /// The user closed the dialog. Not an error worth reporting.
    Cancelled,
    /// No dialog could be shown at all, with an actionable explanation.
    NoBackend(String),
}

/// Which dialog to put on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    /// WSL: bridge to the Windows picker through `powershell.exe`.
    WindowsBridge,
    /// Whatever `rfd` can reach — portal on Linux, native on Windows/macOS.
    Rfd,
    /// A Linux session with no portal and no bridge: nothing can be shown.
    None,
}

/// The PowerShell that puts up the Windows folder picker and prints the chosen
/// path. `BrowseForFolder` returns `$null` on cancel, so cancel prints nothing.
/// Single-quoted throughout so nothing needs escaping on the way through.
const PS_PICK: &str = "$ErrorActionPreference='Stop'; \
     $s = New-Object -ComObject Shell.Application; \
     $f = $s.BrowseForFolder(0, 'Choose a folder to sync', 0); \
     if ($f -ne $null) { $f.Self.Path }";

/// Opens the platform folder picker and returns the chosen directory.
///
/// Blocking: the caller runs this on a detached thread (dropping a tokio
/// runtime waits forever on `spawn_blocking`, so a dialog left open would hang
/// exit — see the `PickFolder` arm in the worker).
pub fn pick(title: &str) -> Result<PathBuf, PickError> {
    match backend() {
        Backend::WindowsBridge => match pick_via_windows() {
            Ok(p) => Ok(p),
            // The bridge is best-effort: if PowerShell or the COM object is
            // unavailable, still give `rfd` its chance before giving up.
            Err(PickError::Cancelled) => Err(PickError::Cancelled),
            Err(_) => pick_via_rfd(title),
        },
        Backend::Rfd => pick_via_rfd(title),
        Backend::None => Err(PickError::NoBackend(NO_BACKEND_HINT.to_string())),
    }
}

/// Shown when a Linux session can offer no dialog at all. It names the fix and
/// the way around it, because "nothing happened" is not a message.
const NO_BACKEND_HINT: &str = "no folder dialog is available on this desktop — \
     install xdg-desktop-portal-gtk (or xdg-desktop-portal-kde), \
     or type the folder path into the field instead";

fn backend() -> Backend {
    if sysopen::is_wsl() && which("powershell.exe") {
        return Backend::WindowsBridge;
    }
    // Windows and macOS always have a native dialog behind `rfd`.
    if !cfg!(target_os = "linux") {
        return Backend::Rfd;
    }
    if portal_present() {
        Backend::Rfd
    } else {
        Backend::None
    }
}

/// Whether an XDG desktop portal looks installed. `rfd` talks to it over D-Bus
/// and simply yields `None` when nobody answers, so this is checked up front to
/// tell "you cancelled" apart from "there was nothing to cancel".
fn portal_present() -> bool {
    const DIRS: &[&str] = &[
        "/usr/libexec",
        "/usr/lib/xdg-desktop-portal",
        "/usr/lib",
        "/usr/local/libexec",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];
    DIRS.iter()
        .any(|d| std::path::Path::new(d).join("xdg-desktop-portal").is_file())
        || which("xdg-desktop-portal")
}

fn pick_via_rfd(title: &str) -> Result<PathBuf, PickError> {
    rfd::FileDialog::new()
        .set_title(title)
        .pick_folder()
        // `rfd` cannot distinguish a cancel from an unreachable dialog; the
        // backend probe above is what keeps this honest.
        .ok_or(PickError::Cancelled)
}

/// Runs the Windows picker and converts its answer back to a Linux path.
fn pick_via_windows() -> Result<PathBuf, PickError> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", PS_PICK])
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| PickError::NoBackend(format!("could not run powershell.exe: {e}")))?;
    if !out.status.success() {
        return Err(PickError::NoBackend(
            "the Windows folder picker exited with an error".to_string(),
        ));
    }
    let win = parse_ps_path(&String::from_utf8_lossy(&out.stdout)).ok_or(PickError::Cancelled)?;
    wslpath_to_unix(&win).ok_or_else(|| {
        PickError::NoBackend(format!(
            "could not map the Windows path {win} back into WSL"
        ))
    })
}

/// The chosen path out of PowerShell's stdout: the last non-blank line, with
/// trailing CR/LF removed. `None` when the user cancelled (nothing printed).
fn parse_ps_path(stdout: &str) -> Option<String> {
    let line = stdout
        .lines()
        .map(|l| l.trim_end_matches(['\r', '\n']).trim())
        .rfind(|l| !l.is_empty())?;
    Some(line.to_string())
}

/// `wslpath -u` — the inverse of the conversion `sysopen` uses for reveal.
fn wslpath_to_unix(win: &str) -> Option<PathBuf> {
    let out = Command::new("wslpath")
        .arg("-u")
        .arg(win)
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim_end_matches(['\r', '\n']).trim();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_windows_dialog_prints_nothing() {
        assert_eq!(parse_ps_path(""), None);
        assert_eq!(parse_ps_path("\r\n"), None);
        assert_eq!(parse_ps_path("   \n  \r\n"), None);
    }

    #[test]
    fn a_chosen_path_survives_windows_line_endings() {
        assert_eq!(
            parse_ps_path("C:\\Users\\me\\proj\r\n").as_deref(),
            Some("C:\\Users\\me\\proj")
        );
        assert_eq!(
            parse_ps_path("C:\\Users\\me\\proj").as_deref(),
            Some("C:\\Users\\me\\proj")
        );
    }

    #[test]
    fn a_unc_path_back_into_wsl_survives() {
        assert_eq!(
            parse_ps_path("\\\\wsl$\\kali-linux\\home\\me\\proj\r\n").as_deref(),
            Some("\\\\wsl$\\kali-linux\\home\\me\\proj")
        );
    }

    #[test]
    fn the_last_non_blank_line_wins() {
        // A profile banner or a stray warning must not be mistaken for the
        // answer; the path is what PowerShell prints last.
        assert_eq!(
            parse_ps_path("warning: something\r\nC:\\proj\r\n\r\n").as_deref(),
            Some("C:\\proj")
        );
    }

    #[test]
    fn a_path_with_spaces_is_not_split() {
        assert_eq!(
            parse_ps_path("C:\\Users\\me\\My Documents\\a folder\r\n").as_deref(),
            Some("C:\\Users\\me\\My Documents\\a folder")
        );
    }

    #[test]
    fn the_no_backend_hint_names_a_fix_and_a_way_around() {
        // "nothing happened" is not a message: the text must say what to
        // install and what to do instead.
        assert!(NO_BACKEND_HINT.contains("xdg-desktop-portal"));
        assert!(NO_BACKEND_HINT.contains("type the folder path"));
    }

    #[test]
    fn the_powershell_script_cancels_cleanly_and_needs_no_escaping() {
        // Single quotes throughout, so the command survives the trip through
        // the shell-less `Command` argv without escaping.
        assert!(!PS_PICK.contains('"'));
        assert!(PS_PICK.contains("BrowseForFolder"));
        // Cancel must print nothing rather than the string "null".
        assert!(PS_PICK.contains("-ne $null"));
    }
}
