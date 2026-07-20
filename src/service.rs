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
         ExecStart=\"{exe}\" start --dir \"{dir}\" --log-file\n\
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
        <string>--log-file</string>
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
//
// The task is created with the `ScheduledTasks` PowerShell cmdlets, NOT
// `schtasks.exe`: the latter's `/Create /SC ONLOGON` returns ERROR_ACCESS_DENIED
// for a non-elevated user, while `Register-ScheduledTask -RunLevel Limited`
// succeeds for the current user without elevation (verified on the target
// machine). Values are passed as single-quoted PowerShell literals with
// embedded quotes doubled, so a path can never break out of its string.

/// Escapes a string for a single-quoted PowerShell literal.
#[cfg(windows)]
fn ps_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Runs a PowerShell script, returning stdout on success.
#[cfg(windows)]
fn powershell(script: &str) -> Result<String, ServiceError> {
    run_ok(
        Command::new("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]),
        "powershell",
    )
}

#[cfg(windows)]
pub fn install(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let abs = std::path::absolute(dir)?;
    let exe = current_exe()?;
    let (exe_s, dir_s) = (exe.display().to_string(), abs.display().to_string());
    // A single quote in either path would need a third escaping layer; reject
    // it rather than risk a malformed task (Windows user paths never have one).
    if exe_s.contains('\'') || dir_s.contains('\'') {
        return Err(ServiceError::Failed(
            "binary or folder path contains a single quote; unsupported for the service task"
                .into(),
        ));
    }
    // The action runs the exe through a hidden PowerShell host so no console
    // window flashes at logon. The inner command single-quotes the exe/dir; the
    // outer `ps_lit` doubles those quotes so the task stores them correctly.
    let inner = format!("& '{exe_s}' start --dir '{dir_s}' --log-file");
    let argument = format!("-NoProfile -WindowStyle Hidden -Command \"{inner}\"");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument {arg}; \
         $t = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME; \
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero); \
         Register-ScheduledTask -TaskName {name} -Action $a -Trigger $t -Settings $s -RunLevel Limited -Force | Out-Null; \
         'ok'",
        arg = ps_lit(&argument),
        name = ps_lit(&name),
    );
    powershell(&script)?;
    Ok(format!(
        "installed Scheduled Task {name} (starts at logon; start now with: schtasks /Run /TN {name})"
    ))
}

#[cfg(windows)]
pub fn uninstall(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let script = format!(
        "Unregister-ScheduledTask -TaskName {name} -Confirm:$false -ErrorAction SilentlyContinue; 'ok'",
        name = ps_lit(&name),
    );
    powershell(&script)?;
    Ok(format!("removed Scheduled Task {name}"))
}

#[cfg(windows)]
pub fn platform_status(dir: &Path) -> Result<String, ServiceError> {
    let name = instance_name(dir);
    let script = format!(
        "$t = Get-ScheduledTask -TaskName {name} -ErrorAction SilentlyContinue; \
         if (-not $t) {{ 'not installed' }} else {{ \
           $i = $t | Get-ScheduledTaskInfo; \
           \"state: $($t.State)\"; \"last run: $($i.LastRunTime)\"; \"last result: $($i.LastTaskResult)\" }}",
        name = ps_lit(&name),
    );
    // Status is best-effort: a query failure still yields a useful line.
    Ok(powershell(&script).unwrap_or_else(|e| format!("{name}: status query failed ({e})")))
}

// ── P16: one device-wide supervisor service (hosts every folder) ────────────
//
// A single autostart entry that runs `tazamun start --all`, so one process and
// one OS service serve every registered session instead of one unit per folder.
// The per-folder `install`/`uninstall` above stay for scripts and for anyone
// who wants an isolated unit; `--all` targets this one. Pure renderers are
// unit-tested; only install/uninstall/status shell out.

/// Fixed instance name for the device-wide supervisor service.
pub fn supervisor_name() -> &'static str {
    "tazamun-supervisor"
}

/// launchd label for the supervisor (reverse-DNS, like the per-folder labels).
pub fn supervisor_label() -> &'static str {
    "io.tazamun.supervisor"
}

