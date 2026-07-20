//! P19 git-style hooks: run a user executable on a session event.
//!
//! An executable at `.tazamun/hooks/<event>` (e.g. `on-sync`, `on-conflict`,
//! `on-lock-denied`, `on-peer-offline`) is invoked with a one-line JSON event
//! on stdin. Hooks are **fire-and-forget** and can never block or slow the sync
//! path: [`fire`] spawns the child off the daemon actor (on the blocking pool)
//! and returns immediately; a hung or hostile hook is killed after
//! [`HOOK_TIMEOUT`](crate::consts::HOOK_TIMEOUT). Output is discarded. There is
//! no hook if the file is absent or (on Unix) not executable — so the feature
//! costs nothing until a user drops a script in.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// The hooks directory for a session folder.
pub fn hooks_dir(dir: &Path) -> PathBuf {
    crate::state::AppState::meta_dir(dir).join("hooks")
}

/// The path of a named hook.
pub fn hook_path(dir: &Path, event: &str) -> PathBuf {
    hooks_dir(dir).join(event)
}

/// Whether a runnable hook exists for `event`: a regular file that, on Unix,
/// has an execute bit set. On Windows executability is by extension, so any
/// present file counts.
pub fn exists(dir: &Path, event: &str) -> bool {
    let p = hook_path(dir, event);
    let Ok(meta) = std::fs::metadata(&p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Fires the `event` hook with `payload` as one-line JSON on stdin, off the
/// actor. No-op when the hook is absent. Never blocks the caller.
pub fn fire(dir: &Path, event: &'static str, payload: serde_json::Value) {
    if !exists(dir, event) {
        return;
    }
    let path = hook_path(dir, event);
    let root = dir.to_path_buf();
    let mut body = serde_json::to_vec(&payload).unwrap_or_default();
    body.push(b'\n');
    tokio::task::spawn_blocking(move || run_hook(&path, &root, body));
}

/// Runs one hook to completion or timeout, discarding its output. Killing on
/// overrun bounds a hung/malicious hook. All errors are swallowed — a hook can
/// never surface a failure into the sync path.
fn run_hook(path: &Path, root: &Path, stdin_bytes: Vec<u8>) {
    let Ok(mut child) = Command::new(path)
        // Run from the synced folder root, so a hook's relative file ops land
        // where the user expects (not inside `.tazamun`).
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // Feed stdin from a detached thread so a hook that keeps its read end open
    // without draining (an over-64-KiB payload would otherwise block a blocking
    // pool thread forever) cannot defeat the timeout: the deadline loop below
    // still kills the child, which closes the pipe and unblocks the writer.
    let stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            let _ = s.write_all(&stdin_bytes);
            // s drops here → EOF to the hook.
        }
    });
    let deadline = Instant::now() + crate::consts::HOOK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    // The writer exits once the hook drains stdin or the killed child closes the
    // pipe (EPIPE), so this join never hangs.
    let _ = writer.join();
}

/// The hook name for an audit `kind`, or `None` if this kind fires no hook.
pub fn hook_for(kind: &str) -> Option<&'static str> {
    match kind {
        "pull-applied" => Some("on-sync"),
        "quarantine" => Some("on-conflict"),
        "lock-denied" => Some("on-lock-denied"),
        "peer-offline" => Some("on-peer-offline"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_hook_is_a_noop_and_kind_mapping_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!exists(dir.path(), "on-sync"));
        assert_eq!(hook_for("pull-applied"), Some("on-sync"));
        assert_eq!(hook_for("quarantine"), Some("on-conflict"));
        assert_eq!(hook_for("lock-denied"), Some("on-lock-denied"));
        assert_eq!(hook_for("peer-offline"), Some("on-peer-offline"));
        assert_eq!(hook_for("lock"), None);
        assert_eq!(hook_for("publish"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_hook_receives_the_json_on_stdin() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let hooks = hooks_dir(dir.path());
        std::fs::create_dir_all(&hooks).unwrap();
        let out = dir.path().join("hook-out.txt");
        // A hook that copies its stdin to a witness file.
        let script = format!("#!/bin/sh\ncat > '{}'\n", out.display());
        let hook = hooks.join("on-sync");
        std::fs::write(&hook, script).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(exists(dir.path(), "on-sync"));

        fire(
            dir.path(),
            "on-sync",
            serde_json::json!({"event":"on-sync","path":"a.txt"}),
        );
        // Give the blocking task time to run the hook.
        for _ in 0..100 {
            if out.exists() && !std::fs::read(&out).unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let got = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(got.trim()).unwrap();
        assert_eq!(v["event"], "on-sync");
        assert_eq!(v["path"], "a.txt");
    }
}
