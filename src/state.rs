//! Persistent per-session state.
//!
//! Invariant: `state.json` is only ever replaced atomically (tempfile + fsync +
//! rename inside `.tazamun/`), so a crash at any point leaves either the old or
//! the new state on disk, never a torn file. Secret material inside is
//! protected by file mode 0600 on Unix.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Absolute on-disk path for this relative path under `root`. On Windows
    /// the result is `\\?\` extended-length form (see [`crate::win_fs`]), so
    /// every downstream filesystem call keeps working past `MAX_PATH`.
    pub fn to_fs_path(&self, root: &Path) -> PathBuf {
        let mut out = root.to_path_buf();
        for seg in self.0.split('/') {
            out.push(seg);
        }
        crate::win_fs::to_extended(&out)
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
    /// P14: an optional human name for this version (`pre-rework`), local to
    /// this node. Absent on entries written before P14 (serde default).
    #[serde(default)]
    pub tag: Option<String>,
    /// P14: a pinned version is never pruned by depth and its blobs are never
    /// GC'd (it stays in `history`, which the GC-protect set already covers).
    #[serde(default)]
    pub pinned: bool,
}

/// Persisted, per-session network preferences (flags at runtime override).
///
/// What this folder is allowed to do in the session (P10). Local enforcement
/// only for now: the daemon refuses this node's own edit paths. Mesh-wide
/// enforcement (peers refusing a viewer's requests) is a later phase and the
/// refusal text says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeRole {
    /// Full lock/unlock rights — the default and the pre-P10 behavior.
    #[default]
    Editor,
    /// Syncs and reads everything; every local edit path (lock, unlock,
    /// restore, publish) is refused. Files stay read-only even in easy mode.
    Viewer,
    /// Receive-only history keeper: same local refusals as `Viewer`, but
    /// keeps a deeper version history for every path it receives.
    Archive,
}

impl NodeRole {
    /// Whether this role may take leases and publish edits.
    pub fn can_edit(self) -> bool {
        matches!(self, NodeRole::Editor)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NodeRole::Editor => "editor",
            NodeRole::Viewer => "viewer",
            NodeRole::Archive => "archive",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "editor" => Ok(NodeRole::Editor),
            "viewer" => Ok(NodeRole::Viewer),
            "archive" => Ok(NodeRole::Archive),
            other => Err(format!(
                "unknown role {other:?} (valid: editor, viewer, archive)"
            )),
        }
    }

    /// Stable wire code shared with [`crate::session`] grants.
    pub fn code(self) -> u8 {
        match self {
            NodeRole::Editor => crate::session::ROLE_EDITOR,
            NodeRole::Viewer => crate::session::ROLE_VIEWER,
            NodeRole::Archive => crate::session::ROLE_ARCHIVE,
        }
    }

    /// Maps a grant's role code back to a `NodeRole` (unknown codes are treated
    /// as the most-restrictive `Viewer`, so a future/garbled code never elevates).
    pub fn from_code(code: u8) -> Self {
        match code {
            crate::session::ROLE_EDITOR => NodeRole::Editor,
            crate::session::ROLE_ARCHIVE => NodeRole::Archive,
            _ => NodeRole::Viewer,
        }
    }
}

