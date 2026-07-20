//! Multi-selection for the native GUI's session sidebar.
//!
//! The sidebar draws a flat list of sessions, each identified by a stable key
//! (its directory path). This module owns the standard desktop interaction on
//! top of that list and nothing else: a plain click replaces the selection,
//! Ctrl toggles one row, Shift takes the inclusive range from an anchor, and
//! Ctrl+Shift adds that range to what is already selected.
//!
//! [`Selection`] is pure `std` — no egui, no I/O, no clock — so every gesture
//! is a plain call over a `&[String]` of whatever the sidebar is showing right
//! now, and every branch is unit-testable on any host. The displayed slice is
//! passed in per call rather than stored, because it is the caller's filtered,
//! sorted view and it changes underneath us constantly. That is also why the
//! anchor is a *key*: see the field comment on [`Selection`].
//!
//! Two edge rules are decided here rather than left to emerge. A Shift-click
//! with no live anchor — never clicked, or the anchor's session was removed or
//! filtered away — degrades to a range of exactly the clicked row, so plain
//! Shift behaves as a plain click and Ctrl+Shift behaves as an additive click,
//! and it re-anchors there so the *next* Shift-click ranges properly. And
//! because the selection is keyed, a key repeated in the displayed slice is one
//! selectable item drawn twice: clicking either row selects both, an anchor
//! resolves to its first occurrence, [`ordered`](Selection::ordered) yields the
//! key once, and [`runs`](Selection::runs) — which describes rows, not keys —
//! covers every position the key occupies.

use std::collections::BTreeSet;

/// Which sessions are selected, plus the anchor that Shift-click ranges from.
#[derive(Debug, Default, Clone)]
pub struct Selection {
    /// `BTreeSet` over `HashSet`: a selection is at most one sidebar's worth of
    /// sessions, so lookup cost is irrelevant, and a sorted set iterates and
    /// prints the same on every run and every host. Determinism is cheap here
    /// and the rest of the crate depends on it everywhere state is compared.
    keys: BTreeSet<String>,
    /// The Shift-range origin, held as a key and never an index. This is the
    /// load-bearing design point: the displayed list is filtered and re-sorted
    /// under us and sessions come and go, so a stored index would keep pointing
    /// at whichever row slid into that slot and would silently select the wrong
    /// range. It is resolved against the live slice at click time and treated
    /// as absent the moment it stops resolving.
    anchor: Option<String>,
}

impl Selection {
    /// True when `key` is part of the current selection.
    pub fn is_selected(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// How many are selected.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The selected keys, in the order they appear in `keys`. Selected keys
    /// that are no longer displayed are omitted, and a key repeated in `keys`
    /// is yielded once, at its first position.
    pub fn ordered(&self, keys: &[String]) -> Vec<String> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut out: Vec<String> = Vec::new();
        for key in keys {
            if self.keys.contains(key.as_str()) && seen.insert(key.as_str()) {
                out.push(key.clone());
            }
        }
        out
    }

    /// Apply a click at index `idx` of the currently displayed `keys`, with the
    /// modifier state that accompanied it. Plain click replaces the selection
    /// with just that row and re-anchors; Ctrl toggles that row and re-anchors;
    /// Shift selects the inclusive range from the anchor to `idx` (replacing);
    /// Ctrl+Shift adds that range to the existing selection. An out-of-range
    /// `idx` or an empty `keys` is a no-op.
    pub fn click(&mut self, keys: &[String], idx: usize, ctrl: bool, shift: bool) {
        let Some(target) = keys.get(idx) else {
            return;
        };

        if shift {
            let resolved = self
                .anchor
                .as_deref()
                .and_then(|anchor| keys.iter().position(|key| key == anchor));
            // No live anchor degrades to the clicked row alone, in both
            // directions the range could have gone.
            let (lo, hi) = match resolved {
                Some(from) => (from.min(idx), from.max(idx)),
                None => (idx, idx),
            };

            if !ctrl {
                self.keys.clear();
            }
            for key in keys.get(lo..=hi).unwrap_or_default() {
                self.keys.insert(key.clone());
            }
            // Successive Shift-clicks re-range from the same origin, so the
            // anchor moves only when there was no usable one to begin with.
            if resolved.is_none() {
                self.anchor = Some(target.clone());
            }
            return;
        }

        if ctrl {
            if !self.keys.remove(target.as_str()) {
                self.keys.insert(target.clone());
            }
        } else {
            self.keys.clear();
            self.keys.insert(target.clone());
        }
        // Ctrl re-anchors whether it turned the row on or off, matching the
        // platform convention that the last touched row is the next origin.
        self.anchor = Some(target.clone());
    }

