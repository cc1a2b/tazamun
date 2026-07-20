//! Did-you-mean matching for the command line.
//!
//! clap ships its own suggester, but it scores with Jaro above a 0.7 cut, and
//! Jaro is unkind to short words with a missing letter: `gi` against `gui`
//! scores 0.611, so `tazamun gi` was answered with a bare "unrecognized
//! subcommand" and no hint at all — for a one-character typo on the command
//! most people reach for first.
//!
//! This module scores the same candidates three ways and takes the best:
//! prefix (what a half-typed command looks like), subsequence (what a dropped
//! letter looks like), and Damerau-Levenshtein distance (what a mistyped or
//! transposed letter looks like — plain Levenshtein charges two for a swap,
//! which rejects `lcok` for `lock`). `gi` is a subsequence of `gui` *and* one
//! edit away, so it lands comfortably.
//!
//! Pure: no I/O, no clock, no allocation beyond the returned names. Every rule
//! here is exhaustively unit-testable and is tested below.

/// Longest edit distance still worth offering, by length of what was typed.
/// One-character input is excluded entirely: at that length everything is
/// within one edit of everything, and a wrong guess is worse than silence.
fn max_distance(typed_len: usize) -> usize {
    match typed_len {
        0..=1 => 0,
        2..=4 => 1,
        5..=7 => 2,
        _ => 3,
    }
}

/// How strongly `candidate` answers `typed`. Higher is better; `None` means
/// not close enough to offer. Case-insensitive — a shouted command is still
/// the command.
fn score(typed: &str, candidate: &str) -> Option<u32> {
    if typed.is_empty() || candidate.is_empty() {
        return None;
    }
    let t: Vec<char> = typed.chars().flat_map(char::to_lowercase).collect();
    let c: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();

    if t == c {
        return Some(1000);
    }
    // A prefix is the strongest signal: the user began typing the right thing.
    if c.starts_with(t.as_slice()) {
        // Prefer the shortest completion, so `sta` offers `start` over
        // `start-service` when both exist.
        return Some(900u32.saturating_sub(c.len().saturating_sub(t.len()) as u32));
    }
    let dist = damerau(&t, &c);
    let subseq = is_subsequence(&t, &c);
    // A dropped letter reads as a subsequence; allow it one extra edit of slack
    // so `gi` still reaches `gui` at length two.
    let budget = max_distance(t.len()) + usize::from(subseq);
    if dist <= budget && budget > 0 {
        // Closer is better, and a dropped letter is a much stronger signal than
        // a substituted one — `gi` means `gui`, not `gc`. The bonus is sized to
        // clear [`MARGIN`] so the weaker match is dropped rather than offered
        // alongside.
        let base = 800u32.saturating_sub((dist as u32).saturating_mul(100));
        return Some(base + if subseq { 60 } else { 0 });
    }
    None
}

/// True when every char of `t` appears in `c` in order (not necessarily
/// adjacent) — what a command with a letter left out looks like.
fn is_subsequence(t: &[char], c: &[char]) -> bool {
    let mut it = c.iter();
    t.iter().all(|ch| it.any(|x| x == ch))
}

/// Damerau-Levenshtein (optimal string alignment) over two char slices.
///
/// Plain Levenshtein charges two edits for a transposition, which is wrong for
/// the commonest typo there is: `lcok` is one slip of the fingers from `lock`,
/// not two, and at four characters a budget of one would have rejected it.
/// Three rolling rows, so the cost is the same order as the two-row form.
fn damerau(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let n = b.len();
    // `prev2` is the row before last, needed only for the transposition step.
    let mut prev2: Vec<usize> = vec![0; n + 1];
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(prev2[j - 2] + 1);
            }
            cur[j] = best;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

/// How far below the best a candidate may score and still be worth mentioning.
/// Genuine alternatives (`sta` → `start`, `status`) sit within a few points of
/// each other; an also-ran a whole edit worse is noise.
const MARGIN: u32 = 50;

