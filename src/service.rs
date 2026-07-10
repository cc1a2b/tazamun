//! Background service management: one OS-native autostart entry per session
//! folder, plus the size-rotated daemon log used when running unattended.
//!
//! Invariant: one session folder = one instance, named
//! `tazamun-<8 hex of blake3(absolute folder path)>` on every OS, so repeated
//! installs are idempotent and two folders never collide. Backends are thin
//! wrappers over the platform's own facility — systemd user units (Linux),
//! LaunchAgents (macOS), logon Scheduled Tasks (Windows) — never a custom
//! supervisor. Plist/unit generation is pure and unit-tested; only the
//! `install`/`uninstall`/`status` entry points shell out.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Stable instance name for a session folder.
pub fn instance_name(dir: &Path) -> String {
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = blake3::hash(abs.to_string_lossy().as_bytes());
    let hex = data_encoding::HEXLOWER.encode(&digest.as_bytes()[..4]);
    format!("tazamun-{hex}")
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("service io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Failed(String),
}

fn run_ok(cmd: &mut Command, what: &str) -> Result<String, ServiceError> {
    let out = cmd.output().map_err(ServiceError::Io)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(ServiceError::Failed(format!(
            "{what} failed ({}): {}{}",
            out.status,
            stdout,
            String::from_utf8_lossy(&out.stderr)
        )))
    }
}

/// The absolute path of the running tazamun binary (what the service runs).
fn current_exe() -> Result<PathBuf, ServiceError> {
    std::env::current_exe().map_err(ServiceError::Io)
}

// ── Linux: systemd user unit ────────────────────────────────────────────────

/// Renders the systemd user unit (pure; unit-tested against a golden file).
pub fn systemd_unit(exe: &Path, dir: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=tazamun sync daemon ({dir})\n\
         After=network.target\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=3\n\
         \n\
         [Service]\n\
         ExecStart=\"{exe}\" start --dir \"{dir}\"\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display(),
        dir = dir.display(),
    )
}

#[cfg(target_os = "linux")]
fn unit_path(name: &str) -> Result<PathBuf, ServiceError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ServiceError::Failed("HOME is not set".into()))?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{name}.service")))
}

#[cfg(target_os = "linux")]
pub fn install(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let unit = unit_path(&name)?;
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let abs = std::path::absolute(dir)?;
    std::fs::write(&unit, systemd_unit(&current_exe()?, &abs))?;
    run_ok(
        Command::new("systemctl").args(["--user", "daemon-reload"]),
        "systemctl daemon-reload",
    )?;
    run_ok(
        Command::new("systemctl").args(["--user", "enable", "--now", &name]),
        "systemctl enable --now",
    )?;
    Ok(format!(
        "installed systemd user unit {name} ({})\n\
         note: for the service to run while you are logged out, enable\n\
         lingering once: loginctl enable-linger $USER",
        unit.display()
    ))
}

#[cfg(target_os = "linux")]
pub fn uninstall(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    // Best-effort stop/disable; removing the unit file is the authoritative act.
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", &name])
        .output();
    let unit = unit_path(&name)?;
    if unit.exists() {
        std::fs::remove_file(&unit)?;
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    Ok(format!("removed systemd user unit {name}"))
}

#[cfg(target_os = "linux")]
pub fn platform_status(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let out = Command::new("systemctl")
        .args(["--user", "--no-pager", "status", &name])
        .output()
        .map_err(ServiceError::Io)?;
    // `systemctl status` exits non-zero for inactive units; the text is still
    // the answer.
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── macOS: LaunchAgent ──────────────────────────────────────────────────────

/// Renders the LaunchAgent plist (pure; unit-tested against a golden file).
/// RunAtLoad starts it at login; KeepAlive/SuccessfulExit=false restarts it
/// only after failures (launchd throttles restarts to ~10s intervals).
pub fn launch_agent_plist(label: &str, exe: &Path, dir: &Path, logs: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>start</string>
        <string>--dir</string>
        <string>{dir}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#,
        label = label,
        exe = exe.display(),
        dir = dir.display(),
        out = logs.join("launchd.out.log").display(),
        err = logs.join("launchd.err.log").display(),
    )
}

/// The launchd label for a session folder.
pub fn launchd_label(dir: &Path) -> String {
    // instance_name is "tazamun-<hex8>"; the label wants reverse-DNS.
    format!("io.tazamun.{}", &instance_name(dir)["tazamun-".len()..])
}

#[cfg(target_os = "macos")]
fn plist_path(label: &str) -> Result<PathBuf, ServiceError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ServiceError::Failed("HOME is not set".into()))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn gui_domain() -> String {
    // launchctl's per-user GUI domain: gui/<uid>.
    let uid = run_ok(Command::new("id").arg("-u"), "id -u")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "501".to_string());
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
pub fn install(dir: &Path) -> Result<String, ServiceError> {
    let label = launchd_label(dir);
    let abs = std::path::absolute(dir)?;
    let logs = crate::state::logs_dir(&abs);
    std::fs::create_dir_all(&logs)?;
    let plist = plist_path(&label)?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &plist,
        launch_agent_plist(&label, &current_exe()?, &abs, &logs),
    )?;
    // Modern bootstrap first; fall back to the legacy loader on refusal.
    let domain = gui_domain();
    let boot = Command::new("launchctl")
        .args(["bootstrap", &domain])
        .arg(&plist)
        .output()
        .map_err(ServiceError::Io)?;
    if !boot.status.success() {
        run_ok(
            Command::new("launchctl").args(["load", "-w"]).arg(&plist),
            "launchctl load -w",
        )?;
    }
    Ok(format!(
        "installed LaunchAgent {label} ({})",
        plist.display()
    ))
}

