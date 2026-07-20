//! Pure sync-scope policy: what enters the index and what is held out of it
//! (zero I/O).
//!
//! One [`IgnoreSet`] answers, for any sanitized relative path, whether this
//! node should carry it: [`Verdict::Sync`], or a held verdict naming why —
//! the built-in junk preset, a `.tazamunignore` rule, per-node selective
//! sync, or the size ceiling. The daemon consults it on every watcher event,
//! at genesis/startup scan, and before pulling a remote record; this module
//! itself never touches the filesystem (the daemon reads `.tazamunignore`
//! and hands the text in), so the whole policy is exhaustively unit-testable
//! and fuzzable.
//!
//! Invariant (Golden-Invariant corollary): a held path is **left alone** —
//! local files are neither quarantined nor published; remote records are
//! acknowledged but never materialized. Holding never deletes anything, and
//! [`IGNORE_FILE`] itself is always synced so the session agrees on the rules.

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::state::RelPath;

/// The in-folder ignore file (gitignore syntax). Synced like any other file
/// and exempt from every hold rule, including itself.
pub const IGNORE_FILE: &str = ".tazamunignore";

/// Editor droppings and OS metadata nobody means to sync. Applied *before*
/// user rules, so a `.tazamunignore` line can override any of it (gitignore
/// semantics: the last matching rule wins, and `!pattern` re-includes).
pub const JUNK_PATTERNS: &[&str] = &[
    // vim: foo.swp / .foo.txt.swp / swap siblings
    "*.swp",
    "*.swo",
    "*.swx",
    ".*.sw?",
    // generic editor backup droppings
    "*~",
    // emacs: lock + autosave (the leading # must be escaped or gitignore
    // syntax reads the whole line as a comment)
    ".#*",
    "\\#*#",
    // Microsoft Office owner-lock files
    "~$*",
    // macOS metadata
    ".DS_Store",
    "._*",
    ".AppleDouble",
    // Windows metadata + NTFS mark-of-the-web residue copied as plain files
    "Thumbs.db",
    "desktop.ini",
    "*:Zone.Identifier",
];

/// Why a path is not carried by this node. Every variant is a *hold*: the
/// bytes involved are left exactly where they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Carried: index, publish, pull, and enforce as usual.
    Sync,
    /// Matched the built-in junk preset (`junk-filter on`).
    Junk,
    /// Matched a `.tazamunignore` rule.
    UserIgnored,
    /// Outside `sync-only`, or under a `sync-skip` subtree.
    Skipped,
    /// Larger than the `max-file-size` ceiling. Only returned when a size is
    /// known (local publish candidates and remote records carry one).
    TooLarge { size: u64, cap: u64 },
}

impl Verdict {
    pub fn is_sync(&self) -> bool {
        matches!(self, Verdict::Sync)
    }

    /// Short reason text for `status` listings and log lines.
    pub fn reason(&self) -> String {
        match self {
            Verdict::Sync => "synced".to_string(),
            Verdict::Junk => "junk filter (built-in preset; see .tazamunignore docs)".to_string(),
            Verdict::UserIgnored => format!("ignored by {IGNORE_FILE}"),
            Verdict::Skipped => "outside this node's selective-sync scope".to_string(),
            Verdict::TooLarge { size, cap } => {
                format!("file is {size} bytes, over the {cap}-byte max-file-size ceiling")
            }
        }
    }
}

/// The compiled sync-scope policy. Build once from config + the ignore-file
/// text; rebuild whenever either changes. Matching is pure.
pub struct IgnoreSet {
    matcher: Option<Gitignore>,
    /// `sync-only` subtree (empty = everything), normalized without slashes.
    only: Option<String>,
    /// `sync-skip` subtrees, normalized.
    skip: Vec<String>,
    /// `max-file-size` in bytes; 0 = unlimited.
    max_file_size: u64,
    /// How many trailing lines of the matcher are user lines (for telling
    /// Junk apart from UserIgnored on a match).
    junk_enabled: bool,
    user_matcher: Option<Gitignore>,
}

impl IgnoreSet {
    /// Compiles the policy. `user_rules` is the raw text of `.tazamunignore`
    /// (empty string when the file does not exist); malformed glob lines are
    /// skipped exactly as git skips them — never a hard error.
    pub fn build(
        user_rules: &str,
        junk_filter: bool,
        sync_only: &str,
        sync_skip: &str,
        max_file_size: u64,
    ) -> Self {
        // Combined matcher decides the verdict (junk first, user rules after,
        // so user rules win ties per gitignore last-match semantics); the
        // user-only matcher attributes a hit to the right variant.
        let combined = {
            let mut b = GitignoreBuilder::new("");
            if junk_filter {
                for p in JUNK_PATTERNS {
                    let _ = b.add_line(None, p);
                }
            }
            for line in user_rules.lines() {
                let _ = b.add_line(None, line);
            }
            b.build().ok()
        };
        let user_only = {
            let mut b = GitignoreBuilder::new("");
            for line in user_rules.lines() {
                let _ = b.add_line(None, line);
            }
            b.build().ok()
        };
        Self {
            matcher: combined,
            only: normalize_subtree(sync_only),
            skip: sync_skip
                .split(',')
                .filter_map(normalize_subtree_str)
                .collect(),
            max_file_size,
            junk_enabled: junk_filter,
            user_matcher: user_only,
        }
    }

