//! P19 opt-in desktop notifications via the OS-native notifier.
//!
//! Off by default (a sync daemon popping toasts is intrusive). When
//! `config notify on`, the events that genuinely want a human — a conflict
//! quarantined, a peer that went offline while holding a lease, an available
//! update — shell out to the platform notifier with no new dependency and no
//! tray app: `notify-send` (Linux), `osascript` (macOS), a PowerShell toast
//! (Windows). Fire-and-forget off the actor; a missing notifier is silently
//! ignored.

use std::path::Path;

/// The audit `kind`s that warrant a desktop notification, with a title.
pub fn notify_title(kind: &str) -> Option<&'static str> {
    match kind {
        "quarantine" => Some("tazamun: conflict preserved"),
        "peer-offline-held" => Some("tazamun: peer offline mid-lease"),
        "update-available" => Some("tazamun: update available"),
        _ => None,
    }
}

/// Sends a notification off the actor (best-effort). Safe to call for any kind;
/// only [`notify_title`]-worthy kinds actually notify.
pub fn maybe_notify(kind: &str, body: String) {
    let Some(title) = notify_title(kind) else {
        return;
    };
    tokio::task::spawn_blocking(move || send(title, &body));
}

/// Shells out to the platform notifier. Best-effort: a missing tool is ignored.
pub fn send(title: &str, body: &str) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("notify-send")
            .arg("--app-name=tazamun")
            .arg("--") // end of options: a title/body starting with `-` is data
            .arg(title)
            .arg(body)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        // AppleScript string-escape: backslash and double-quote.
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            esc(body),
            esc(title)
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        // A minimal balloon via PowerShell + WinForms NotifyIcon — no module
        // install needed. Single-quoted PS literals with doubled quotes so a
        // crafted title/body cannot break out of the string.
        let ps_lit = |s: &str| s.replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference='SilentlyContinue'; \
             Add-Type -AssemblyName System.Windows.Forms; \
             $n=New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon=[System.Drawing.SystemIcons]::Information; $n.Visible=$true; \
             $n.ShowBalloonTip(5000,'{}','{}',[System.Windows.Forms.ToolTipIcon]::Info); \
             Start-Sleep -Milliseconds 6000; $n.Dispose()",
            ps_lit(title),
            ps_lit(body),
        );
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, target_os = "macos", target_os = "windows")))]
    {
        let _ = (title, body);
    }
}

/// Where the updater's "update available" notification is surfaced from — kept
/// here so the one-line call site stays declarative. `dir` is accepted for
/// symmetry with future per-folder notifier config; unused today.
pub fn update_available(_dir: &Path, version: &str) {
    maybe_notify(
        "update-available",
        format!("tazamun {version} is available — run `tazamun update`"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_noteworthy_kinds_notify() {
        assert!(notify_title("quarantine").is_some());
        assert!(notify_title("peer-offline-held").is_some());
        assert!(notify_title("update-available").is_some());
        assert!(notify_title("lock").is_none());
        assert!(notify_title("publish").is_none());
        // maybe_notify on a non-worthy kind must not spawn / must be a no-op;
        // it simply returns (no runtime needed for the early return).
        maybe_notify("lock", "x".into());
    }
}
