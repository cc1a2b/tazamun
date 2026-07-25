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

/// True when this looks like WSL2 in its default **NAT** networking mode: the
/// kernel is WSL and the address the kernel picks to reach the internet is in
/// the `172.16.0.0/12` range WSL's virtual switch hands out. In that mode each
/// WSL machine sits on its own isolated subnet and cannot reach another WSL
/// machine directly; P2P then leans entirely on a relay, which the double NAT
/// frequently defeats — the exact "connect timed out" a user hits with `send`,
/// `receive`, or a session join. Mirrored networking mode puts WSL on the
/// host's real LAN and resolves it. Pure and unit-tested; the section below
/// supplies the live inputs.
pub fn is_wsl_nat(is_wsl: bool, default_ipv4: Option<std::net::Ipv4Addr>) -> bool {
    is_wsl
        && default_ipv4.is_some_and(|ip| {
            let o = ip.octets();
            o[0] == 172 && (16..=31).contains(&o[1])
        })
}

fn running_under_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/version")
            .map(|v| v.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// The IPv4 address the kernel would use to reach the internet, learned from a
/// UDP `connect` (which sends no packets). `None` when there is no route.
fn default_outbound_ipv4() -> Option<std::net::Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect((std::net::Ipv4Addr::new(1, 1, 1, 1), 80))
        .ok()?;
    match sock.local_addr().ok()? {
        std::net::SocketAddr::V4(a) => Some(*a.ip()),
        std::net::SocketAddr::V6(_) => None,
    }
}

/// Networking-environment section. WSL2 NAT mode is the one setup where a
/// perfectly healthy daemon still cannot reach its peers, and the fix is
/// Windows-side (the daemon cannot apply it) — so the least the tool can do is
/// name it. `None` off WSL, where this never applies.
pub fn network_env_section() -> Option<Section> {
    if !running_under_wsl() {
        return None;
    }
    let ip = default_outbound_ipv4();
    if is_wsl_nat(true, ip) {
        let ips = ip.map(|i| i.to_string()).unwrap_or_else(|| "?".into());
        return Some(
            Section::new("networking (WSL)", Verdict::Warn)
                .line(format!(
                    "mode               : NAT — this WSL is isolated at {ips}"
                ))
                .line("peer reach         : two WSL machines here cannot reach each other,")
                .line("                     and relay-only P2P across two WSL NATs often times out")
                .action(
                    "enable WSL mirrored networking on BOTH machines: put a line `[wsl2]` and \
                     below it `networkingMode=mirrored` in C:\\Users\\<you>\\.wslconfig, then \
                     `wsl --shutdown` and reopen (needs Windows 11 22H2+). On Windows 10, run \
                     the native Windows tazamun build instead of the one inside WSL.",
                ),
        );
    }
    Some(
        Section::from_ok("networking (WSL)")
            .line("mode               : mirrored or bridged (not isolated NAT) — peer reach OK"),
    )
}

/// Windows long-path section: the embedded `longPathAware` manifest status,
/// the `LongPathsEnabled` registry switch, and a live >260-char probe with
/// plain (non-`\\?\`) APIs. tazamun itself is immune either way — every fs
/// boundary converts to `\\?\` extended-length form (`src/win_fs.rs`) — but
/// other programs touching a deep session folder (editors, explorers) depend
/// on the registry switch, so a `0` gets a WARN with the exact enable command.
#[cfg(windows)]
pub fn long_paths_section(_dir: &Path) -> Option<Section> {
    let mut s = Section::from_ok("long paths");
    s.lines
        .push("manifest           : longPathAware embedded at build time".into());
    s.lines
        .push("tazamun fs calls   : \\\\?\\ extended-length at every boundary".into());

    let reg = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SYSTEM\CurrentControlSet\Control\FileSystem",
            "/v",
            "LongPathsEnabled",
        ])
        .output();
    let enabled = match &reg {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains("0x1") {
                Some(true)
            } else if text.contains("0x0") {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    };
    match enabled {
        Some(true) => s
            .lines
            .push("LongPathsEnabled   : 1 (other programs get long paths too)".into()),
        Some(false) => {
            s.verdict = Verdict::Warn;
            s.lines
                .push("LongPathsEnabled   : 0 — OTHER programs will fail on deep paths".into());
            s.action = Some(
                "run as admin: New-ItemProperty -Path \"HKLM:\\SYSTEM\\CurrentControlSet\\Control\\FileSystem\" -Name LongPathsEnabled -Value 1 -PropertyType DWORD -Force"
                    .into(),
            );
        }
        None => s
            .lines
            .push("LongPathsEnabled   : unreadable (reg query failed)".into()),
    }

    // Live probe: can a plain Win32 path exceed MAX_PATH end-to-end right now?
    let deep = std::env::temp_dir()
        .join("a".repeat(80))
        .join("b".repeat(80))
        .join("c".repeat(80));
    let probe = std::fs::create_dir_all(&deep)
        .and_then(|()| std::fs::write(deep.join("probe.txt"), b"x"))
        .and_then(|()| std::fs::remove_file(deep.join("probe.txt")));
    match probe {
        Ok(()) => s
            .lines
            .push("live >260 probe    : plain-path create+write+delete OK".into()),
        Err(e) => {
            s.verdict = s.verdict.max_with(Verdict::Warn);
            s.lines.push(format!(
                "live >260 probe    : failed with plain paths ({e})"
            ));
        }
    }
    Some(s)
}

