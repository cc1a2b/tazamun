//! `tazamun doctor` — one-shot NAT and environment report.
//!
//! Invariant: doctor never mutates session state. It queries a running daemon
//! over IPC for its live network view (clearly labelled "from daemon") and
//! augments that with its own local probes (filesystem, IPC path, relay
//! reachability). The path-mount classifier is injected so the DrvFS warning
//! is testable without a real `/mnt`.

use std::path::Path;

use serde::Serialize;

/// Verdict for one report section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl Verdict {
    fn rank(self) -> u8 {
        match self {
            Verdict::Ok => 0,
            Verdict::Warn => 1,
            Verdict::Fail => 2,
        }
    }

    /// The more severe of two verdicts.
    pub fn max_with(self, other: Verdict) -> Verdict {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// One section of the report.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub name: String,
    pub verdict: Verdict,
    /// Human lines; each rendered under the section header.
    pub lines: Vec<String>,
    /// One actionable sentence when not OK.
    pub action: Option<String>,
}

impl Section {
    fn new(name: &str, verdict: Verdict) -> Self {
        Self {
            name: name.to_string(),
            verdict,
            lines: Vec::new(),
            action: None,
        }
    }

    /// An empty section starting at OK (built up by the caller).
    pub fn from_ok(name: &str) -> Self {
        Self::new(name, Verdict::Ok)
    }

    /// An empty section starting at WARN.
    pub fn from_warn(name: &str) -> Self {
        Self::new(name, Verdict::Warn)
    }

    fn line(mut self, s: impl Into<String>) -> Self {
        self.lines.push(s.into());
        self
    }

    fn action(mut self, s: impl Into<String>) -> Self {
        self.action = Some(s.into());
        self
    }
}

/// The full report; the process exit code is `worst_verdict().rank()`.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    pub fn worst(&self) -> Verdict {
        self.sections
            .iter()
            .map(|s| s.verdict)
            .max_by_key(|v| v.rank())
            .unwrap_or(Verdict::Ok)
    }

    pub fn exit_code(&self) -> i32 {
        self.worst().rank() as i32
    }
}

/// How a session folder path is mounted (injected for testability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// Native Linux filesystem — good.
    Native,
    /// A Windows drive mount under WSL (`/mnt/*`) — inotify is unreliable.
    DrvFs,
    /// Not Linux / not applicable.
    Other,
}

/// Classifies a path by its mount for the filesystem-sanity section.
pub fn classify_mount(path: &Path) -> MountKind {
    #[cfg(target_os = "linux")]
    {
        // WSL exposes Windows drives at /mnt/<letter>; those are DrvFS/9p.
        let s = path.to_string_lossy();
        let mut comps = path.components();
        use std::path::Component;
        if let (Some(Component::RootDir), Some(Component::Normal(first))) =
            (comps.next(), comps.next())
            && first == "mnt"
            && s.len() >= 6
            && s.as_bytes().get(5).is_some_and(u8::is_ascii_alphabetic)
        {
            return MountKind::DrvFs;
        }
        MountKind::Native
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        MountKind::Other
    }
}

/// Filesystem-sanity section: watcher backend, mount class, and a live
/// read-only enforcement probe (create → chmod 0444 → verify not writable).
pub fn filesystem_section(dir: &Path, mount: MountKind) -> Section {
    let watcher = if cfg!(target_os = "linux") {
        "inotify"
    } else if cfg!(target_os = "macos") {
        "FSEvents"
    } else if cfg!(target_os = "windows") {
        "ReadDirectoryChangesW"
    } else {
        "notify (platform default)"
    };

    let mut verdict = Verdict::Ok;
    let mut action = None;
    let mut lines = vec![format!("watcher backend    : {watcher}")];
    match mount {
        MountKind::DrvFs => {
            verdict = Verdict::Warn;
            lines.push(format!(
                "session folder     : {} (WSL /mnt drive)",
                dir.display()
            ));
            action = Some(
                "move the session folder to the native Linux filesystem (e.g. ~/…); \
                 /mnt drives do not deliver reliable file-change events"
                    .to_string(),
            );
        }
        MountKind::Native => {
            lines.push(format!(
                "session folder     : {} (native FS)",
                dir.display()
            ));
        }
        MountKind::Other => {
            lines.push(format!("session folder     : {}", dir.display()));
        }
    }

    // Read-only enforcement probe in the metadata tmp dir.
    match readonly_probe(dir) {
        Ok(()) => lines.push("read-only enforce  : working (create+chmod probe passed)".into()),
        Err(e) => {
            verdict = Verdict::Fail;
            lines.push(format!("read-only enforce  : FAILED ({e})"));
            action = Some(
                "the filesystem did not honor a read-only permission change; \
                 tazamun's accidental-save guard rail will not work here"
                    .to_string(),
            );
        }
    }

    let mut s = Section::new("filesystem", verdict);
    s.lines = lines;
    s.action = action;
    s
}

