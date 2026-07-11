//! Filesystem watcher: debounced change events for the session folder.
//!
//! Invariant: anything under a `.tazamun` component never produces an event,
//! and directory events are dropped — only file-level create/modify/remove
//! reaches the daemon. Echo suppression for tazamun's own writes lives in the
//! daemon's mute set, not here.

use std::path::{Component, Path, PathBuf};

use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::mpsc;
use tracing::warn;

use crate::consts::{DEBOUNCE, META_DIR};
use crate::state::RelPath;
use crate::sync::index::sanitize_rel_path;

/// A change observed in the session folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchKind {
    Created,
    Modified,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    pub kind: WatchKind,
    pub rel: RelPath,
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("watcher: {0}")]
    Notify(#[from] notify::Error),
}

/// The live watcher; dropping it stops watching.
pub struct Watcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
}

fn contains_meta(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(seg) => seg.eq_ignore_ascii_case(META_DIR),
        _ => false,
    })
}

/// Maps an absolute event path to a session-relative path. The event path is
/// tried against every candidate root: on macOS a session under
/// `/var/folders/…` is reported by FSEvents as its canonical
/// `/private/var/folders/…` form, so stripping must succeed against either the
/// original or the canonicalized root.
fn to_rel(roots: &[PathBuf], abs: &Path) -> Option<RelPath> {
    let rel = roots.iter().find_map(|root| abs.strip_prefix(root).ok())?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(seg) => parts.push(seg.to_str()?.to_string()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    sanitize_rel_path(&parts.join("/")).ok()
}

/// Starts watching `root` recursively, forwarding debounced events into `tx`.
pub fn spawn(root: PathBuf, tx: mpsc::Sender<WatchEvent>) -> Result<Watcher, WatchError> {
    // Candidate roots for relative-path mapping: the original path plus its
    // canonical form (they differ on macOS, where `/var` → `/private/var`),
    // plus the Windows `\\?\` extended-length form — the root actually watched
    // there, so the directory handle opens past MAX_PATH; events may be
    // reported under either spelling. Deduplicate so the common case tries a
    // single root.
    let mut event_roots = vec![root.clone()];
    if let Ok(canon) = root.canonicalize()
        && canon != root
    {
        event_roots.push(canon);
    }
    let watch_root = crate::win_fs::to_extended(&root);
    if watch_root != root {
        event_roots.push(watch_root.clone());
    }
    let mut debouncer =
        new_debouncer(
            DEBOUNCE,
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    for ev in events {
                        let kind = match ev.event.kind {
                            EventKind::Create(_) => WatchKind::Created,
                            EventKind::Remove(_) => WatchKind::Removed,
                            EventKind::Modify(_) | EventKind::Any | EventKind::Other => {
                                WatchKind::Modified
                            }
                            EventKind::Access(_) => continue,
                        };
                        for path in &ev.event.paths {
                            if contains_meta(path) {
                                continue;
                            }
                            // Directory events carry no file content to sync.
                            if path.is_dir() {
                                continue;
                            }
                            let Some(rel) = to_rel(&event_roots, path) else {
                                continue;
                            };
                            let kind = if kind == WatchKind::Removed && path.is_file() {
                                // Rename pairs can surface as Remove on a path
                                // that still exists; trust the filesystem.
                                WatchKind::Modified
                            } else {
                                kind.clone()
                            };
                            if tx.blocking_send(WatchEvent { kind, rel }).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(errors) => {
                    for e in errors {
                        warn!("watch error: {e}");
                    }
                }
            },
        )?;
    debouncer.watch(&watch_root, RecursiveMode::Recursive)?;
    Ok(Watcher {
        _debouncer: debouncer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_paths_are_filtered() {
        assert!(contains_meta(Path::new("/x/.tazamun/state.json")));
        assert!(contains_meta(Path::new("/x/.TAZAMUN/y")));
        assert!(!contains_meta(Path::new("/x/normal/file.txt")));
    }

    #[test]
    fn rel_mapping() {
        let roots = vec![PathBuf::from("/root")];
        assert_eq!(
            to_rel(&roots, Path::new("/root/a/b.txt")).unwrap().as_str(),
            "a/b.txt"
        );
        assert!(to_rel(&roots, Path::new("/elsewhere/a")).is_none());
        assert!(to_rel(&roots, Path::new("/root")).is_none());
    }

    #[test]
    fn rel_mapping_tries_canonical_root() {
        // macOS reports events under the canonical /private/var prefix while
        // the session root is the /var symlink; stripping must still succeed.
        let roots = vec![
            PathBuf::from("/var/folders/x/session"),
            PathBuf::from("/private/var/folders/x/session"),
        ];
        assert_eq!(
            to_rel(
                &roots,
                Path::new("/private/var/folders/x/session/notes.txt")
            )
            .unwrap()
            .as_str(),
            "notes.txt"
        );
        assert_eq!(
            to_rel(&roots, Path::new("/var/folders/x/session/notes.txt"))
                .unwrap()
                .as_str(),
            "notes.txt"
        );
    }
}