/// Older state files without a `config` block deserialize with defaults, so
/// upgrading in place needs no migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Lease time-to-live in milliseconds (this node's leases; carried on the
    /// wire so peers honor the holder's choice). Clamped to
    /// `[MIN_LEASE_TTL, MAX_LEASE_TTL]` on use.
    #[serde(default = "default_lease_ttl_ms")]
    pub lease_ttl_ms: u64,
    /// Lock-acquire timeout in milliseconds. Clamped to
    /// `[MIN_ACQUIRE_TIMEOUT, MAX_ACQUIRE_TIMEOUT]` on use.
    #[serde(default = "default_acquire_timeout_ms")]
    pub acquire_timeout_ms: u64,
    /// Auto-lock-on-first-write: when on, a write to an un-leased path attempts
    /// a standard acquire instead of an immediate violation. Off by default.
    #[serde(default)]
    pub autolock: bool,
    /// Strict exclusive-checkout enforcement. On (default): every synced file
    /// is read-only and editing requires an explicit lease. Off ("easy mode"):
    /// files stay writable and an un-leased local edit auto-acquires a lease and
    /// publishes (as `autolock` does), with conflict quarantine as the safety
    /// net — trading the one-editor-at-a-time guarantee for edit-in-place ease.
    #[serde(default = "default_true")]
    pub strict: bool,
    /// This folder's role in the session: `editor` (full rights, default),
    /// `viewer` (sync + read only), or `archive` (receive-only, deep history).
    #[serde(default)]
    pub role: NodeRole,
    /// How long a lock-waitlist entry lives before timing out, in milliseconds.
    #[serde(default = "default_wait_timeout_ms")]
    pub wait_timeout_ms: u64,
    /// Loopback port the web dashboard binds (P7). Effective on next `start`.
    #[serde(default = "default_dashboard_port")]
    pub dashboard_port: u16,
    /// Release channel `tazamun update` follows: `stable` (default) skips
    /// prereleases; `beta` takes the newest release including prereleases.
    #[serde(default = "default_update_channel")]
    pub update_channel: String,
    /// Built-in junk filter: editor swap/backup files and OS metadata are held
    /// out of the sync (see `sync::ignore::JUNK_PATTERNS`). On by default; a
    /// `.tazamunignore` negation (`!pattern`) re-includes any of it.
    #[serde(default = "default_true")]
    pub junk_filter: bool,
    /// Selective sync: when non-empty, this node carries ONLY this subtree —
    /// everything else is held (acknowledged, listed, never materialized).
    #[serde(default)]
    pub sync_only: String,
    /// Selective sync: comma-separated subtrees this node does not carry.
    #[serde(default)]
    pub sync_skip: String,
    /// Per-file size ceiling in bytes for paths newly entering the index;
    /// 0 = unlimited. Already-indexed files are never affected.
    #[serde(default)]
    pub max_file_size: u64,
    /// P14: how many historical versions to keep per path. 0 = auto (the
    /// role default: 5 for editor/viewer, deeper for `archive`); a set value
    /// overrides it. Pinned versions are kept regardless of this cap.
    #[serde(default)]
    pub history_depth: usize,
    /// P15: download rate ceiling in bytes/sec across all pulls on this folder;
    /// 0 = unlimited. Enforced by a token bucket before each chunk fetch.
    #[serde(default)]
    pub max_down: u64,
    /// P19: write the append-only audit log (`.tazamun/audit.jsonl`). On by
    /// default (cheap, capped); `tazamun log` reads it.
    #[serde(default = "default_true")]
    pub audit: bool,
    /// P19: run user hooks under `.tazamun/hooks/` on session events. On by
    /// default, but costs nothing unless a hook file is actually present.
    #[serde(default = "default_true")]
    pub hooks: bool,
    /// P19: opt-in desktop notifications for events that want a human. Off by
    /// default (a sync daemon should not pop toasts unasked).
    #[serde(default)]
    pub notify: bool,
}

fn default_relay() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

fn default_lease_ttl_ms() -> u64 {
    crate::consts::LEASE_TTL.as_millis() as u64
}

fn default_acquire_timeout_ms() -> u64 {
    crate::consts::ACQUIRE_TIMEOUT.as_millis() as u64
}

fn default_wait_timeout_ms() -> u64 {
    crate::consts::WAIT_TIMEOUT.as_millis() as u64
}

fn default_dashboard_port() -> u16 {
    crate::consts::DASHBOARD_PORT
}