/// The candidates worth suggesting for `typed`, best first, at most `limit`.
/// Empty when nothing is close — silence beats a confident wrong guess.
pub fn closest(typed: &str, candidates: &[&str], limit: usize) -> Vec<String> {
    let mut scored: Vec<(u32, &str)> = candidates
        .iter()
        .filter_map(|c| score(typed, c).map(|s| (s, *c)))
        .collect();
    // Sort by score descending, then by name so equal scores are deterministic
    // rather than dependent on the order the candidates arrived in.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let Some(&(best, _)) = scored.first() else {
        return Vec::new();
    };
    let cut = best.saturating_sub(MARGIN);
    scored
        .into_iter()
        .take_while(|(s, _)| *s >= cut)
        .take(limit)
        .map(|(_, c)| c.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real subcommand surface, so the tests answer the question that
    /// actually matters: what does this print for a typo of a real command?
    const CMDS: &[&str] = &[
        "init",
        "join",
        "start",
        "stop",
        "status",
        "lock",
        "unlock",
        "locks",
        "files",
        "peers",
        "history",
        "restore",
        "versions",
        "conflicts",
        "doctor",
        "config",
        "setup",
        "gui",
        "dashboard",
        "service",
        "update",
        "completions",
        "man",
    ];

    fn first(typed: &str) -> Option<String> {
        closest(typed, CMDS, 3).into_iter().next()
    }

    // ─── the bug that prompted this module ───────────────────────────────────

    #[test]
    fn gi_suggests_gui() {
        // clap's Jaro scored this 0.611 against a 0.7 cut and said nothing.
        assert_eq!(first("gi").as_deref(), Some("gui"));
    }

    #[test]
    fn other_one_letter_drops_are_caught() {
        assert_eq!(first("gu").as_deref(), Some("gui"));
        assert_eq!(first("stat").as_deref(), Some("status"));
        assert_eq!(first("docter").as_deref(), Some("doctor"));
        assert_eq!(first("confg").as_deref(), Some("config"));
        assert_eq!(first("verions").as_deref(), Some("versions"));
    }

    #[test]
    fn transpositions_and_wrong_letters_are_caught() {
        assert_eq!(first("satus").as_deref(), Some("status"));
        assert_eq!(first("lcok").as_deref(), Some("lock"));
        assert_eq!(first("joim").as_deref(), Some("join"));
        assert_eq!(first("serivce").as_deref(), Some("service"));
    }

    #[test]
    fn an_exact_name_scores_highest() {
        assert_eq!(first("gui").as_deref(), Some("gui"));
        assert_eq!(first("status").as_deref(), Some("status"));
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(first("GUI").as_deref(), Some("gui"));
        assert_eq!(first("Gi").as_deref(), Some("gui"));
        assert_eq!(first("STATUS").as_deref(), Some("status"));
    }

    // ─── prefixes ────────────────────────────────────────────────────────────

    #[test]
    fn a_prefix_prefers_the_shortest_completion() {
        // `lock` and `locks` both start with `lock`; the shorter wins.
        assert_eq!(first("lock").as_deref(), Some("lock"));
        // `sto` reaches `stop` before anything further away.
        assert_eq!(first("sto").as_deref(), Some("stop"));
    }

    #[test]
    fn a_prefix_beats_an_edit_of_the_same_length() {
        // `sta` is a prefix of `start`/`status` but one edit from `stop`.
        let got = closest("sta", CMDS, 3);
        assert!(
            got.first().is_some_and(|s| s.starts_with("sta")),
            "prefix should lead, got {got:?}"
        );
    }

    // ─── restraint ───────────────────────────────────────────────────────────

    #[test]
    fn a_single_character_offers_completions_but_never_guesses() {
        // At one character everything is within an edit of everything, so the
        // edit path is closed — but a prefix is still a real signal, and
        // completing it is useful rather than presumptuous.
        assert!(closest("x", CMDS, 3).is_empty(), "no command starts with x");
        assert!(closest("z", CMDS, 3).is_empty());
        assert_eq!(first("g").as_deref(), Some("gui"));
        assert!(
            closest("s", CMDS, 9).iter().all(|s| s.starts_with('s')),
            "a single letter must complete, never guess"
        );
    }

    #[test]
    fn nonsense_suggests_nothing() {
        assert!(closest("zzzzzzzz", CMDS, 3).is_empty());
        assert!(closest("qwertyuiop", CMDS, 3).is_empty());
    }

    #[test]
    fn empty_input_and_empty_candidates_are_handled() {
        assert!(closest("", CMDS, 3).is_empty());
        assert!(closest("gui", &[], 3).is_empty());
        assert!(closest("", &[], 3).is_empty());
    }

    #[test]
    fn a_zero_limit_returns_nothing() {
        assert!(closest("gi", CMDS, 0).is_empty());
    }

    #[test]
    fn the_limit_is_respected() {
        assert!(closest("stat", CMDS, 2).len() <= 2);
        assert!(closest("s", CMDS, 5).len() <= 5);
    }

    // ─── determinism and totality ────────────────────────────────────────────

    #[test]
    fn equal_scores_break_ties_by_name_not_input_order() {
        let a = closest("sto", CMDS, 5);
        let mut shuffled: Vec<&str> = CMDS.to_vec();
        shuffled.reverse();
        let b = closest("sto", &shuffled, 5);
        assert_eq!(a, b, "suggestion order must not depend on candidate order");
    }

    #[test]
    fn multibyte_input_does_not_panic_and_is_not_offered() {
        // Char-based throughout, so no slicing ever lands mid-codepoint.
        assert!(closest("مزامنة", CMDS, 3).is_empty());
        assert!(closest("gü", CMDS, 3).len() <= 3);
        let _ = closest("🙂🙂", CMDS, 3);
    }

    #[test]
    fn a_very_long_input_is_handled() {
        let long = "g".repeat(4096);
        assert!(closest(&long, CMDS, 3).is_empty());
    }

    // ─── the metrics themselves ──────────────────────────────────────────────

    #[test]
    fn damerau_matches_known_values_and_charges_one_for_a_swap() {
        let v = |s: &str| -> Vec<char> { s.chars().collect() };
        assert_eq!(damerau(&v("gi"), &v("gui")), 1);
        assert_eq!(damerau(&v("kitten"), &v("sitting")), 3);
        assert_eq!(damerau(&v(""), &v("abc")), 3);
        assert_eq!(damerau(&v("abc"), &v("")), 3);
        assert_eq!(damerau(&v("abc"), &v("abc")), 0);
        // The whole reason for Damerau over plain Levenshtein: a transposition
        // is one slip, not two.
        assert_eq!(damerau(&v("lcok"), &v("lock")), 1);
        assert_eq!(damerau(&v("ab"), &v("ba")), 1);
    }

    #[test]
    fn a_clearly_better_match_is_offered_alone() {
        // `gi` is a dropped letter from `gui` and a substitution from `gc`;
        // only the first is worth showing.
        assert_eq!(closest("gi", &["gui", "gc"], 3), vec!["gui".to_string()]);
    }

    #[test]
    fn subsequence_is_order_sensitive() {
        let v = |s: &str| -> Vec<char> { s.chars().collect() };
        assert!(is_subsequence(&v("gi"), &v("gui")));
        assert!(is_subsequence(&v("sts"), &v("status")));
        assert!(!is_subsequence(&v("ig"), &v("gui")));
        assert!(is_subsequence(&v(""), &v("gui")));
    }
}