#[cfg(target_os = "macos")]
pub fn uninstall(dir: &Path) -> Result<String, ServiceError> {
    let label = launchd_label(dir);
    let domain = gui_domain();
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .output();
    let plist = plist_path(&label)?;
    if plist.exists() {
        std::fs::remove_file(&plist)?;
    }
    Ok(format!("removed LaunchAgent {label}"))
}

#[cfg(target_os = "macos")]
pub fn platform_status(dir: &Path) -> Result<String, ServiceError> {
    let label = launchd_label(dir);
    let out = Command::new("launchctl")
        .args(["print", &format!("{}/{label}", gui_domain())])
        .output()
        .map_err(ServiceError::Io)?;
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text.lines().take(12).collect::<Vec<_>>().join("\n"))
    } else {
        Ok(format!("{label}: not loaded"))
    }
}

// ── Windows: logon Scheduled Task ───────────────────────────────────────────
//
// A Scheduled Task (ONLOGON, limited run level) rather than a Windows service:
// it runs in the user session with the user's environment and needs no
// elevation or stored password. The action goes through a hidden PowerShell
// host — tradeoff: a brief hidden powershell.exe wrapper process exists purely
// to suppress the console window a bare exe would flash at logon.

#[cfg(windows)]
pub fn install(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let abs = std::path::absolute(dir)?;
    let exe = current_exe()?;
    let action = format!(
        "powershell.exe -NoProfile -WindowStyle Hidden -Command \"& '{}' start --dir '{}'\"",
        exe.display(),
        abs.display()
    );
    run_ok(
        Command::new("schtasks").args([
            "/Create", "/F", "/SC", "ONLOGON", "/RL", "LIMITED", "/TN", &name, "/TR", &action,
        ]),
        "schtasks /Create",
    )?;
    Ok(format!(
        "installed Scheduled Task {name} (starts at logon; start now with: schtasks /Run /TN {name})"
    ))
}

#[cfg(windows)]
pub fn uninstall(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    run_ok(
        Command::new("schtasks").args(["/Delete", "/F", "/TN", &name]),
        "schtasks /Delete",
    )?;
    Ok(format!("removed Scheduled Task {name}"))
}