fn default_update_channel() -> String {
    "stable".to_string()
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            relay: default_relay(),
            lan: true,
            airgap: false,
            lease_ttl_ms: default_lease_ttl_ms(),
            acquire_timeout_ms: default_acquire_timeout_ms(),
            autolock: false,
            strict: true,
            role: NodeRole::default(),
            wait_timeout_ms: default_wait_timeout_ms(),
            dashboard_port: default_dashboard_port(),
            update_channel: default_update_channel(),
            junk_filter: true,
            sync_only: String::new(),
            sync_skip: String::new(),
            max_file_size: 0,
            history_depth: 0,
            max_down: 0,
            audit: true,
            hooks: true,
            notify: false,
        }
    }
}

/// Parses a human size: plain bytes, `KB`/`MB`/`GB` suffixes (binary, 1024),
/// or `0`/`off`/`unlimited` for no ceiling.
pub fn parse_size(value: &str) -> Result<u64, String> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() || v == "0" || v == "off" || v == "unlimited" {
        return Ok(0);
    }
    let (num, mult) = if let Some(n) = v.strip_suffix("gb") {
        (n, 1024 * 1024 * 1024)
    } else if let Some(n) = v.strip_suffix("mb") {
        (n, 1024 * 1024)
    } else if let Some(n) = v.strip_suffix("kb") {
        (n, 1024)
    } else if let Some(n) = v.strip_suffix('b') {
        (n, 1)
    } else {
        (v.as_str(), 1)
    };
    let num: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid size {value:?} (use 0, 500KB, 100MB, 2GB)"))?;
    num.checked_mul(mult)
        .ok_or_else(|| format!("size {value:?} overflows"))
}

/// Validates a selective-sync subtree value: empty clears the setting;
/// anything else must pass the same path sanitizer every synced path passes.
fn validate_subtree(value: &str) -> Result<String, String> {
    let v = value.trim().trim_matches('/');
    if v.is_empty() {
        return Ok(String::new());
    }
    crate::sync::index::sanitize_rel_path(v)
        .map(|p| p.as_str().to_string())
        .map_err(|e| format!("invalid subtree {value:?}: {e}"))
}

/// Formats a byte ceiling for display; 0 renders as `unlimited`.
pub fn fmt_size(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    const KB: u64 = 1024;
    match bytes {
        0 => "unlimited".to_string(),
        b if b % GB == 0 => format!("{}GB", b / GB),
        b if b % MB == 0 => format!("{}MB", b / MB),
        b if b % KB == 0 => format!("{}KB", b / KB),
        b => format!("{b}B"),
    }
}

fn parse_on_off(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => Err(format!("expected on/off, got {other:?}")),
    }
}

impl SessionConfig {
    /// Whether settled files on this node are clamped read-only. Strict mode
    /// clamps; easy mode leaves files writable — but a non-editor role always
    /// clamps regardless, because this node can never publish an edit, so a
    /// writable file would only invite changes that must be reverted.
    pub fn enforce_readonly(&self) -> bool {
        self.strict || !self.role.can_edit()
    }