/// Non-Windows: long paths are not a constraint; no section.
#[cfg(not(windows))]
pub fn long_paths_section(_dir: &Path) -> Option<Section> {
    None
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

/// P18 quarantine report: how much preserved-bytes hygiene debt the folder
/// carries. Purely local (reads `.tazamun/conflicts` directly), so it works
/// with the daemon down. Informational unless the quarantine has grown old or
/// large enough to deserve a look.
pub fn quarantine_section(dir: &Path) -> Section {
    let entries = crate::conflicts::list(dir);
    if entries.is_empty() {
        return Section::new("quarantine", Verdict::Ok)
            .line("copies             : none — no preserved conflict bytes");
    }
    let count = entries.len();
    let bytes: u64 = entries.iter().map(|e| e.size).sum();
    let now = crate::now_ms();
    let oldest_ms = entries
        .iter()
        .map(|e| now.saturating_sub(e.ts_ms))
        .max()
        .unwrap_or(0);
    let oldest = humantime::format_duration(std::time::Duration::from_secs(oldest_ms / 1000));
    // Old or bulky quarantines deserve a nudge, not an alarm.
    const NUDGE_AGE_MS: u64 = 90 * 24 * 60 * 60 * 1000;
    const NUDGE_BYTES: u64 = 512 * 1024 * 1024;
    let verdict = if oldest_ms > NUDGE_AGE_MS || bytes > NUDGE_BYTES {
        Verdict::Warn
    } else {
        Verdict::Ok
    };
    let s = Section::new("quarantine", verdict)
        .line(format!("copies             : {count}"))
        .line(format!(
            "total size         : {}",
            crate::state::fmt_size(bytes)
        ))
        .line(format!("oldest             : {oldest} ago"));
    if verdict == Verdict::Warn {
        s.action(
            "review with `tazamun conflicts list`, resolve what matters, then \
             `tazamun conflicts prune --older-than 90d`",
        )
    } else {
        s
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
        // A zero here means "the daemon's relay link is up" (connection/TLS
        // handshake succeeded) without a separately measured round-trip.
        Some(Ok(0)) => s = s.line("reachability       : reachable (relay link up)"),
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
    use std::net::Ipv4Addr;

    #[test]
    fn wsl_nat_is_the_isolated_172_range_and_only_on_wsl() {
        // The condition that strands two WSL peers: WSL + a 172.16/12 address.
        // Generic representatives — the range and its two boundaries, no real
        // machine's address.
        assert!(is_wsl_nat(true, Some(Ipv4Addr::new(172, 20, 0, 10))));
        assert!(is_wsl_nat(true, Some(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_wsl_nat(true, Some(Ipv4Addr::new(172, 31, 255, 254))));
        // Not WSL — never flagged, whatever the address.
        assert!(!is_wsl_nat(false, Some(Ipv4Addr::new(172, 22, 41, 207))));
        // Mirrored mode / a real LAN — WSL, but the host's own address.
        assert!(!is_wsl_nat(true, Some(Ipv4Addr::new(192, 168, 1, 40))));
        assert!(!is_wsl_nat(true, Some(Ipv4Addr::new(10, 0, 0, 5))));
        // 172.15 and 172.32 are outside the private /12, so not the NAT range.
        assert!(!is_wsl_nat(true, Some(Ipv4Addr::new(172, 15, 0, 1))));
        assert!(!is_wsl_nat(true, Some(Ipv4Addr::new(172, 32, 0, 1))));
        // No route at all — cannot claim NAT mode.
        assert!(!is_wsl_nat(true, None));
    }

    #[test]
    fn mount_classifier() {
        assert_eq!(classify_mount(Path::new("/mnt/c/work/x")), {
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
        // Use a real temp dir so the read-only probe succeeds; the DrvFs mount
        // kind is injected (the classifier is tested separately). A DrvFs
        // classification alone must produce a Warn with an action, independent
        // of the actual filesystem under the probe.
        let dir = tempfile::tempdir().unwrap();
        let s = filesystem_section(dir.path(), MountKind::DrvFs);
        assert_eq!(s.verdict, Verdict::Warn, "{:?}", s.lines);
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