/// Renders the systemd user unit for the supervisor (pure; unit-tested). No
/// `--dir`, no `--log-file`: it hosts every folder and its stdout is captured
/// by journald (`journalctl --user -u tazamun-supervisor`); each hosted session
/// still writes its own per-folder log.
pub fn systemd_supervisor_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=tazamun supervisor (all registered folders)\n\
         After=network.target\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=3\n\
         \n\
         [Service]\n\
         ExecStart=\"{exe}\" start --all\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display(),
    )
}

/// Renders the supervisor LaunchAgent plist (pure; unit-tested). launchd
/// captures stdout/stderr into `logs`.
pub fn launch_agent_supervisor_plist(exe: &Path, logs: &Path) -> String {
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
        <string>--all</string>
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
        label = supervisor_label(),
        exe = exe.display(),
        out = logs.join("supervisor.out.log").display(),
        err = logs.join("supervisor.err.log").display(),
    )
}

#[cfg(target_os = "linux")]
pub fn install_supervisor() -> Result<String, ServiceError> {
    let name = supervisor_name();
    let unit = unit_path(name)?;
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&unit, systemd_supervisor_unit(&current_exe()?))?;
    run_ok(
        Command::new("systemctl").args(["--user", "daemon-reload"]),
        "systemctl daemon-reload",
    )?;
    run_ok(
        Command::new("systemctl").args(["--user", "enable", "--now", name]),
        "systemctl enable --now",
    )?;
    Ok(format!(
        "installed device-wide supervisor unit {name} ({})\n\
         it hosts every registered folder in one process.\n\
         note: to run while logged out, enable lingering once: loginctl enable-linger $USER",
        unit.display()
    ))
}

#[cfg(target_os = "linux")]
pub fn uninstall_supervisor() -> Result<String, ServiceError> {
    let name = supervisor_name();
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", name])
        .output();
    let unit = unit_path(name)?;
    if unit.exists() {
        std::fs::remove_file(&unit)?;
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    Ok(format!("removed device-wide supervisor unit {name}"))
}

#[cfg(target_os = "linux")]
pub fn supervisor_status() -> Result<String, ServiceError> {
    let name = supervisor_name();
    let out = Command::new("systemctl")
        .args(["--user", "--no-pager", "status", name])
        .output()
        .map_err(ServiceError::Io)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The device-wide log directory for the supervisor service (`~/Library/Logs/
/// tazamun` on macOS).
#[cfg(target_os = "macos")]
fn supervisor_logs() -> Result<PathBuf, ServiceError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ServiceError::Failed("HOME is not set".into()))?;
    Ok(home.join("Library").join("Logs").join("tazamun"))
}

#[cfg(target_os = "macos")]
pub fn install_supervisor() -> Result<String, ServiceError> {
    let label = supervisor_label();
    let logs = supervisor_logs()?;
    std::fs::create_dir_all(&logs)?;
    let plist = plist_path(label)?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &plist,
        launch_agent_supervisor_plist(&current_exe()?, &logs),
    )?;
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
        "installed device-wide supervisor LaunchAgent {label} ({}); it hosts every folder",
        plist.display()
    ))
}

#[cfg(target_os = "macos")]
pub fn uninstall_supervisor() -> Result<String, ServiceError> {
    let label = supervisor_label();
    let domain = gui_domain();
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .output();
    let plist = plist_path(label)?;
    if plist.exists() {
        std::fs::remove_file(&plist)?;
    }
    Ok(format!(
        "removed device-wide supervisor LaunchAgent {label}"
    ))
}

#[cfg(target_os = "macos")]
pub fn supervisor_status() -> Result<String, ServiceError> {
    let label = supervisor_label();
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

#[cfg(windows)]
pub fn install_supervisor() -> Result<String, ServiceError> {
    let name = supervisor_name();
    let exe = current_exe()?;
    let exe_s = exe.display().to_string();
    if exe_s.contains('\'') {
        return Err(ServiceError::Failed(
            "binary path contains a single quote; unsupported for the service task".into(),
        ));
    }
    let inner = format!("& '{exe_s}' start --all");
    let argument = format!("-NoProfile -WindowStyle Hidden -Command \"{inner}\"");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument {arg}; \
         $t = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME; \
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero); \
         Register-ScheduledTask -TaskName {name} -Action $a -Trigger $t -Settings $s -RunLevel Limited -Force | Out-Null; \
         'ok'",
        arg = ps_lit(&argument),
        name = ps_lit(name),
    );
    powershell(&script)?;
    Ok(format!(
        "installed device-wide supervisor Scheduled Task {name} (hosts every folder; starts at logon)"
    ))
}