    /// Applies any persisted config key — the full surface: network, editing
    /// policy, role, timings, interface. This is the **single parser** shared
    /// by `tazamun config set`, the setup panel, and the init wizard, so a key
    /// means exactly the same thing everywhere. Pure (no I/O); the caller
    /// persists. Returns a human note describing what was set.
    pub fn set_value(&mut self, key: &str, value: &str) -> Result<String, String> {
        let clamp_dur = |value: &str, min: Duration, max: Duration| -> Result<Duration, String> {
            let raw = humantime::parse_duration(value.trim())
                .map_err(|e| format!("invalid duration {value:?}: {e} (use 90s, 15m, 2h)"))?;
            Ok(raw.clamp(min, max))
        };
        match key {
            "relay" => {
                let choice = crate::net::endpoint::RelayChoice::parse(value)?;
                self.relay = choice.to_config_string();
                Ok(format!("relay = {}", self.relay))
            }
            "lan" => {
                self.lan = parse_on_off(value)?;
                Ok(format!("lan = {}", if self.lan { "on" } else { "off" }))
            }
            "airgap" => {
                self.airgap = parse_on_off(value)?;
                Ok(format!(
                    "airgap = {}",
                    if self.airgap { "on" } else { "off" }
                ))
            }
            "strict" => {
                self.strict = parse_on_off(value)?;
                Ok(format!(
                    "strict = {}",
                    if self.strict { "on" } else { "off (easy mode)" }
                ))
            }
            "role" => {
                self.role = NodeRole::parse(value)?;
                Ok(format!("role = {}", self.role.as_str()))
            }
            "autolock" => {
                self.autolock = parse_on_off(value)?;
                Ok(format!(
                    "autolock = {}",
                    if self.autolock { "on" } else { "off" }
                ))
            }
            "lease-ttl" => {
                let d = clamp_dur(
                    value,
                    crate::consts::MIN_LEASE_TTL,
                    crate::consts::MAX_LEASE_TTL,
                )?;
                self.lease_ttl_ms = d.as_millis() as u64;
                Ok(format!("lease-ttl = {}", humantime::format_duration(d)))
            }
            "acquire-timeout" => {
                let d = clamp_dur(
                    value,
                    crate::consts::MIN_ACQUIRE_TIMEOUT,
                    crate::consts::MAX_ACQUIRE_TIMEOUT,
                )?;
                self.acquire_timeout_ms = d.as_millis() as u64;
                Ok(format!(
                    "acquire-timeout = {}",
                    humantime::format_duration(d)
                ))
            }
            "wait-timeout" => {
                let d = clamp_dur(value, Duration::from_secs(10), crate::consts::MAX_LEASE_TTL)?;
                self.wait_timeout_ms = d.as_millis() as u64;
                Ok(format!("wait-timeout = {}", humantime::format_duration(d)))
            }
            "dashboard-port" | "dashboard.port" => {
                let p: u16 = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid port {value:?} (1024–65535)"))?;
                if p < 1024 {
                    return Err("port must be >= 1024".to_string());
                }
                self.dashboard_port = p;
                Ok(format!("dashboard-port = {p} (effective on next start)"))
            }
            "update-channel" => {
                let v = value.trim().to_ascii_lowercase();
                if v != "stable" && v != "beta" {
                    return Err(format!("unknown channel {value:?} (valid: stable, beta)"));
                }
                self.update_channel = v;
                Ok(format!("update-channel = {}", self.update_channel))
            }
            "junk-filter" => {
                self.junk_filter = parse_on_off(value)?;
                Ok(format!(
                    "junk-filter = {}",
                    if self.junk_filter { "on" } else { "off" }
                ))
            }
            "audit" => {
                self.audit = parse_on_off(value)?;
                Ok(format!("audit = {}", if self.audit { "on" } else { "off" }))
            }
            "hooks" => {
                self.hooks = parse_on_off(value)?;
                Ok(format!("hooks = {}", if self.hooks { "on" } else { "off" }))
            }
            "notify" => {
                self.notify = parse_on_off(value)?;
                Ok(format!(
                    "notify = {}",
                    if self.notify { "on" } else { "off" }
                ))
            }
            "sync-only" => {
                let v = validate_subtree(value)?;
                self.sync_only = v;
                Ok(if self.sync_only.is_empty() {
                    "sync-only cleared (carrying everything)".to_string()
                } else {
                    format!("sync-only = {}/", self.sync_only)
                })
            }
            "sync-skip" => {
                let mut parts = Vec::new();
                for raw in value.split(',') {
                    let v = validate_subtree(raw)?;
                    if !v.is_empty() {
                        parts.push(v);
                    }
                }
                self.sync_skip = parts.join(",");
                Ok(if self.sync_skip.is_empty() {
                    "sync-skip cleared".to_string()
                } else {
                    format!("sync-skip = {}", self.sync_skip)
                })
            }
            "max-file-size" => {
                self.max_file_size = parse_size(value)?;
                Ok(format!(
                    "max-file-size = {} (new paths only)",
                    fmt_size(self.max_file_size)
                ))
            }
            "history-depth" => {
                let v = value.trim();
                let n: usize = if v.eq_ignore_ascii_case("auto") {
                    0
                } else {
                    v.parse().map_err(|_| {
                        format!("invalid history-depth {value:?} (0/auto, or 1-1000)")
                    })?
                };
                if n > 1000 {
                    return Err("history-depth caps at 1000".to_string());
                }
                self.history_depth = n;
                Ok(format!(
                    "history-depth = {}",
                    if n == 0 {
                        "auto".to_string()
                    } else {
                        n.to_string()
                    }
                ))
            }
            "max-down" => {
                self.max_down = parse_size(value)?;
                Ok(format!(
                    "max-down = {}",
                    if self.max_down == 0 {
                        "unlimited".to_string()
                    } else {
                        format!("{}/s", fmt_size(self.max_down))
                    }
                ))
            }
            other => Err(format!(
                "unknown config key {other:?} (valid: relay, lan, airgap, strict, role, \
                 autolock, lease-ttl, acquire-timeout, wait-timeout, dashboard-port, \
                 update-channel, junk-filter, sync-only, sync-skip, max-file-size, \
                 history-depth, max-down, audit, hooks, notify)"
            )),
        }
    }