    /// Select every key in `keys`, anchoring at the first.
    pub fn select_all(&mut self, keys: &[String]) {
        self.keys.clear();
        for key in keys {
            self.keys.insert(key.clone());
        }
        self.anchor = keys.first().cloned();
    }

    /// Drop the whole selection and the anchor.
    pub fn clear(&mut self) {
        self.keys.clear();
        self.anchor = None;
    }

    /// Forget any selected key that is no longer in `keys` (a session was
    /// removed or filtered away), and drop a dangling anchor. Returns true when
    /// anything was actually removed — a shrunken selection *or* a dropped
    /// anchor, so the caller can treat it as one repaint signal.
    pub fn retain_existing(&mut self, keys: &[String]) -> bool {
        let live: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        let before = self.keys.len();
        self.keys.retain(|key| live.contains(key.as_str()));

        let dangling = self
            .anchor
            .as_deref()
            .is_some_and(|anchor| !live.contains(anchor));
        if dangling {
            self.anchor = None;
        }
        self.keys.len() != before || dangling
    }

    /// Contiguous runs of selected rows as inclusive `(first_idx, last_idx)`
    /// index pairs into `keys`, in ascending order. Feeds the margin braces the
    /// sidebar draws, so adjacent selected rows share one brace.
    pub fn runs(&self, keys: &[String]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut open: Option<usize> = None;
        for (i, key) in keys.iter().enumerate() {
            if self.keys.contains(key.as_str()) {
                if open.is_none() {
                    open = Some(i);
                }
            } else if let Some(first) = open.take() {
                out.push((first, i.saturating_sub(1)));
            }
        }
        if let Some(first) = open {
            out.push((first, keys.len().saturating_sub(1)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The sidebar's usual five sessions.
    fn abcde() -> Vec<String> {
        ks(&["a", "b", "c", "d", "e"])
    }

    #[test]
    fn default_selection_is_empty_with_no_anchor() {
        let s = Selection::default();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(!s.is_selected("a"));
        assert!(s.ordered(&abcde()).is_empty());
        assert!(s.runs(&abcde()).is_empty());
    }

    #[test]
    fn plain_click_replaces_and_anchors() {
        let keys = abcde();
        let mut s = Selection::default();

        s.click(&keys, 1, false, false);
        assert_eq!(s.ordered(&keys), ks(&["b"]));

        // A second plain click collapses the selection to just that row.
        s.click(&keys, 3, true, false);
        assert_eq!(s.ordered(&keys), ks(&["b", "d"]));
        s.click(&keys, 4, false, false);
        assert_eq!(s.ordered(&keys), ks(&["e"]));

        // ...and re-anchored there, so a Shift-click ranges from e.
        s.click(&keys, 2, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c", "d", "e"]));
    }

    #[test]
    fn ctrl_click_toggles_on_and_off() {
        let keys = abcde();
        let mut s = Selection::default();

        s.click(&keys, 1, true, false);
        assert_eq!(s.ordered(&keys), ks(&["b"]));

        // Ctrl adds without disturbing what is already selected.
        s.click(&keys, 3, true, false);
        assert_eq!(s.ordered(&keys), ks(&["b", "d"]));

        // Ctrl on a selected row turns it back off, leaving the rest.
        s.click(&keys, 1, true, false);
        assert_eq!(s.ordered(&keys), ks(&["d"]));
        s.click(&keys, 3, true, false);
        assert!(s.is_empty());
    }

    #[test]
    fn ctrl_click_moves_the_anchor() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 3, true, false);
        assert_eq!(s.ordered(&keys), ks(&["a", "d"]));

        // Ranging now starts at d, not at the original plain click on a.
        s.click(&keys, 4, false, true);
        assert_eq!(s.ordered(&keys), ks(&["d", "e"]));
    }

    #[test]
    fn ctrl_click_reanchors_even_when_deselecting() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 2, true, false);
        s.click(&keys, 2, true, false); // toggled c back off
        assert_eq!(s.ordered(&keys), ks(&["a"]));

        // c is unselected but still the anchor.
        s.click(&keys, 4, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c", "d", "e"]));
    }

    #[test]
    fn shift_range_forward_from_anchor() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 1, false, false);
        s.click(&keys, 3, false, true);
        assert_eq!(s.ordered(&keys), ks(&["b", "c", "d"]));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn shift_range_backward_from_anchor() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 3, false, false);
        s.click(&keys, 1, false, true);
        assert_eq!(s.ordered(&keys), ks(&["b", "c", "d"]));