    /// A policy that carries everything (the pre-P11 behavior).
    pub fn carry_all() -> Self {
        Self::build("", false, "", "", 0)
    }

    /// The verdict for a sanitized relative path. `size` is supplied where a
    /// size is known (publish candidates, remote records); path-only checks
    /// (watch events before stat) pass `None` and re-check at publish time.
    pub fn verdict(&self, rel: &RelPath, size: Option<u64>) -> Verdict {
        let path = rel.as_str();
        // The ignore file itself is the session's shared contract: always
        // synced, never junk, never skipped, never size-held.
        if path == IGNORE_FILE {
            return Verdict::Sync;
        }
        // Selective sync first: scope is per-node topology, deliberate and
        // coarse, so it wins over content rules.
        if let Some(only) = &self.only
            && !in_subtree(path, only)
        {
            return Verdict::Skipped;
        }
        if self.skip.iter().any(|s| in_subtree(path, s)) {
            return Verdict::Skipped;
        }
        // Content rules: the combined matcher answers, the user-only matcher
        // attributes. A user `!rule` re-include beats the junk preset because
        // the user lines are added after the junk lines (last match wins).
        if let Some(m) = &self.matcher
            && m.matched_path_or_any_parents(path, false).is_ignore()
        {
            let user_hit = self
                .user_matcher
                .as_ref()
                .is_some_and(|u| u.matched_path_or_any_parents(path, false).is_ignore());
            return if user_hit {
                Verdict::UserIgnored
            } else if self.junk_enabled {
                Verdict::Junk
            } else {
                // Unreachable in practice (combined == user when junk is off),
                // but attribute honestly rather than panic.
                Verdict::UserIgnored
            };
        }
        if self.max_file_size > 0
            && let Some(size) = size
            && size > self.max_file_size
        {
            return Verdict::TooLarge {
                size,
                cap: self.max_file_size,
            };
        }
        Verdict::Sync
    }
}

/// Normalizes a subtree config value: trims whitespace and slashes; empty →
/// `None`. Values are matched against sanitized relative paths, so no further
/// validation is needed — a nonsense subtree simply matches nothing.
fn normalize_subtree(value: &str) -> Option<String> {
    normalize_subtree_str(value)
}

fn normalize_subtree_str(value: &str) -> Option<String> {
    let v = value.trim().trim_matches('/').to_string();
    if v.is_empty() { None } else { Some(v) }
}

