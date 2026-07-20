//! Folder grouping and weighting for the native GUI's Files view.
//!
//! A flat list of paths tells you what a session holds but never where its
//! weight sits. This module supplies both halves of the structure the view
//! draws: files bucketed by their top-level folder (root-level files collect
//! under [`ROOT_NAME`]), and each bucket's [`share`](Group::share) of the
//! session's total bytes — the number behind the proportion bar.
//!
//! Groups carry *indices* into the caller's slice rather than clones, so the
//! view keeps its own row data and this module stays pure `std` — no egui, no
//! I/O, no `PathBuf`. Ordering is total and deterministic under both
//! [`SortMode`]s: every comparison breaks its ties, so equal keys never depend
//! on hash order or input order for their final position.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// How the Files view orders groups and the files inside them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortMode {
    /// Alphabetical, case-insensitive.
    #[default]
    Name,
    /// Largest first.
    Size,
}

/// One top-level folder's worth of files (or the session root).
#[derive(Clone, Debug)]
pub struct Group {
    /// Display name: the top path segment, or `ROOT_NAME` for files at the root.
    pub name: String,
    /// Indices into the slice passed to [`group_files`], already sorted.
    pub indices: Vec<usize>,
    /// Total bytes across this group's files.
    pub bytes: u64,
    /// This group's share of the whole session, 0.0..=1.0 (0.0 when the
    /// session holds no bytes at all).
    pub share: f32,
    /// True for the session-root bucket, so the view can style it apart from a
    /// real folder without comparing display names.
    pub root: bool,
}

/// Display name used for files that sit directly in the session root.
pub const ROOT_NAME: &str = "in this folder";

/// Internal bucket key for root-level files. NUL cannot occur in a path
/// segment on any supported platform, so a real folder can never collide with
/// the root bucket the way a human-readable key would.
const ROOT_KEY: &str = "\0root";

/// The leading path segment of `path`, or `""` when the path has no separator
/// (a root-level file). Handles both `/` and `\` separators.
pub fn top_segment(path: &str) -> &str {
    match path.find(['/', '\\']) {
        // Both separators are ASCII, so the hit is always a char boundary.
        Some(cut) => &path[..cut],
        None => "",
    }
}

/// Groups `files` (path, size) by top-level folder and sorts both the groups
/// and each group's indices by `sort`. Groups are stable for equal keys.
pub fn group_files(files: &[(String, u64)], sort: SortMode) -> Vec<Group> {
    let mut buckets: BTreeMap<String, (Vec<usize>, u64)> = BTreeMap::new();
    let mut total: u64 = 0;

    for (i, (path, size)) in files.iter().enumerate() {
        let seg = top_segment(path);
        let key = if seg.is_empty() { ROOT_KEY } else { seg };
        // Only a first sighting pays for the owned key.
        match buckets.get_mut(key) {
            Some((indices, bytes)) => {
                indices.push(i);
                *bytes = bytes.saturating_add(*size);
            }
            None => {
                buckets.insert(key.to_string(), (vec![i], *size));
            }
        }
        total = total.saturating_add(*size);
    }

    let mut groups: Vec<Group> = buckets
        .into_iter()
        .map(|(key, (mut indices, bytes))| {
            sort_indices(&mut indices, files, sort);
            let share = if total == 0 {
                0.0
            } else {
                (bytes as f64 / total as f64) as f32
            };
            let root = key == ROOT_KEY;
            let name = if root { ROOT_NAME.to_string() } else { key };
            Group {
                name,
                indices,
                bytes,
                share,
                root,
            }
        })
        .collect();

    match sort {
        SortMode::Name => groups.sort_by(|a, b| ci_cmp(&a.name, &b.name)),
        SortMode::Size => {
            groups.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| ci_cmp(&a.name, &b.name)))
        }
    }
    groups
}

/// A compact "3 files · 1.2 MB · 41%" summary line for a group. `size_text`
/// is the caller's already-formatted byte string.
pub fn group_caption(count: usize, size_text: &str, share: f32) -> String {
    let s = if count == 1 { "" } else { "s" };
    let pct = if share.is_finite() {
        (share * 100.0).round().clamp(0.0, 100.0) as u32
    } else {
        0
    };
    format!("{count} file{s} · {size_text} · {pct}%")
}

/// Orders one group's indices in place under `sort`; ties always fall back to
/// case-insensitive path so the result is total, not merely stable.
fn sort_indices(indices: &mut [usize], files: &[(String, u64)], sort: SortMode) {
    match sort {
        SortMode::Name => {
            indices.sort_by(|&a, &b| ci_cmp(path_at(files, a), path_at(files, b)));
        }
        SortMode::Size => indices.sort_by(|&a, &b| {
            size_at(files, b)
                .cmp(&size_at(files, a))
                .then_with(|| ci_cmp(path_at(files, a), path_at(files, b)))
        }),
    }
}