        // A range onto the anchor itself is the single anchored row.
        s.click(&keys, 3, false, true);
        assert_eq!(s.ordered(&keys), ks(&["d"]));
    }

    #[test]
    fn shift_range_replaces_prior_selection() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 4, true, false);
        assert_eq!(s.ordered(&keys), ks(&["a", "e"]));

        // Anchored at e; the range wipes a rather than keeping it.
        s.click(&keys, 2, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c", "d", "e"]));
    }

    #[test]
    fn successive_shift_clicks_keep_the_same_anchor() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 2, false, false);

        s.click(&keys, 4, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c", "d", "e"]));

        // Still ranging from c, now in the other direction.
        s.click(&keys, 0, false, true);
        assert_eq!(s.ordered(&keys), ks(&["a", "b", "c"]));

        s.click(&keys, 3, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c", "d"]));
    }

    #[test]
    fn shift_without_anchor_acts_as_plain_click() {
        let keys = abcde();
        let mut s = Selection::default();

        s.click(&keys, 2, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c"]));

        // It re-anchored, so the follow-up Shift-click ranges normally.
        s.click(&keys, 4, false, true);
        assert_eq!(s.ordered(&keys), ks(&["c", "d", "e"]));
    }

    #[test]
    fn ctrl_shift_without_anchor_adds_only_the_clicked_row() {
        let keys = ks(&["a", "b", "c", "d"]);
        let mut s = Selection::default();
        s.click(&keys, 0, true, false);
        s.click(&keys, 2, true, false);
        assert_eq!(s.ordered(&keys), ks(&["a", "c"]));

        // c disappears from the sidebar, taking the anchor with it.
        let shrunk = ks(&["a", "b", "d"]);
        assert!(s.retain_existing(&shrunk));
        assert_eq!(s.ordered(&shrunk), ks(&["a"]));

        // Ctrl+Shift with a dead anchor adds the clicked row; it must not
        // replace the surviving selection the way plain Shift would.
        s.click(&shrunk, 2, true, true);
        assert_eq!(s.ordered(&shrunk), ks(&["a", "d"]));

        // ...and it re-anchored at d.
        s.click(&shrunk, 0, false, true);
        assert_eq!(s.ordered(&shrunk), ks(&["a", "b", "d"]));
    }

    #[test]
    fn shift_after_anchor_key_removed_degrades_and_reanchors() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 1, false, false); // anchor b
        assert_eq!(s.ordered(&keys), ks(&["b"]));

        // b is gone; the anchor no longer resolves.
        let shrunk = ks(&["a", "c", "d", "e"]);
        assert!(s.retain_existing(&shrunk));
        assert!(s.is_empty());

        s.click(&shrunk, 3, false, true);
        assert_eq!(s.ordered(&shrunk), ks(&["e"]));

        // Re-anchored at e, so this ranges rather than degrading again.
        s.click(&shrunk, 0, false, true);
        assert_eq!(s.ordered(&shrunk), ks(&["a", "c", "d", "e"]));
    }

    #[test]
    fn ctrl_shift_adds_range_to_existing_selection() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 3, true, false); // anchor d
        assert_eq!(s.ordered(&keys), ks(&["a", "d"]));

        s.click(&keys, 1, true, true); // range b..=d, additive
        assert_eq!(s.ordered(&keys), ks(&["a", "b", "c", "d"]));
    }

    #[test]
    fn ctrl_shift_keeps_anchor_for_further_ranges() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 3, false, false); // anchor d
        s.click(&keys, 1, true, true);
        assert_eq!(s.ordered(&keys), ks(&["b", "c", "d"]));

        // Still anchored at d, so this extends forward from d.
        s.click(&keys, 4, true, true);
        assert_eq!(s.ordered(&keys), ks(&["b", "c", "d", "e"]));

        // A plain Shift proves the anchor never left d.
        s.click(&keys, 4, false, true);
        assert_eq!(s.ordered(&keys), ks(&["d", "e"]));
    }

    #[test]
    fn anchor_is_a_key_not_an_index() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 1, false, false); // anchor b, at index 1

        // a and c are filtered away; b now sits at index 0.
        let filtered = ks(&["b", "d", "e"]);
        s.click(&filtered, 2, false, true);
        // Key-anchored: b..=e. An index-anchored anchor would have started at
        // d and produced just {d, e}.
        assert_eq!(s.ordered(&filtered), ks(&["b", "d", "e"]));
    }

    #[test]
    fn click_out_of_range_is_a_no_op() {
        let keys = ks(&["a", "b", "c"]);
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);

        for (ctrl, shift) in [(false, false), (true, false), (false, true), (true, true)] {
            s.click(&keys, 3, ctrl, shift);
            s.click(&keys, usize::MAX, ctrl, shift);
        }
        assert_eq!(s.ordered(&keys), ks(&["a"]));

        // The anchor was left alone too.
        s.click(&keys, 2, false, true);
        assert_eq!(s.ordered(&keys), ks(&["a", "b", "c"]));
    }

    #[test]
    fn click_on_empty_keys_is_a_no_op() {
        let empty: Vec<String> = Vec::new();
        let mut s = Selection::default();
        for (ctrl, shift) in [(false, false), (true, false), (false, true), (true, true)] {
            s.click(&empty, 0, ctrl, shift);
        }
        assert!(s.is_empty());

        let keys = ks(&["a", "b"]);
        s.click(&keys, 1, false, false);
        s.click(&empty, 0, false, false);
        assert_eq!(s.ordered(&keys), ks(&["b"]));
    }

    #[test]
    fn select_all_then_clear() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 2, false, false);

        s.select_all(&keys);
        assert_eq!(s.len(), 5);
        assert_eq!(s.ordered(&keys), keys);
        assert!(keys.iter().all(|k| s.is_selected(k)));
        assert_eq!(s.runs(&keys), vec![(0, 4)]);

        // Anchored at the first row, so Shift ranges down from a.
        s.click(&keys, 1, false, true);
        assert_eq!(s.ordered(&keys), ks(&["a", "b"]));

        s.select_all(&keys);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.runs(&keys).is_empty());

        // clear() dropped the anchor, so Shift degrades to a plain click.
        s.click(&keys, 3, false, true);
        assert_eq!(s.ordered(&keys), ks(&["d"]));
    }

    #[test]
    fn select_all_on_empty_keys_clears() {
        let keys = abcde();
        let mut s = Selection::default();
        s.select_all(&keys);
        s.select_all(&[]);
        assert!(s.is_empty());

        // No anchor survives an empty select-all either.
        s.click(&keys, 4, false, true);
        assert_eq!(s.ordered(&keys), ks(&["e"]));
    }

    #[test]
    fn select_all_dedupes_repeated_keys() {
        let keys = ks(&["x", "y", "x"]);
        let mut s = Selection::default();
        s.select_all(&keys);
        assert_eq!(s.len(), 2);
        assert_eq!(s.ordered(&keys), ks(&["x", "y"]));
        assert_eq!(s.runs(&keys), vec![(0, 2)]);
    }

    #[test]
    fn retain_existing_removes_missing_and_reports_true() {
        let keys = abcde();
        let mut s = Selection::default();
        s.select_all(&keys);

        let shrunk = ks(&["a", "c", "e"]);
        assert!(s.retain_existing(&shrunk));
        assert_eq!(s.len(), 3);
        assert_eq!(s.ordered(&shrunk), ks(&["a", "c", "e"]));
        assert!(!s.is_selected("b"));
        assert!(!s.is_selected("d"));
    }

    #[test]
    fn retain_existing_with_nothing_missing_reports_false() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 1, false, false);
        s.click(&keys, 3, true, false);

        assert!(!s.retain_existing(&keys));
        assert_eq!(s.ordered(&keys), ks(&["b", "d"]));

        // A grown list still removes nothing.
        let grown = ks(&["a", "b", "c", "d", "e", "f"]);
        assert!(!s.retain_existing(&grown));
        assert_eq!(s.ordered(&grown), ks(&["b", "d"]));
    }

    #[test]
    fn retain_existing_removing_all_empties_selection() {
        let keys = abcde();
        let mut s = Selection::default();
        s.select_all(&keys);

        assert!(s.retain_existing(&[]));
        assert!(s.is_empty());
        // Idempotent: a second sweep has nothing left to remove.
        assert!(!s.retain_existing(&[]));
    }

    #[test]
    fn retain_existing_drops_dangling_anchor_alone() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 1, true, false);
        s.click(&keys, 1, true, false); // b deselected but still the anchor
        assert_eq!(s.ordered(&keys), ks(&["a"]));

        // The selection set is untouched, yet b vanished from the list — a
        // dropped anchor alone still reports true so the caller repaints.
        let shrunk = ks(&["a", "c", "d", "e"]);
        assert!(s.retain_existing(&shrunk));
        assert_eq!(s.ordered(&shrunk), ks(&["a"]));

        s.click(&shrunk, 3, false, true);
        assert_eq!(s.ordered(&shrunk), ks(&["e"]));
    }

    #[test]
    fn retain_existing_keeps_a_live_anchor() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 1, true, false);
        s.click(&keys, 1, true, false); // anchor b, not selected

        assert!(!s.retain_existing(&keys));
        s.click(&keys, 2, false, true);
        assert_eq!(s.ordered(&keys), ks(&["b", "c"]));
    }

    #[test]
    fn ordered_follows_display_order_not_insertion_order() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 4, true, false);
        s.click(&keys, 0, true, false);
        s.click(&keys, 2, true, false);
        assert_eq!(s.ordered(&keys), ks(&["a", "c", "e"]));

        // Re-sorting the sidebar re-orders the result, with no re-clicking.
        let reversed = ks(&["e", "d", "c", "b", "a"]);
        assert_eq!(s.ordered(&reversed), ks(&["e", "c", "a"]));
    }

    #[test]
    fn ordered_skips_keys_absent_from_display() {
        let keys = abcde();
        let mut s = Selection::default();
        s.select_all(&keys);

        let filtered = ks(&["b", "d"]);
        assert_eq!(s.ordered(&filtered), ks(&["b", "d"]));
        // The hidden keys are still selected; ordered() just does not show them.
        assert_eq!(s.len(), 5);
        assert!(s.is_selected("a"));
        assert!(s.ordered(&[]).is_empty());
    }

    #[test]
    fn ordered_yields_a_repeated_key_once() {
        let keys = ks(&["x", "y", "x", "z", "x"]);
        let mut s = Selection::default();
        s.click(&keys, 2, false, false);
        assert_eq!(s.len(), 1);
        assert_eq!(s.ordered(&keys), ks(&["x"]));

        s.click(&keys, 3, true, false);
        assert_eq!(s.ordered(&keys), ks(&["x", "z"]));
    }

    #[test]
    fn duplicate_key_rows_select_together() {
        let keys = ks(&["x", "y", "x"]);
        let mut s = Selection::default();

        // One key drawn twice is one selectable item: clicking either row
        // lights both.
        s.click(&keys, 2, false, false);
        assert!(s.is_selected("x"));
        assert_eq!(s.len(), 1);
        assert_eq!(s.runs(&keys), vec![(0, 0), (2, 2)]);

        // Ctrl on the *other* occurrence toggles the same item back off.
        s.click(&keys, 0, true, false);
        assert!(s.is_empty());
        assert!(s.runs(&keys).is_empty());
    }

    #[test]
    fn duplicate_anchor_resolves_to_its_first_occurrence() {
        let keys = ks(&["x", "y", "x", "z"]);
        let mut s = Selection::default();
        s.click(&keys, 2, false, false); // anchored on key "x"

        // Anchor resolves to index 0, so the range covers y. Resolving to the
        // clicked index 2 instead would have selected only {x, z}.
        s.click(&keys, 3, false, true);
        assert_eq!(s.ordered(&keys), ks(&["x", "y", "z"]));
        assert!(s.is_selected("y"));
    }

    #[test]
    fn runs_empty_selection_and_empty_keys() {
        let keys = abcde();
        let mut s = Selection::default();
        assert!(s.runs(&keys).is_empty());
        assert!(s.runs(&[]).is_empty());

        s.select_all(&keys);
        // Nothing displayed means nothing to brace.
        assert!(s.runs(&[]).is_empty());
    }

    #[test]
    fn runs_single_row() {
        let keys = abcde();
        let mut s = Selection::default();

        s.click(&keys, 0, false, false);
        assert_eq!(s.runs(&keys), vec![(0, 0)]);

        s.click(&keys, 2, false, false);
        assert_eq!(s.runs(&keys), vec![(2, 2)]);

        // A run that ends on the final row closes at keys.len() - 1.
        s.click(&keys, 4, false, false);
        assert_eq!(s.runs(&keys), vec![(4, 4)]);
    }

    #[test]
    fn runs_all_selected_is_one_run() {
        let keys = abcde();
        let mut s = Selection::default();
        s.select_all(&keys);
        assert_eq!(s.runs(&keys), vec![(0, 4)]);

        let single = ks(&["only"]);
        let mut s = Selection::default();
        s.select_all(&single);
        assert_eq!(s.runs(&single), vec![(0, 0)]);
    }

    #[test]
    fn runs_alternating_rows() {
        let keys = abcde();
        let mut s = Selection::default();
        s.click(&keys, 0, true, false);
        s.click(&keys, 2, true, false);
        s.click(&keys, 4, true, false);
        assert_eq!(s.runs(&keys), vec![(0, 0), (2, 2), (4, 4)]);
    }

    #[test]
    fn runs_two_separated_blocks() {
        let keys = ks(&["a", "b", "c", "d", "e", "f", "g"]);
        let mut s = Selection::default();
        s.click(&keys, 0, false, false);
        s.click(&keys, 1, false, true); // a..=b
        s.click(&keys, 4, true, false);
        s.click(&keys, 6, true, true); // e..=g, additive
        assert_eq!(s.runs(&keys), vec![(0, 1), (4, 6)]);
        assert_eq!(s.ordered(&keys), ks(&["a", "b", "e", "f", "g"]));
    }

    #[test]
    fn runs_ignore_selected_keys_absent_from_keys() {
        let keys = abcde();
        let mut s = Selection::default();
        s.select_all(&keys);

        // Selection still holds b, c, d; runs describes only displayed rows and
        // must not assume the selection is sorted or fully present. Hiding the
        // rows between a and e makes them adjacent, so they brace as one run.
        let filtered = ks(&["a", "e"]);
        assert_eq!(s.runs(&filtered), vec![(0, 1)]);

        // An unselected row between them splits the brace again.
        let gapped = ks(&["a", "unknown", "e"]);
        assert_eq!(s.runs(&gapped), vec![(0, 0), (2, 2)]);

        let unknown = ks(&["zz", "yy"]);
        assert!(s.runs(&unknown).is_empty());
    }

    #[test]
    fn runs_are_ascending_and_within_bounds() {
        let keys = ks(&["a", "b", "c", "d", "e", "f"]);
        let mut s = Selection::default();
        s.click(&keys, 5, true, false);
        s.click(&keys, 1, true, false);
        s.click(&keys, 2, true, false);

        let runs = s.runs(&keys);
        assert_eq!(runs, vec![(1, 2), (5, 5)]);
        assert!(runs.iter().all(|&(lo, hi)| lo <= hi && hi < keys.len()));
        assert!(runs.windows(2).all(|w| w[0].1 < w[1].0));
    }

    #[test]
    fn is_empty_tracks_len() {
        let keys = abcde();
        let mut s = Selection::default();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);

        s.click(&keys, 0, true, false);
        assert!(!s.is_empty());
        assert_eq!(s.len(), 1);

        s.click(&keys, 0, true, false);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);

        s.select_all(&keys);
        assert!(!s.is_empty());
        assert_eq!(s.len(), keys.len());
    }
}