#[cfg(windows)]
pub fn uninstall_supervisor() -> Result<String, ServiceError> {
    let name = supervisor_name();
    let script = format!(
        "Unregister-ScheduledTask -TaskName {name} -Confirm:$false -ErrorAction SilentlyContinue; 'ok'",
        name = ps_lit(name),
    );
    powershell(&script)?;
    Ok(format!(
        "removed device-wide supervisor Scheduled Task {name}"
    ))
}

#[cfg(windows)]
pub fn supervisor_status() -> Result<String, ServiceError> {
    let name = supervisor_name();
    let script = format!(
        "$t = Get-ScheduledTask -TaskName {name} -ErrorAction SilentlyContinue; \
         if (-not $t) {{ 'not installed' }} else {{ \
           $i = $t | Get-ScheduledTaskInfo; \
           \"state: $($t.State)\"; \"last run: $($i.LastRunTime)\"; \"last result: $($i.LastTaskResult)\" }}",
        name = ps_lit(name),
    );
    Ok(powershell(&script).unwrap_or_else(|e| format!("{name}: status query failed ({e})")))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn install_supervisor() -> Result<String, ServiceError> {
    Err(ServiceError::Failed(
        "service install is not supported on this platform".into(),
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn uninstall_supervisor() -> Result<String, ServiceError> {
    Err(ServiceError::Failed(
        "service uninstall is not supported on this platform".into(),
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn supervisor_status() -> Result<String, ServiceError> {
    Err(ServiceError::Failed(
        "service status is not supported on this platform".into(),
    ))
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

/// The last `n` lines of the per-session daemon log, if any.
pub fn log_tail(dir: &Path, n: usize) -> Option<Vec<String>> {
    let path = crate::state::log_file_path(dir);
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let start = lines.len().saturating_sub(n);
    Some(lines[start..].to_vec())
}

// ── Line-capped daemon log ──────────────────────────────────────────────────

/// A `tracing` writer that appends to the per-session OS log file
/// ([`crate::state::log_file_path`]) and keeps only the most recent
/// `max_lines` lines: once the file overshoots the cap it is rewritten to its
/// tail, so old noise is dropped and the log self-bounds without external
/// rotation. Small and in-crate on purpose — the rolling appenders rotate by
/// time or size, not lines, and pull in more surface (rationale in DECISIONS.md).
#[derive(Clone)]
pub struct LineCappedLog {
    inner: Arc<Mutex<LineCappedInner>>,
}

struct LineCappedInner {
    path: PathBuf,
    max_lines: usize,
    file: Option<std::fs::File>,
    lines: usize,
}

/// Trim only after overshooting the cap by `max_lines / this`, so the
/// whole-file rewrite is amortized over many writes instead of running on
/// every line past the cap.
const LOG_TRIM_SLACK_DIV: usize = 5;

impl LineCappedLog {
    /// Opens the per-session OS log file ([`crate::state::log_file_path`]).
    pub fn open(dir: &Path, max_lines: usize) -> std::io::Result<Self> {
        Self::open_at(crate::state::log_file_path(dir), max_lines)
    }

    /// Opens (creating parents) a line-capped log at an explicit path, seeding
    /// the line counter from any existing content. Used by [`Self::open`] and
    /// exercised directly by the cap test.
    pub fn open_at(path: PathBuf, max_lines: usize) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let lines = std::fs::read_to_string(&path)
            .map(|t| t.lines().count())
            .unwrap_or(0);
        Ok(Self {
            inner: Arc::new(Mutex::new(LineCappedInner {
                path,
                max_lines: max_lines.max(1),
                file: Some(file),
                lines,
            })),
        })
    }
}

impl LineCappedInner {
    /// Rewrites the log keeping only its last `max_lines` lines, via a temp
    /// file + rename so a crash mid-trim never truncates the live log.
    fn trim(&mut self) -> std::io::Result<()> {
        self.file = None;
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(self.max_lines);
        let kept = &all[start..];
        let tmp = self.path.with_extension("log.tmp");
        {
            // Buffered: the tail can be tens of thousands of lines (the audit
            // log reuses this), so one write batch, not a syscall per line.
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            for line in kept {
                writeln!(f, "{line}")?;
            }
            f.flush()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        self.file = Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.lines = kept.len();
        Ok(())
    }
}

impl Write for LineCappedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("log lock poisoned"))?;
        let n = {
            let file = inner
                .file
                .as_mut()
                .ok_or_else(|| std::io::Error::other("log file closed"))?;
            file.write(buf)?
        };
        inner.lines += buf[..n].iter().filter(|&&b| b == b'\n').count();
        let ceiling = inner.max_lines + inner.max_lines / LOG_TRIM_SLACK_DIV;
        if inner.lines > ceiling {
            inner.trim()?;
        }
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

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LineCappedLog {
    type Writer = LineCappedLog;
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

    // The plist only ever renders on macOS; on Windows `Path::join` would
    // backslash the joined log paths and the byte-for-byte golden could never
    // match. Unix-only keeps the comparison honest for the platform it serves.
    #[cfg(not(windows))]
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
        <string>--log-file</string>
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
            "ExecStart=\"/opt/tazamun\" start --dir \"/data/proj\" --log-file",
            "Restart=on-failure",
            "WantedBy=default.target",
            "StartLimitBurst=3",
        ] {
            assert!(unit.contains(needle), "missing {needle:?} in:\n{unit}");
        }
    }

    #[test]
    fn supervisor_unit_hosts_all_folders_no_dir() {
        let unit = systemd_supervisor_unit(Path::new("/opt/tazamun"));
        // The supervisor runs `start --all` with no --dir and no --log-file.
        assert!(unit.contains("ExecStart=\"/opt/tazamun\" start --all\n"));
        assert!(!unit.contains("--dir"), "supervisor must not pin a folder");
        assert!(!unit.contains("--log-file"), "journald captures stdout");
        assert!(unit.contains("Restart=on-failure"));
        assert_eq!(supervisor_name(), "tazamun-supervisor");
    }

    #[cfg(not(windows))]
    #[test]
    fn supervisor_plist_matches_golden() {
        let got = launch_agent_supervisor_plist(
            Path::new("/usr/local/bin/tazamun"),
            Path::new("/Users/u/Library/Logs/tazamun"),
        );
        let golden = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.tazamun.supervisor</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/tazamun</string>
        <string>start</string>
        <string>--all</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/Users/u/Library/Logs/tazamun/supervisor.out.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/u/Library/Logs/tazamun/supervisor.err.log</string>
</dict>
</plist>
"#;
        assert_eq!(got, golden, "supervisor plist must stay byte-stable");
    }

    #[test]
    fn line_capped_log_keeps_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let cap = 100usize;
        let mut log = LineCappedLog::open_at(path.clone(), cap).unwrap();
        // Write far past the cap so at least one trim runs.
        for i in 0..1000 {
            log.write_all(format!("line {i:04}\n").as_bytes()).unwrap();
        }
        log.flush().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Bounded to the cap plus the slack ceiling — never the full 1000.
        assert!(
            lines.len() <= cap + cap / LOG_TRIM_SLACK_DIV,
            "log not capped: {} lines",
            lines.len()
        );
        // The newest line survives; the oldest is dropped.
        assert!(text.contains("line 0999"), "newest line missing");
        assert!(!text.contains("line 0000"), "oldest line should be trimmed");
        // Re-opening seeds the counter from the trimmed file (no reset blowup).
        let reopened = LineCappedLog::open_at(path.clone(), cap).unwrap();
        assert!(reopened.inner.lock().unwrap().lines <= cap + cap / LOG_TRIM_SLACK_DIV);
    }

    #[test]
    fn log_tail_returns_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        // Writer and log_tail both derive the same per-session OS path; the
        // tempdir's unique path yields a unique hash, so this never collides
        // with a real session. Removed at the end to avoid leaving a stray file.
        let mut log = LineCappedLog::open(dir.path(), 10_000).unwrap();
        for i in 0..9 {
            writeln!(log, "line-{i}").unwrap();
        }
        log.flush().unwrap();
        let tail = log_tail(dir.path(), 5).expect("log exists");
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0], "line-4");
        assert_eq!(tail[4], "line-8");
        let _ = std::fs::remove_file(crate::state::log_file_path(dir.path()));
    }
}