/// Creates a temp file, marks it read-only, and confirms it is not writable.
fn readonly_probe(dir: &Path) -> Result<(), String> {
    let probe_dir = crate::state::tmp_dir(dir);
    std::fs::create_dir_all(&probe_dir).map_err(|e| e.to_string())?;
    let file = tempfile::Builder::new()
        .prefix("doctor-ro-")
        .tempfile_in(&probe_dir)
        .map_err(|e| e.to_string())?;
    let path = file.path().to_path_buf();
    crate::guard::set_readonly(&path).map_err(|e| e.to_string())?;
    let readonly = std::fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions()
        .readonly();
    // Restore writability so the temp file can be cleaned up.
    let _ = crate::guard::set_writable(&path);
    if readonly {
        Ok(())
    } else {
        Err("permissions did not become read-only".into())
    }
}

/// IPC section: reports the resolved socket path/name and whether a daemon is
/// answering on it.
pub fn ipc_section(dir: &Path, daemon_alive: bool) -> Section {
    #[cfg(unix)]
    let where_ = crate::state::AppState::meta_dir(dir)
        .join("daemon.sock")
        .display()
        .to_string();
    #[cfg(not(unix))]
    let where_ = format!("named pipe for {}", dir.display());

    if daemon_alive {
        Section::new("ipc", Verdict::Ok)
            .line(format!("socket             : {where_}"))
            .line("daemon             : responding")
    } else {
        Section::new("ipc", Verdict::Warn)
            .line(format!("socket             : {where_}"))
            .line("daemon             : not running")
            .action("start the daemon with `tazamun start` for live network probes")
    }
}

/// Builds the relay section from the daemon's reported policy/home relay and a
/// locally-measured reachability probe result.
pub fn relay_section(
    policy: &str,
    home_relay: Option<&str>,
    probe: Option<Result<u128, String>>,
) -> Section {
    if policy.starts_with("disabled") {
        return Section::new("relay", Verdict::Ok)
            .line("policy             : disabled by flag (--no-relay)")
            .line("relays             : not used — direct/LAN only");
    }
    let mut s = Section::new("relay", Verdict::Ok).line(format!("policy             : {policy}"));
    match home_relay {
        Some(url) => s = s.line(format!("home relay         : {url}")),
        None => {
            s = s.line("home relay         : none selected yet");
            s.verdict = Verdict::Warn;
            s.action = Some(
                "no home relay chosen yet — if peers can't connect directly, \
                 ensure relays are reachable"
                    .into(),
            );
        }
    }
    match probe {
        Some(Ok(ms)) => s = s.line(format!("reachability       : OK ({ms} ms)")),
        Some(Err(e)) => {
            s = s.line(format!("reachability       : FAILED ({e})"));
            s.verdict = Verdict::Fail;
            s.action = Some(
                "the configured relay did not respond; check the URL and network egress".into(),
            );
        }
        None => s = s.line("reachability       : not probed"),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_classifier() {
        assert_eq!(classify_mount(Path::new("/mnt/e/Programming/x")), {
            #[cfg(target_os = "linux")]
            {
                MountKind::DrvFs
            }
            #[cfg(not(target_os = "linux"))]
            {
                MountKind::Other
            }
        });
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                classify_mount(Path::new("/home/u/projects/x")),
                MountKind::Native
            );
            assert_eq!(classify_mount(Path::new("/mnt/c/y")), MountKind::DrvFs);
            // "/mnt" alone (no drive letter) is not a DrvFS drive.
            assert_eq!(classify_mount(Path::new("/mnt")), MountKind::Native);
            assert_eq!(classify_mount(Path::new("/mntopia/x")), MountKind::Native);
        }
    }

    #[test]
    fn drvfs_section_warns_with_action() {
        let s = filesystem_section(Path::new("/mnt/e/proj"), MountKind::DrvFs);
        assert_eq!(s.verdict, Verdict::Warn);
        assert!(s.action.is_some());
        assert!(s.lines.iter().any(|l| l.contains("WSL /mnt drive")));
    }

    #[test]
    fn native_readonly_probe_passes() {
        let dir = tempfile::tempdir().unwrap();
        let s = filesystem_section(dir.path(), MountKind::Native);
        // On a normal native FS the probe passes → section is OK.
        assert_eq!(s.verdict, Verdict::Ok, "{:?}", s.lines);
        assert!(s.lines.iter().any(|l| l.contains("working")));
    }

    #[test]
    fn relay_disabled_is_ok_not_error() {
        let s = relay_section("disabled (--no-relay)", None, None);
        assert_eq!(s.verdict, Verdict::Ok);
        assert!(s.lines.iter().any(|l| l.contains("disabled by flag")));
    }

    #[test]
    fn relay_probe_failure_is_fail() {
        let s = relay_section(
            "custom: https://r.example",
            Some("https://r.example"),
            Some(Err("timeout".into())),
        );
        assert_eq!(s.verdict, Verdict::Fail);
    }

    #[test]
    fn report_worst_and_exit_code() {
        let r = Report {
            sections: vec![
                Section::new("a", Verdict::Ok),
                Section::new("b", Verdict::Warn),
            ],
        };
        assert_eq!(r.worst(), Verdict::Warn);
        assert_eq!(r.exit_code(), 1);
        let r = Report {
            sections: vec![Section::new("a", Verdict::Fail)],
        };
        assert_eq!(r.exit_code(), 2);
    }
}