    /// Applies a config change for the keys that may be set **live** through
    /// the running daemon (the dashboard's `/api/config` and the `ConfigSet`
    /// IPC). Delegates parsing to [`Self::set_value`] — same key, same
    /// meaning. Keys that change enforcement or network topology
    /// (`relay`/`lan`/`airgap`/`strict`/`role`) require a restart and are
    /// refused here. Pure (no I/O); the caller persists.
    pub fn set_live_value(&mut self, key: &str, value: &str) -> Result<String, String> {
        match key {
            "autolock" | "lease-ttl" | "acquire-timeout" | "wait-timeout" | "dashboard-port"
            | "dashboard.port" | "update-channel" | "max-down" | "audit" | "hooks" | "notify" => {
                self.set_value(key, value)
            }
            "relay" | "lan" | "airgap" | "strict" | "role" | "junk-filter" | "sync-only"
            | "sync-skip" | "max-file-size" => Err(format!(
                "{key} needs a restart; use `tazamun config set {key} …` then restart the daemon"
            )),
            other => Err(format!(
                "unknown or non-live config key {other:?} (live keys: autolock, lease-ttl, \
                 acquire-timeout, wait-timeout, dashboard-port, update-channel, max-down, \
                 audit, hooks, notify)"
            )),
        }
    }

    /// Effective lease TTL, clamped to `[MIN_LEASE_TTL, MAX_LEASE_TTL]`.
    pub fn lease_ttl(&self) -> Duration {
        Duration::from_millis(self.lease_ttl_ms)
            .clamp(crate::consts::MIN_LEASE_TTL, crate::consts::MAX_LEASE_TTL)
    }

    /// Effective lease renew interval — always `ttl / 3`, never configured
    /// directly, so a holder renews comfortably before expiry.
    pub fn lease_renew(&self) -> Duration {
        self.lease_ttl() / 3
    }

    /// Effective acquire timeout, clamped to
    /// `[MIN_ACQUIRE_TIMEOUT, MAX_ACQUIRE_TIMEOUT]`.
    pub fn acquire_timeout(&self) -> Duration {
        Duration::from_millis(self.acquire_timeout_ms).clamp(
            crate::consts::MIN_ACQUIRE_TIMEOUT,
            crate::consts::MAX_ACQUIRE_TIMEOUT,
        )
    }