/// Whether `path` is `subtree` itself or inside it (segment-aware: `docs`
/// contains `docs/a.txt` but not `docs2/a.txt`).
fn in_subtree(path: &str, subtree: &str) -> bool {
    path == subtree
        || path
            .strip_prefix(subtree)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::index::sanitize_rel_path;

    fn rel(s: &str) -> RelPath {
        sanitize_rel_path(s).expect("test path is valid")
    }

    fn v(set: &IgnoreSet, path: &str) -> Verdict {
        set.verdict(&rel(path), None)
    }

    #[test]
    fn junk_preset_catches_the_real_world_droppings() {
        let set = IgnoreSet::build("", true, "", "", 0);
        // The exact junk the field testing hit: vim dot-swaps and NTFS ADS
        // residue, plus the usual OS metadata.
        for junk in [
            ".readme.txt.swp",
            "notes.swp",
            "a/b/.deep.md.swo",
            "draft~",
            ".#lockfile",
            "#autosave#",
            "~$report.docx",
            ".DS_Store",
            "sub/.DS_Store",
            "._resource",
            "Thumbs.db",
            "desktop.ini",
            "photo.jpg:Zone.Identifier",
        ] {
            assert_eq!(v(&set, junk), Verdict::Junk, "{junk} should be junk");
        }
        // Ordinary files pass.
        for keep in ["readme.txt", "a/b/c.rs", "swp.txt", "DS_Store.md"] {
            assert_eq!(v(&set, keep), Verdict::Sync, "{keep} should sync");
        }
    }

    #[test]
    fn junk_filter_off_carries_everything() {
        let set = IgnoreSet::build("", false, "", "", 0);
        assert_eq!(v(&set, ".DS_Store"), Verdict::Sync);
        assert_eq!(v(&set, "x.swp"), Verdict::Sync);
    }

    #[test]
    fn user_rules_follow_gitignore_semantics() {
        let rules = "\
# comment lines and blanks are ignored

*.log
build/
!keep.log
docs/**/draft.md
/rooted.txt
";
        let set = IgnoreSet::build(rules, false, "", "", 0);
        assert_eq!(v(&set, "app.log"), Verdict::UserIgnored);
        assert_eq!(v(&set, "sub/deep/app.log"), Verdict::UserIgnored);
        // Negation re-includes.
        assert_eq!(v(&set, "keep.log"), Verdict::Sync);
        // A directory rule holds everything under it.
        assert_eq!(v(&set, "build/out/bin.o"), Verdict::UserIgnored);
        // ** spans directories.
        assert_eq!(v(&set, "docs/a/b/draft.md"), Verdict::UserIgnored);
        assert_eq!(v(&set, "docs/final.md"), Verdict::Sync);
        // A rooted pattern matches only at the root.
        assert_eq!(v(&set, "rooted.txt"), Verdict::UserIgnored);
        assert_eq!(v(&set, "sub/rooted.txt"), Verdict::Sync);
    }

    #[test]
    fn user_negation_overrides_the_junk_preset() {
        // The user explicitly wants .DS_Store synced: their rule wins.
        let set = IgnoreSet::build("!.DS_Store\n", true, "", "", 0);
        assert_eq!(v(&set, ".DS_Store"), Verdict::Sync);
        // Other junk is still junk.
        assert_eq!(v(&set, "x.swp"), Verdict::Junk);
    }

    #[test]
    fn matches_are_attributed_to_the_right_source() {
        let set = IgnoreSet::build("*.log\n", true, "", "", 0);
        assert_eq!(v(&set, "a.log"), Verdict::UserIgnored);
        assert_eq!(v(&set, "a.swp"), Verdict::Junk);
    }

    #[test]
    fn selective_sync_only_and_skip() {
        // only docs/: everything else is Skipped.
        let set = IgnoreSet::build("", false, "docs", "", 0);
        assert_eq!(v(&set, "docs/a.md"), Verdict::Sync);
        assert_eq!(v(&set, "docs"), Verdict::Sync);
        assert_eq!(v(&set, "src/main.rs"), Verdict::Skipped);
        assert_eq!(v(&set, "docs2/a.md"), Verdict::Skipped, "segment-aware");

        // skip renders/,tmp: those subtrees are Skipped, the rest syncs.
        let set = IgnoreSet::build("", false, "", "renders, tmp/", 0);
        assert_eq!(v(&set, "renders/big.exr"), Verdict::Skipped);
        assert_eq!(v(&set, "tmp/x"), Verdict::Skipped);
        assert_eq!(v(&set, "docs/a.md"), Verdict::Sync);
        assert_eq!(v(&set, "renders.txt"), Verdict::Sync, "segment-aware");
    }

    #[test]
    fn size_ceiling_holds_only_when_size_is_known() {
        let set = IgnoreSet::build("", false, "", "", 1000);
        assert_eq!(
            set.verdict(&rel("big.bin"), Some(1001)),
            Verdict::TooLarge {
                size: 1001,
                cap: 1000
            }
        );
        assert_eq!(set.verdict(&rel("big.bin"), Some(1000)), Verdict::Sync);
        // Path-only checks cannot size-hold; the publish path re-checks.
        assert_eq!(set.verdict(&rel("big.bin"), None), Verdict::Sync);
        // 0 = unlimited.
        let set = IgnoreSet::build("", false, "", "", 0);
        assert_eq!(set.verdict(&rel("big.bin"), Some(u64::MAX)), Verdict::Sync);
    }

    #[test]
    fn the_ignore_file_itself_is_always_synced() {
        // Even a hostile ruleset cannot hold the shared contract back.
        let set = IgnoreSet::build(".tazamunignore\n*\n", true, "docs", "", 1);
        assert_eq!(v(&set, IGNORE_FILE), Verdict::Sync);
        assert_eq!(set.verdict(&rel(IGNORE_FILE), Some(999_999)), Verdict::Sync);
    }

    #[test]
    fn precedence_scope_beats_content_rules() {
        // A path outside the sync-only scope reports Skipped even if it would
        // also match junk — scope is the coarser, more explanatory answer.
        let set = IgnoreSet::build("", true, "docs", "", 0);
        assert_eq!(v(&set, "src/.DS_Store"), Verdict::Skipped);
        assert_eq!(v(&set, "docs/.DS_Store"), Verdict::Junk);
    }

    #[test]
    fn garbage_rules_never_panic_and_never_hard_fail() {
        // Malformed globs are skipped like git does; binary noise is fine.
        let set = IgnoreSet::build("[[[\n***/\x07\nvalid.txt\n", true, "", "", 0);
        assert_eq!(v(&set, "valid.txt"), Verdict::UserIgnored);
        assert_eq!(v(&set, "other.txt"), Verdict::Sync);
    }

    #[test]
    fn carry_all_is_the_pre_p11_behavior() {
        let set = IgnoreSet::carry_all();
        for p in [".DS_Store", "x.swp", "renders/huge.bin", "a.log"] {
            assert_eq!(v(&set, p), Verdict::Sync);
        }
        assert_eq!(set.verdict(&rel("x"), Some(u64::MAX)), Verdict::Sync);
    }

    #[test]
    fn reasons_read_like_explanations() {
        assert!(Verdict::Junk.reason().contains("junk"));
        assert!(Verdict::UserIgnored.reason().contains(IGNORE_FILE));
        assert!(Verdict::Skipped.reason().contains("selective-sync"));
        assert!(
            Verdict::TooLarge { size: 9, cap: 5 }
                .reason()
                .contains("max-file-size")
        );
    }
}
