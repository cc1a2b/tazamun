//! macOS LaunchAgent lifecycle, exercised only by the hosted `macos-full`
//! dispatch. Best-effort by design: CI sessions frequently refuse
//! `launchctl bootstrap` into the GUI domain, and that refusal must not fail
//! the suite — the plist write/removal is asserted strictly, the live
//! bootstrap is logged and tolerated.

#![cfg(target_os = "macos")]

use std::path::PathBuf;

fn agents_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME set"))
        .join("Library")
        .join("LaunchAgents")
}

#[test]
fn launch_agent_install_writes_plist_and_uninstall_removes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A real session folder: service install refuses folders without one.
    tazamun::cli::init(dir.path()).expect("init");

    let label = tazamun::service::launchd_label(dir.path());
    let plist = agents_dir().join(format!("{label}.plist"));

    match tazamun::service::install(dir.path()) {
        Ok(msg) => eprintln!("install: {msg}"),
        Err(e) => {
            // Best-effort: `launchctl bootstrap`/`load` may be refused in a CI
            // session (no GUI domain). The plist must exist regardless if the
            // failure came from launchctl rather than the filesystem.
            eprintln!("install reported: {e} (tolerated if the plist landed)");
        }
    }
    assert!(
        plist.is_file(),
        "LaunchAgent plist must be written at {}",
        plist.display()
    );
    let body = std::fs::read_to_string(&plist).expect("read plist");
    assert!(body.contains(&label), "label present in plist");
    assert!(body.contains("RunAtLoad"), "RunAtLoad present");

    // Live check, best-effort: bootstrap may have been refused — log either way.
    let out = std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{}/{}", unsafe_uid(), label)])
        .output()
        .expect("launchctl runs");
    eprintln!(
        "launchctl print: status={} ({} bytes stdout)",
        out.status,
        out.stdout.len()
    );

    let msg = tazamun::service::uninstall(dir.path()).expect("uninstall");
    eprintln!("uninstall: {msg}");
    assert!(!plist.exists(), "plist removed on uninstall");
}

fn unsafe_uid() -> String {
    String::from_utf8_lossy(
        &std::process::Command::new("id")
            .arg("-u")
            .output()
            .expect("id -u")
            .stdout,
    )
    .trim()
    .to_string()
}
