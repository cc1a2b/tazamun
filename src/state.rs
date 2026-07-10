//! Persistent per-session state.
//!
//! Invariant: `state.json` is only ever replaced atomically (tempfile + fsync +
//! rename inside `.tazamun/`), so a crash at any point leaves either the old or
//! the new state on disk, never a torn file. Secret material inside is
//! protected by file mode 0600 on Unix.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::consts::META_DIR;
use crate::proto::{FileRecord, ManifestRef};
use crate::session::AddrWire;
use crate::sync::vclock::VClock;

/// A sanitized, forward-slash relative path inside the session folder.
///
/// Construction contract: application code obtains values exclusively through
/// [`crate::sync::index::sanitize_rel_path`]. Serde deserialization is a raw
/// constructor by necessity (wire decoding), which is why every remote-supplied
/// path is re-sanitized at the daemon boundary before use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelPath(String);

impl RelPath {
    pub(crate) fn new_unchecked(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Joins this relative path onto `root` segment by segment. Only safe for
    /// sanitized values, which is all this type holds outside serde decoding.
    pub fn to_fs_path(&self, root: &Path) -> PathBuf {
        let mut out = root.to_path_buf();
        for seg in self.0.split('/') {
            out.push(seg);
        }
        out
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One historical version of a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub manifest: ManifestRef,
    pub vv: VClock,
    pub ts_ms: u64,
    pub size: u64,
}

/// Persisted, per-session network preferences (flags at runtime override).
///
/// Older state files without a `config` block deserialize with defaults, so
/// upgrading in place needs no migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Relay policy: `"default"`, `"none"`, or a relay URL string.
    #[serde(default = "default_relay")]
    pub relay: String,
    /// Whether LAN mDNS discovery is enabled (on by default).
    #[serde(default = "default_true")]
    pub lan: bool,
    /// Airgap mode: relays off + all external discovery off + LAN only.
    #[serde(default)]
    pub airgap: bool,
}

fn default_relay() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            relay: default_relay(),
            lan: true,
            airgap: false,
        }
    }
}

/// The whole persisted application state for one session folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub version: u32,
    pub mode: String,
    pub iroh_secret_key: String,
    pub session_secret: String,
    pub lamport: u64,
    #[serde(default)]
    pub config: SessionConfig,
    pub files: BTreeMap<RelPath, FileRecord>,
    pub known_members: BTreeMap<String, AddrWire>,
    pub history: BTreeMap<RelPath, Vec<VersionEntry>>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("session folder is not initialized (run `tazamun init` or `tazamun join`)")]
    NotInitialized,
    #[error("session state already exists in this folder")]
    AlreadyInitialized,
    #[error("unsupported state version {0}")]
    UnsupportedVersion(u32),
    #[error("state io: {0}")]
    Io(#[from] std::io::Error),
    #[error("state parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid secret material in state: {0}")]
    BadSecret(String),
}

impl AppState {
    pub fn new(iroh_secret_key_hex: String, session_secret_hex: String) -> Self {
        Self {
            version: 1,
            mode: "strict".to_string(),
            iroh_secret_key: iroh_secret_key_hex,
            session_secret: session_secret_hex,
            lamport: 0,
            config: SessionConfig::default(),
            files: BTreeMap::new(),
            known_members: BTreeMap::new(),
            history: BTreeMap::new(),
        }
    }

    pub fn meta_dir(dir: &Path) -> PathBuf {
        dir.join(META_DIR)
    }

    pub fn state_path(dir: &Path) -> PathBuf {
        Self::meta_dir(dir).join("state.json")
    }

    pub fn exists(dir: &Path) -> bool {
        Self::state_path(dir).is_file()
    }

