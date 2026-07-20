//! Device-global session registry (P13).
//!
//! Every folder `init`/`join` turns into a tazamun session is recorded here,
//! in one small JSON file under the OS config directory, so the Home screen
//! and `tazamun sessions` can see and manage every session on the machine
//! without a daemon or a `--dir`. The registry stores only the folder path,
//! how it was created, and when; every other detail (peer id, file count,
//! running/stopped) is read live from each session's own `state.json`, so the
//! registry can never go stale on content — only on existence, which
//! [`Registry::prune`] repairs by dropping folders that are gone.
//!
//! It is advisory, never authoritative: losing it loses no data (each session
//! is fully self-describing in its own folder), and it is never on the sync
//! path, so a corrupt or missing file degrades to an empty list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a session came to exist on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// Created here with `tazamun init`.
    Init,
    /// Joined from someone else's invite.
    Join,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::Init => "init",
            SessionKind::Join => "join",
        }
    }
}

/// One registered session: just enough to find it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    /// Absolute folder path.
    pub path: String,
    pub kind: SessionKind,
    /// Epoch-ms the session was registered.
    pub added_ms: u64,
    /// P16: user-paused. A paused session is not brought up by the multi-session
    /// supervisor (`tazamun start --all`) and shows as paused in `tazamun ls`.
    /// Advisory and back-compatible: absent in older files reads as `false`.
    #[serde(default)]
    pub paused: bool,
}

/// The whole registry: a de-duplicated, path-sorted list.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub sessions: Vec<SessionRef>,
}

/// The OS-appropriate base directory for application configuration:
/// `%APPDATA%` on Windows, `~/Library/Application Support` on macOS,
/// `$XDG_CONFIG_HOME` (or `~/.config`) elsewhere, and the OS temp dir as a
/// last resort so a headless/minimal environment still has somewhere to write.
///
/// `TAZAMUN_CONFIG_DIR` overrides all of it. `.cargo/config.toml` sets that for
/// anything cargo launches, which is why it exists: `init` and `join` register
/// the session they create, so running the test suite wrote dead `/tmp` session
/// entries straight into the developer's own `sessions.json` — on every
/// contributor's machine, every run. An installed binary never sees the
/// variable and behaves exactly as before.
pub fn config_base() -> PathBuf {
    if let Some(p) = std::env::var_os("TAZAMUN_CONFIG_DIR")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(p) = std::env::var_os("APPDATA") {
            return PathBuf::from(p);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(p) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(p);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config");
        }
    }
    std::env::temp_dir()
}

/// The registry file path: `<config-base>/tazamun/sessions.json`.
pub fn registry_path() -> PathBuf {
    config_base().join("tazamun").join("sessions.json")
}