    /// Effective waitlist entry lifetime.
    pub fn wait_timeout(&self) -> Duration {
        Duration::from_millis(self.wait_timeout_ms)
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
    /// Remote records this node acknowledges but refuses to materialize
    /// because the path is not representable here (Windows portability rules).
    /// The sync loop treats them as settled — no re-pull churn — and `status`
    /// / `doctor` surface them. Never populated on Unix (warn-only there).
    #[serde(default)]
    pub unapplied: BTreeMap<RelPath, UnappliedEntry>,
    /// P15 resume: records with a pull in flight. Their chunks are kept
    /// GC-protected across a daemon restart, so a partial pull continues
    /// instead of restarting (the store already skips chunks it has). Cleared
    /// when the record is applied. Never on the sync wire.
    #[serde(default)]
    pub pulling: BTreeMap<RelPath, FileRecord>,
    /// P17: the session admin Ed25519 *public* key (hex), the verify key for
    /// role grants. `Some` on every v2 member; `None` on a legacy session (which
    /// enforces no roles — every member is effectively an editor, as before).
    #[serde(default)]
    pub admin_public_key: Option<String>,
    /// P17: the session admin *secret* key (hex), the sign key for grants.
    /// `Some` only on editor/admin nodes (it rides in editor invites); `None` on
    /// viewers/archives — that omission is what makes their role unforgeable.
    #[serde(default)]
    pub admin_secret_key: Option<String>,
    /// P17: this node's own signed role grant, presented to peers after the
    /// handshake so they record and enforce this node's role. `None` on a legacy
    /// session.
    #[serde(default)]
    pub my_grant: Option<crate::session::SignedGrant>,
    /// P17: friendly, local labels for peers (endpoint-id hex → name), shown in
    /// `status`, `locks`, `peers`, and the dashboard instead of a hex prefix.
    /// Purely local display metadata — never on the wire, never trusted.
    #[serde(default)]
    pub peer_names: BTreeMap<String, String>,
}

/// One non-materialized remote record plus the human-readable reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnappliedEntry {
    pub record: FileRecord,
    pub reason: String,
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
            unapplied: BTreeMap::new(),
            pulling: BTreeMap::new(),
            admin_public_key: None,
            admin_secret_key: None,
            my_grant: None,
            peer_names: BTreeMap::new(),
        }
    }

    /// Whether this session enforces roles on the wire (it is a v2 session with
    /// a known admin verify key). Legacy v1 sessions return `false` and behave
    /// exactly as before (no mesh-wide role enforcement).
    pub fn enforcing_roles(&self) -> bool {
        self.admin_public_key.is_some()
    }

    /// The admin public key bytes, if this is a v2 session.
    pub fn admin_public_bytes(&self) -> Option<[u8; 32]> {
        self.admin_public_key.as_deref().and_then(decode_hex32)
    }

    /// The admin secret signing key, if this node holds it (editor/admin).
    pub fn admin_secret_key(&self) -> Option<iroh::SecretKey> {
        let bytes = self.admin_secret_key.as_deref().and_then(decode_hex32)?;
        Some(iroh::SecretKey::from_bytes(&bytes))
    }

    /// `.tazamun` metadata directory, in `\\?\` extended-length form on
    /// Windows (covers `state.json`, staging, blobs, conflicts, logs — every
    /// metadata fs path funnels through here).
    pub fn meta_dir(dir: &Path) -> PathBuf {
        crate::win_fs::to_extended(&dir.join(META_DIR))
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

    /// The node's **public** node id in short form — safe to display or serialize
    /// anywhere. NEVER derive a display id from `iroh_secret_key` directly: that
    /// field is the node's *private* key, so a prefix of it is leaked secret
    /// material (and the wrong identifier). Returns `None` if the stored secret
    /// is unreadable.
    pub fn node_id_short(&self) -> Option<String> {
        let bytes = self.secret_key_bytes().ok()?;
        Some(
            iroh::SecretKey::from_bytes(&bytes)
                .public()
                .fmt_short()
                .to_string(),
        )
    }
}