#[cfg(windows)]
pub fn platform_status(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let out = Command::new("schtasks")
        .args(["/Query", "/TN", &name, "/FO", "LIST", "/V"])
        .output()
        .map_err(ServiceError::Io)?;
    if !out.status.success() {
        return Ok(format!("{name}: not installed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            l.starts_with("TaskName") || l.starts_with("Status") || l.starts_with("Last Run")
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

// Unsupported platforms compile but explain themselves.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn install(_dir: &Path) -> Result<String, ServiceError> {
    Err(ServiceError::Failed(
        "service install is not supported on this platform".into(),
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn uninstall(_dir: &Path) -> Result<String, ServiceError> {
    Err(ServiceError::Failed(
        "service uninstall is not supported on this platform".into(),
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn platform_status(_dir: &Path) -> Result<String, ServiceError> {
    Err(ServiceError::Failed(
        "service status is not supported on this platform".into(),
    ))
}

/// The last `n` lines of the rotated daemon log, if any.
pub fn log_tail(dir: &Path, n: usize) -> Option<Vec<String>> {
    let path = crate::state::logs_dir(dir).join("daemon.log");
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let start = lines.len().saturating_sub(n);
    Some(lines[start..].to_vec())
}

// ── Size-rotated daemon log ─────────────────────────────────────────────────

/// A `tracing` writer that appends to `logs/daemon.log`, rotating at
/// `max_bytes` and keeping `keep` old generations (`daemon.log.1` newest …
/// `daemon.log.<keep>` oldest). Small and in-crate on purpose: the external
/// rolling appenders rotate by time, not size, and pull in more surface than
/// forty lines of rename logic (rationale in DECISIONS.md).
#[derive(Clone)]
pub struct RotatingLog {
    inner: Arc<Mutex<RotatingInner>>,
}

struct RotatingInner {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<std::fs::File>,
    written: u64,
}

impl RotatingLog {
    pub fn open(dir: &Path, max_bytes: u64, keep: usize) -> std::io::Result<Self> {
        let logs = crate::state::logs_dir(dir);
        std::fs::create_dir_all(&logs)?;
        let path = logs.join("daemon.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            inner: Arc::new(Mutex::new(RotatingInner {
                path,
                max_bytes,
                keep,
                file: Some(file),
                written,
            })),
        })
    }
}

impl RotatingInner {
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        // daemon.log.(keep-1) → daemon.log.keep … daemon.log → daemon.log.1
        for i in (1..self.keep).rev() {
            let from = self.path.with_extension(format!("log.{i}"));
            let to = self.path.with_extension(format!("log.{}", i + 1));
            if from.exists() {
                let _ = std::fs::rename(&from, &to);
            }
        }
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        self.file = Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("log lock poisoned"))?;
        if inner.written + buf.len() as u64 > inner.max_bytes {
            inner.rotate()?;
        }
        let Some(file) = inner.file.as_mut() else {
            return Err(std::io::Error::other("log file closed"));
        };
        let n = file.write(buf)?;
        inner.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("log lock poisoned"))?;
        if let Some(file) = inner.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingLog {
    type Writer = RotatingLog;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_name_is_stable_and_distinct() {
        let a = instance_name(Path::new("/sessions/alpha"));
        let b = instance_name(Path::new("/sessions/beta"));
        assert!(a.starts_with("tazamun-") && a.len() == "tazamun-".len() + 8);
        assert_ne!(a, b, "different folders get different names");
        assert_eq!(a, instance_name(Path::new("/sessions/alpha")), "stable");
    }

    #[test]
    fn launch_agent_plist_matches_golden() {
        let got = launch_agent_plist(
            "io.tazamun.deadbeef",
            Path::new("/usr/local/bin/tazamun"),
            Path::new("/Users/u/project"),
            Path::new("/Users/u/project/.tazamun/logs"),
        );
        let golden = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.tazamun.deadbeef</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/tazamun</string>
        <string>start</string>
        <string>--dir</string>
        <string>/Users/u/project</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/Users/u/project/.tazamun/logs/launchd.out.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/u/project/.tazamun/logs/launchd.err.log</string>
</dict>
</plist>
"#;
        assert_eq!(got, golden, "plist generation must stay byte-stable");
    }

    #[test]
    fn systemd_unit_contains_the_load_bearing_lines() {
        let unit = systemd_unit(Path::new("/opt/tazamun"), Path::new("/data/proj"));
        for needle in [
            "ExecStart=\"/opt/tazamun\" start --dir \"/data/proj\"",
            "Restart=on-failure",
            "WantedBy=default.target",
            "StartLimitBurst=3",
        ] {
            assert!(unit.contains(needle), "missing {needle:?} in:\n{unit}");
        }
    }

    #[test]
    fn rotating_log_rotates_and_keeps_three() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::open(dir.path(), 100, 3).unwrap();
        // Each record is 40 bytes; 100-byte cap → rotations as we exceed it.
        for i in 0..12 {
            let line = format!("{i:02} {}\n", "x".repeat(36));
            log.write_all(line.as_bytes()).unwrap();
        }
        log.flush().unwrap();
        let logs = crate::state::logs_dir(dir.path());
        assert!(logs.join("daemon.log").exists());
        assert!(logs.join("daemon.log.1").exists());
        assert!(logs.join("daemon.log.2").exists());
        assert!(logs.join("daemon.log.3").exists());
        assert!(
            !logs.join("daemon.log.4").exists(),
            "keep=3 must drop older generations"
        );
        // The newest line is in the live file.
        let live = std::fs::read_to_string(logs.join("daemon.log")).unwrap();
        assert!(live.contains("11 "), "latest record in live log: {live}");
    }

    #[test]
    fn log_tail_returns_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::open(dir.path(), 10_000, 3).unwrap();
        for i in 0..9 {
            writeln!(log, "line-{i}").unwrap();
        }
        let tail = log_tail(dir.path(), 5).expect("log exists");
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0], "line-4");
        assert_eq!(tail[4], "line-8");
    }
}