impl Registry {
    /// Loads the registry, or an empty one if it is missing/unreadable/corrupt
    /// (advisory: a bad file must never block a command).
    pub fn load() -> Self {
        Self::load_from(&registry_path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Writes the registry atomically (temp + rename) so a crash never tears it.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&registry_path())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Records `dir` (absolute) as a session, replacing any existing entry for
    /// the same path (idempotent re-init/join) and keeping the list sorted. An
    /// existing paused flag is preserved across re-registration.
    pub fn register(&mut self, dir: &Path, kind: SessionKind, now_ms: u64) {
        let abs = absolute(dir);
        let paused = self
            .sessions
            .iter()
            .find(|s| s.path == abs)
            .map(|s| s.paused)
            .unwrap_or(false);
        self.sessions.retain(|s| s.path != abs);
        self.sessions.push(SessionRef {
            path: abs,
            kind,
            added_ms: now_ms,
            paused,
        });
        self.sessions.sort_by(|a, b| a.path.cmp(&b.path));
    }

    /// Sets the paused flag for `dir`. Returns `Some(previous)` if the session
    /// is registered, or `None` if there is no entry for the path.
    pub fn set_paused(&mut self, dir: &Path, paused: bool) -> Option<bool> {
        let abs = absolute(dir);
        let entry = self.sessions.iter_mut().find(|s| s.path == abs)?;
        let prev = entry.paused;
        entry.paused = paused;
        Some(prev)
    }

    /// Whether `dir` is registered and currently paused.
    pub fn is_paused(&self, dir: &Path) -> bool {
        let abs = absolute(dir);
        self.sessions.iter().any(|s| s.path == abs && s.paused)
    }

    /// Drops the entry for `dir`; returns whether one was removed.
    pub fn forget(&mut self, dir: &Path) -> bool {
        let abs = absolute(dir);
        let before = self.sessions.len();
        self.sessions.retain(|s| s.path != abs);
        self.sessions.len() != before
    }

    /// Drops entries whose folder is no longer a session on disk. Returns the
    /// paths pruned, so the caller can report them.
    pub fn prune(&mut self, is_session: impl Fn(&Path) -> bool) -> Vec<String> {
        let mut gone = Vec::new();
        self.sessions.retain(|s| {
            if is_session(Path::new(&s.path)) {
                true
            } else {
                gone.push(s.path.clone());
                false
            }
        });
        gone
    }
}

/// Convenience for the sync CLI paths: load, register (pruning dead entries
/// first), and save — best-effort, since the registry is advisory and its
/// failure must never fail an `init`/`join`.
pub fn register_session(dir: &Path, kind: SessionKind) {
    let mut reg = Registry::load();
    reg.prune(crate::state::AppState::exists);
    reg.register(dir, kind, crate::now_ms());
    let _ = reg.save();
}

/// Best-effort forget on session teardown.
pub fn forget_session(dir: &Path) -> bool {
    let mut reg = Registry::load();
    let removed = reg.forget(dir);
    let _ = reg.save();
    removed
}

fn absolute(dir: &Path) -> String {
    std::path::absolute(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    /// The whole point of `TAZAMUN_CONFIG_DIR`: under cargo it is set by
    /// `.cargo/config.toml`, so nothing the test suite registers can reach the
    /// developer's real `sessions.json`. If this ever fails, `cargo test` has
    /// started writing into `~/.config` again.
    #[test]
    fn cargo_runs_are_isolated_from_the_real_config_dir() {
        let base = super::config_base();
        let over = std::env::var_os("TAZAMUN_CONFIG_DIR")
            .expect("cargo sets TAZAMUN_CONFIG_DIR via .cargo/config.toml");
        assert_eq!(base, std::path::PathBuf::from(&over));
        assert!(
            super::registry_path().starts_with(&base),
            "the registry must live under the overridden base, not the real one"
        );
    }

    use super::*;

    #[test]
    fn register_is_idempotent_sorted_and_forgettable() {
        let mut r = Registry::default();
        r.register(Path::new("/b/two"), SessionKind::Init, 100);
        r.register(Path::new("/a/one"), SessionKind::Join, 200);
        // Re-register /b/two: replaces, does not duplicate.
        r.register(Path::new("/b/two"), SessionKind::Join, 300);
        assert_eq!(r.sessions.len(), 2);
        // Sorted by path.
        assert_eq!(r.sessions[0].path, "/a/one");
        assert_eq!(r.sessions[1].path, "/b/two");
        // The replacement took the newer kind/time.
        assert_eq!(r.sessions[1].kind, SessionKind::Join);
        assert_eq!(r.sessions[1].added_ms, 300);
        assert!(r.forget(Path::new("/a/one")));
        assert!(!r.forget(Path::new("/a/one")), "already gone");
        assert_eq!(r.sessions.len(), 1);
    }

    #[test]
    fn paused_flag_sets_and_survives_reregister() {
        let mut r = Registry::default();
        r.register(Path::new("/p/one"), SessionKind::Init, 1);
        assert!(!r.is_paused(Path::new("/p/one")), "default not paused");
        // Set paused; the returned previous is the old value.
        assert_eq!(r.set_paused(Path::new("/p/one"), true), Some(false));
        assert!(r.is_paused(Path::new("/p/one")));
        // Re-register (e.g. re-join) must not silently un-pause.
        r.register(Path::new("/p/one"), SessionKind::Join, 2);
        assert!(
            r.is_paused(Path::new("/p/one")),
            "paused survives re-register"
        );
        // Un-pause; setting an unknown path returns None.
        assert_eq!(r.set_paused(Path::new("/p/one"), false), Some(true));
        assert!(!r.is_paused(Path::new("/p/one")));
        assert_eq!(r.set_paused(Path::new("/nope"), true), None);
    }

    #[test]
    fn paused_flag_round_trips_and_defaults_on_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tazamun").join("sessions.json");
        let mut r = Registry::default();
        r.register(Path::new("/x/proj"), SessionKind::Init, 42);
        r.set_paused(Path::new("/x/proj"), true);
        r.save_to(&path).unwrap();
        assert_eq!(Registry::load_from(&path), r);
        // An entry written by an older build (no `paused` key) loads as not paused.
        let legacy = r#"{"sessions":[{"path":"/x/proj","kind":"init","added_ms":42}]}"#;
        std::fs::write(&path, legacy).unwrap();
        assert!(!Registry::load_from(&path).is_paused(Path::new("/x/proj")));
    }

    #[test]
    fn prune_drops_only_absent_sessions() {
        let mut r = Registry::default();
        r.register(Path::new("/live"), SessionKind::Init, 1);
        r.register(Path::new("/dead"), SessionKind::Init, 2);
        let gone = r.prune(|p| p == Path::new("/live"));
        assert_eq!(gone, vec!["/dead".to_string()]);
        assert_eq!(r.sessions.len(), 1);
        assert_eq!(r.sessions[0].path, "/live");
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tazamun").join("sessions.json");
        let mut r = Registry::default();
        r.register(Path::new("/x/proj"), SessionKind::Init, 42);
        r.save_to(&path).unwrap();
        let back = Registry::load_from(&path);
        assert_eq!(back, r);
        // A corrupt/missing file loads as empty, never an error.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Registry::load_from(&path), Registry::default());
        assert_eq!(
            Registry::load_from(Path::new("/no/such/file.json")),
            Registry::default()
        );
    }
}