    pub fn load(dir: &Path) -> Result<Self, StateError> {
        let path = Self::state_path(dir);
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StateError::NotInitialized);
            }
            Err(e) => return Err(e.into()),
        };
        let state: AppState = serde_json::from_slice(&raw)?;
        if state.version != 1 {
            return Err(StateError::UnsupportedVersion(state.version));
        }
        Ok(state)
    }

    /// Atomically persists the state: tempfile in `.tazamun/` → fsync → rename.
    pub fn save(&self, dir: &Path) -> Result<(), StateError> {
        let meta = Self::meta_dir(dir);
        create_meta_dirs(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&meta)?;
        serde_json::to_writer_pretty(tmp.as_file_mut(), self)?;
        tmp.as_file_mut().write_all(b"\n")?;
        tmp.as_file().sync_all()?;
        set_secret_mode(tmp.path())?;
        tmp.persist(Self::state_path(dir))
            .map_err(|e| StateError::Io(e.error))?;
        #[cfg(unix)]
        if let Ok(d) = std::fs::File::open(&meta) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    pub fn secret_key_bytes(&self) -> Result<[u8; 32], StateError> {
        decode_hex32(&self.iroh_secret_key)
            .ok_or_else(|| StateError::BadSecret("iroh_secret_key".into()))
    }

    pub fn session_secret_bytes(&self) -> Result<[u8; 32], StateError> {
        decode_hex32(&self.session_secret)
            .ok_or_else(|| StateError::BadSecret("session_secret".into()))
    }
}

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    let raw = data_encoding::HEXLOWER_PERMISSIVE
        .decode(s.as_bytes())
        .ok()?;
    raw.try_into().ok()
}

pub fn encode_hex32(b: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(b)
}

/// Creates `.tazamun/` and its runtime subdirectories with restrictive modes.
pub fn create_meta_dirs(dir: &Path) -> std::io::Result<()> {
    let meta = AppState::meta_dir(dir);
    for sub in [
        meta.clone(),
        meta.join("blobs"),
        meta.join("tmp"),
        meta.join("conflicts"),
    ] {
        std::fs::create_dir_all(&sub)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&meta, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_secret_mode(_path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn tmp_dir(dir: &Path) -> PathBuf {
    AppState::meta_dir(dir).join("tmp")
}

pub fn blobs_dir(dir: &Path) -> PathBuf {
    AppState::meta_dir(dir).join("blobs")
}

pub fn conflicts_dir(dir: &Path) -> PathBuf {
    AppState::meta_dir(dir).join("conflicts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(encode_hex32(&[7u8; 32]), encode_hex32(&[9u8; 32]));
        st.lamport = 42;
        st.save(dir.path()).unwrap();
        let back = AppState::load(dir.path()).unwrap();
        assert_eq!(back.lamport, 42);
        assert_eq!(back.secret_key_bytes().unwrap(), [7u8; 32]);
        assert_eq!(back.session_secret_bytes().unwrap(), [9u8; 32]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(AppState::state_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn load_missing_is_not_initialized() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            AppState::load(dir.path()),
            Err(StateError::NotInitialized)
        ));
    }

    #[test]
    fn config_defaults_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(encode_hex32(&[1u8; 32]), encode_hex32(&[2u8; 32]));
        // Defaults: relay default, LAN on, airgap off.
        assert_eq!(st.config.relay, "default");
        assert!(st.config.lan);
        assert!(!st.config.airgap);
        st.config.relay = "https://relay.example.com./".to_string();
        st.config.lan = false;
        st.config.airgap = true;
        st.save(dir.path()).unwrap();
        let back = AppState::load(dir.path()).unwrap();
        assert_eq!(back.config.relay, "https://relay.example.com./");
        assert!(!back.config.lan);
        assert!(back.config.airgap);
    }

    #[test]
    fn old_state_without_config_gets_defaults() {
        // A state.json written before P3 has no "config" key; it must load with
        // the default config (in-place upgrade, no migration).
        let dir = tempfile::tempdir().unwrap();
        create_meta_dirs(dir.path()).unwrap();
        let json = serde_json::json!({
            "version": 1,
            "mode": "strict",
            "iroh_secret_key": encode_hex32(&[3u8; 32]),
            "session_secret": encode_hex32(&[4u8; 32]),
            "lamport": 5,
            "files": {},
            "known_members": {},
            "history": {},
        });
        std::fs::write(
            AppState::state_path(dir.path()),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();
        let st = AppState::load(dir.path()).unwrap();
        assert_eq!(st.lamport, 5);
        assert_eq!(st.config.relay, "default");
        assert!(st.config.lan);
        assert!(!st.config.airgap);
    }
}