pub fn decode_hex32(s: &str) -> Option<[u8; 32]> {
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
        meta.join("hooks"),
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

/// P18: advisory sidecar index for quarantined copies — one JSON line per
/// quarantine event recording the reason and the original (un-truncated)
/// relative path. A sibling of the conflicts dir, so that dir stays purely
/// user bytes. Losing it loses no data (the copies are the truth).
pub fn conflicts_index_path(dir: &Path) -> PathBuf {
    AppState::meta_dir(dir).join("conflicts-index.jsonl")
}

pub fn logs_dir(dir: &Path) -> PathBuf {
    AppState::meta_dir(dir).join("logs")
}

/// The OS-appropriate base directory for application logs, so the daemon log
/// lives outside the synced folder (and can never be swept into a sync):
/// `%LOCALAPPDATA%` on Windows, `~/Library/Logs` on macOS,
/// `$XDG_STATE_HOME` (or `~/.local/state`) on other Unix, and the OS temp
/// directory as the last-resort fallback.
fn os_log_base() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(p);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library").join("Logs");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(p) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(p);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local").join("state");
        }
    }
    std::env::temp_dir()
}

/// Per-session daemon log path under the OS log directory:
/// `<os-log-base>/tazamun/daemon-<hash8>.log`, where the hash is the first
/// 8 bytes of BLAKE3 over the absolute session path — so distinct folders get
/// distinct logs and both writer and reader derive the same file.
pub fn log_file_path(dir: &Path) -> PathBuf {
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = blake3::hash(abs.to_string_lossy().as_bytes());
    let hex = data_encoding::HEXLOWER.encode(&digest.as_bytes()[..8]);
    os_log_base()
        .join("tazamun")
        .join(format!("daemon-{hex}.log"))
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
        // The P4 timing/autolock fields also fall back to their defaults.
        assert_eq!(st.config.lease_ttl(), crate::consts::LEASE_TTL);
        assert_eq!(st.config.acquire_timeout(), crate::consts::ACQUIRE_TIMEOUT);
        assert!(!st.config.autolock);
        // Absent `strict` defaults to true — an in-place upgrade stays strict.
        assert!(st.config.strict);
        // Absent P10 fields default to editor/stable — upgrades change nothing.
        assert_eq!(st.config.role, NodeRole::Editor);
        assert_eq!(st.config.update_channel, "stable");
        assert_eq!(st.config.wait_timeout(), crate::consts::WAIT_TIMEOUT);
    }

    #[test]
    fn lease_timing_helpers_clamp_and_derive() {
        use crate::consts::{
            MAX_ACQUIRE_TIMEOUT, MAX_LEASE_TTL, MIN_ACQUIRE_TIMEOUT, MIN_LEASE_TTL,
        };
        // Below the floor clamps up; above the ceiling clamps down.
        let mut c = SessionConfig {
            lease_ttl_ms: 1, // 1ms
            ..SessionConfig::default()
        };
        assert_eq!(c.lease_ttl(), MIN_LEASE_TTL);
        c.lease_ttl_ms = 48 * 60 * 60 * 1000; // 48h
        assert_eq!(c.lease_ttl(), MAX_LEASE_TTL);

        // Renew is always ttl/3, derived from the (clamped) ttl.
        c.lease_ttl_ms = 90_000;
        assert_eq!(c.lease_ttl(), Duration::from_secs(90));
        assert_eq!(c.lease_renew(), Duration::from_secs(30));

        // Acquire timeout clamps to its own band.
        c.acquire_timeout_ms = 0;
        assert_eq!(c.acquire_timeout(), MIN_ACQUIRE_TIMEOUT);
        c.acquire_timeout_ms = 10 * 60 * 1000;
        assert_eq!(c.acquire_timeout(), MAX_ACQUIRE_TIMEOUT);
    }

    #[test]
    fn new_config_fields_persist_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(encode_hex32(&[1u8; 32]), encode_hex32(&[2u8; 32]));
        st.config.lease_ttl_ms = 300_000;
        st.config.acquire_timeout_ms = 12_000;
        st.config.autolock = true;
        st.config.strict = false;
        st.config.role = NodeRole::Viewer;
        st.config.update_channel = "beta".to_string();
        st.config.wait_timeout_ms = 120_000;
        st.save(dir.path()).unwrap();
        let back = AppState::load(dir.path()).unwrap();
        assert_eq!(back.config.lease_ttl(), Duration::from_secs(300));
        assert_eq!(back.config.acquire_timeout(), Duration::from_secs(12));
        assert!(back.config.autolock);
        assert!(!back.config.strict);
        assert_eq!(back.config.role, NodeRole::Viewer);
        assert_eq!(back.config.update_channel, "beta");
        assert_eq!(back.config.wait_timeout(), Duration::from_secs(120));
    }

    #[test]
    fn strict_and_role_are_restart_required_not_live() {
        // `strict` and `role` change enforcement across the whole tree, so they
        // are restart-required keys like the network ones — refused live.
        let mut c = SessionConfig::default();
        assert!(c.set_live_value("strict", "off").is_err());
        assert!(c.set_live_value("role", "viewer").is_err());
        assert!(c.strict, "live path must not have mutated strict");
        assert_eq!(c.role, NodeRole::Editor, "live path must not mutate role");
    }

    #[test]
    fn set_value_covers_every_key_and_rejects_garbage() {
        // The unified setter is the single parser shared by config set, the
        // setup panel, and the wizard — exercise one success per key.
        let mut c = SessionConfig::default();
        for (key, value) in [
            ("relay", "none"),
            ("lan", "off"),
            ("airgap", "on"),
            ("strict", "off"),
            ("role", "archive"),
            ("autolock", "on"),
            ("lease-ttl", "15m"),
            ("acquire-timeout", "10s"),
            ("wait-timeout", "2m"),
            ("dashboard-port", "9000"),
            ("update-channel", "beta"),
        ] {
            c.set_value(key, value)
                .unwrap_or_else(|e| panic!("set_value({key}, {value}) failed: {e}"));
        }
        assert_eq!(c.relay, "none");
        assert!(!c.lan && c.airgap && !c.strict && c.autolock);
        assert_eq!(c.role, NodeRole::Archive);
        assert_eq!(c.dashboard_port, 9000);
        assert_eq!(c.update_channel, "beta");
        // Garbage values are typed errors, never silent defaults.
        assert!(c.set_value("role", "admin").is_err());
        assert!(c.set_value("update-channel", "nightly").is_err());
        assert!(c.set_value("relay", "not a url").is_err());
        assert!(c.set_value("no-such-key", "x").is_err());
    }

    #[test]
    fn readonly_enforcement_follows_strict_and_role() {
        let mut c = SessionConfig::default();
        // editor + strict: clamped (the default).
        assert!(c.enforce_readonly());
        // editor + easy: writable — the easy-mode contract.
        c.strict = false;
        assert!(!c.enforce_readonly());
        // A non-editor role clamps even in easy mode: this node can never
        // publish, so writable files would only invite doomed edits.
        c.role = NodeRole::Viewer;
        assert!(c.enforce_readonly());
        c.role = NodeRole::Archive;
        assert!(c.enforce_readonly());
    }

    #[test]
    fn role_parses_and_prints() {
        assert_eq!(NodeRole::parse("Editor").unwrap(), NodeRole::Editor);
        assert_eq!(NodeRole::parse(" viewer ").unwrap(), NodeRole::Viewer);
        assert_eq!(NodeRole::parse("ARCHIVE").unwrap(), NodeRole::Archive);
        assert!(NodeRole::parse("owner").is_err());
        assert!(NodeRole::Editor.can_edit());
        assert!(!NodeRole::Viewer.can_edit());
        assert!(!NodeRole::Archive.can_edit());
        assert_eq!(NodeRole::Viewer.as_str(), "viewer");
    }
}