/// Path for an index, or `""` if the index is out of range — comparators must
/// never panic on a stale or hand-built index list.
fn path_at(files: &[(String, u64)], i: usize) -> &str {
    files.get(i).map_or("", |(p, _)| p.as_str())
}

/// Size for an index, or `0` if the index is out of range.
fn size_at(files: &[(String, u64)], i: usize) -> u64 {
    files.get(i).map_or(0, |&(_, s)| s)
}

/// Case-insensitive lexicographic compare, allocation-free: lowercase mappings
/// are folded lazily rather than through two temporary `String`s.
fn ci_cmp(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, size: u64) -> (String, u64) {
        (path.to_string(), size)
    }

    fn named(groups: &[Group]) -> Vec<&str> {
        groups.iter().map(|g| g.name.as_str()).collect()
    }

    fn group<'a>(groups: &'a [Group], name: &str) -> &'a Group {
        match groups.iter().find(|g| g.name == name) {
            Some(g) => g,
            None => panic!("missing group {name:?} in {:?}", named(groups)),
        }
    }

    #[test]
    fn top_segment_splits_on_first_separator() {
        assert_eq!(top_segment("a/b.txt"), "a");
        assert_eq!(top_segment("a\\b.txt"), "a");
        assert_eq!(top_segment("docs/deep/nested/file.txt"), "docs");
        // No separator at all: a root-level file.
        assert_eq!(top_segment("file.txt"), "");
        assert_eq!(top_segment(""), "");
        // A leading separator yields the empty first segment — also root-level.
        assert_eq!(top_segment("/leading"), "");
        assert_eq!(top_segment("\\leading"), "");
        // Whichever separator comes first wins, regardless of kind.
        assert_eq!(top_segment("x/y\\z"), "x");
        assert_eq!(top_segment("p\\q/r"), "p");
    }

    #[test]
    fn root_files_bucket_under_root_name() {
        let files = [
            f("readme.md", 10),
            f("docs/guide.md", 20),
            f("notes.txt", 30),
            f("docs/api.md", 40),
            f("/odd", 5),
        ];
        let groups = group_files(&files, SortMode::Name);
        assert_eq!(groups.len(), 2);

        let root = group(&groups, ROOT_NAME);
        assert_eq!(root.indices.len(), 3);
        assert_eq!(root.bytes, 45);

        let docs = group(&groups, "docs");
        assert_eq!(docs.indices.len(), 2);
        assert_eq!(docs.bytes, 60);
    }

    #[test]
    fn shares_sum_to_one_and_collapse_to_zero() {
        let files = [
            f("a/one", 1),
            f("b/two", 2),
            f("c/three", 3),
            f("root", 4),
            f("a/four", 90),
        ];
        let groups = group_files(&files, SortMode::Size);
        let sum: f32 = groups.iter().map(|g| g.share).sum();
        assert!((sum - 1.0).abs() < 1e-5, "shares summed to {sum}");
        assert!(groups.iter().all(|g| (0.0..=1.0).contains(&g.share)));

        let a = group(&groups, "a");
        assert_eq!(a.bytes, 91);
        assert!((a.share - 91.0 / 100.0).abs() < 1e-5);

        // Every size zero => total zero => every share pinned to 0.0.
        let empty_bytes = [f("a/one", 0), f("b/two", 0), f("root", 0)];
        let groups = group_files(&empty_bytes, SortMode::Name);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|g| g.share == 0.0));
        assert!(groups.iter().all(|g| g.bytes == 0));
    }

    #[test]
    fn name_sort_is_case_insensitive() {
        let files = [
            f("Beta/x", 1),
            f("alpha/y", 1),
            f("Gamma/z", 1),
            f("delta/w", 1),
        ];
        let groups = group_files(&files, SortMode::Name);
        // Byte-order would put every capitalized name first; case folding must not.
        assert_eq!(named(&groups), vec!["alpha", "Beta", "delta", "Gamma"]);
    }

    #[test]
    fn name_sort_orders_indices_case_insensitively() {
        let files = [
            f("docs/Zebra.md", 1),
            f("docs/apple.md", 1),
            f("docs/Mango.md", 1),
        ];
        let groups = group_files(&files, SortMode::Name);
        let docs = group(&groups, "docs");
        let paths: Vec<&str> = docs.indices.iter().map(|&i| files[i].0.as_str()).collect();
        assert_eq!(
            paths,
            vec!["docs/apple.md", "docs/Mango.md", "docs/Zebra.md"]
        );
    }

    #[test]
    fn size_sort_is_largest_first_then_name() {
        let files = [
            f("small/a", 10),
            f("Big/b", 500),
            f("tie/c", 100),
            f("Ally/d", 100),
        ];
        let groups = group_files(&files, SortMode::Size);
        // 500 first; the two 100-byte groups tie and fall back to name.
        assert_eq!(named(&groups), vec!["Big", "Ally", "tie", "small"]);
        assert_eq!(groups[0].bytes, 500);
        assert_eq!(groups[3].bytes, 10);
    }

    #[test]
    fn size_sort_orders_indices_largest_first_then_path() {
        let files = [
            f("d/small.bin", 5),
            f("d/huge.bin", 900),
            f("d/Zed.bin", 50),
            f("d/apex.bin", 50),
        ];
        let groups = group_files(&files, SortMode::Size);
        let d = group(&groups, "d");
        let paths: Vec<&str> = d.indices.iter().map(|&i| files[i].0.as_str()).collect();
        assert_eq!(
            paths,
            vec!["d/huge.bin", "d/apex.bin", "d/Zed.bin", "d/small.bin"]
        );
    }

    #[test]
    fn every_file_appears_exactly_once_with_valid_indices() {
        let files = [
            f("a/1", 3),
            f("root", 7),
            f("b/2", 11),
            f("a/3", 13),
            f("a/1", 17),
            f("b/4", 0),
            f("c\\5", 1),
        ];
        for sort in [SortMode::Name, SortMode::Size] {
            let groups = group_files(&files, sort);
            let mut seen: Vec<usize> = groups
                .iter()
                .flat_map(|g| g.indices.iter())
                .copied()
                .collect();
            assert!(seen.iter().all(|&i| i < files.len()), "index out of range");
            seen.sort_unstable();
            assert_eq!(seen, (0..files.len()).collect::<Vec<_>>(), "sort {sort:?}");

            // Group bytes reconcile with the files each group claims.
            for g in &groups {
                let want: u64 = g.indices.iter().map(|&i| files[i].1).sum();
                assert_eq!(g.bytes, want, "group {} bytes", g.name);
            }
        }
    }

    #[test]
    fn duplicate_paths_are_kept_as_separate_entries() {
        let files = [f("a/dup", 5), f("a/dup", 5), f("a/dup", 5)];
        let groups = group_files(&files, SortMode::Name);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indices.len(), 3);
        assert_eq!(groups[0].bytes, 15);
    }

    #[test]
    fn empty_input_yields_empty_vec() {
        for sort in [SortMode::Name, SortMode::Size] {
            assert!(group_files(&[], sort).is_empty(), "sort {sort:?}");
        }
    }

    #[test]
    fn default_sort_mode_is_name() {
        assert_eq!(SortMode::default(), SortMode::Name);
    }

    #[test]
    fn caption_singular_and_plural() {
        assert_eq!(group_caption(1, "4 KB", 0.5), "1 file · 4 KB · 50%");
        assert_eq!(group_caption(0, "0 B", 0.0), "0 files · 0 B · 0%");
        assert_eq!(group_caption(3, "1.2 MB", 0.41), "3 files · 1.2 MB · 41%");
    }

    #[test]
    fn caption_rounds_and_clamps_percent() {
        assert_eq!(group_caption(2, "x", 0.414), "2 files · x · 41%");
        assert_eq!(group_caption(2, "x", 0.416), "2 files · x · 42%");
        assert_eq!(group_caption(2, "x", 0.006), "2 files · x · 1%");
        assert_eq!(group_caption(2, "x", 0.004), "2 files · x · 0%");
        assert_eq!(group_caption(2, "x", 1.0), "2 files · x · 100%");
        // 0.125 and 0.375 are exact in f32, so these pin the half-away-from-zero
        // rule itself rather than an artifact of decimal-to-binary rounding.
        assert_eq!(group_caption(2, "x", 0.125), "2 files · x · 13%");
        assert_eq!(group_caption(2, "x", 0.375), "2 files · x · 38%");
        // Out-of-range input clamps rather than rendering nonsense.
        assert_eq!(group_caption(2, "x", 3.7), "2 files · x · 100%");
        assert_eq!(group_caption(2, "x", -0.5), "2 files · x · 0%");
    }

    #[test]
    fn caption_non_finite_share_renders_zero() {
        assert_eq!(group_caption(1, "x", f32::NAN), "1 file · x · 0%");
        assert_eq!(group_caption(1, "x", f32::INFINITY), "1 file · x · 0%");
        assert_eq!(group_caption(1, "x", f32::NEG_INFINITY), "1 file · x · 0%");
    }
}
