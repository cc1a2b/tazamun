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

fn to_rel(root: &Path, abs: &Path) -> Option<RelPath> {
    let rel = abs.strip_prefix(root).ok()?;
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
    let event_root = root.clone();
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
                            let Some(rel) = to_rel(&event_root, path) else {
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
    debouncer.watch(&root, RecursiveMode::Recursive)?;
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
        let root = Path::new("/root");
        assert_eq!(
            to_rel(root, Path::new("/root/a/b.txt")).unwrap().as_str(),
            "a/b.txt"
        );
        assert!(to_rel(root, Path::new("/elsewhere/a")).is_none());
        assert!(to_rel(root, Path::new("/root")).is_none());
    }
}
