//! Version vectors keyed by canonical endpoint-id strings.
//!
//! Invariant: pure data structure — no I/O, no clocks; `compare` is a total
//! function over the four causality outcomes.

use std::collections::BTreeMap;

/// A version vector: canonical endpoint id (lowercase hex) → counter.
pub type VClock = BTreeMap<String, u64>;

/// Causal relation of one clock to another, from the first argument's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causality {
    Equal,
    /// The first clock strictly dominates the second (happened after).
    After,
    /// The second clock strictly dominates the first (happened before).
    Before,
    Concurrent,
}

/// Increments `id`'s component in place.
pub fn inc(vv: &mut VClock, id: &str) {
    let slot = vv.entry(id.to_string()).or_insert(0);
    *slot = slot.saturating_add(1);
}

/// Component-wise maximum of two clocks.
pub fn merge(a: &VClock, b: &VClock) -> VClock {
    let mut out = a.clone();
    for (k, v) in b {
        let slot = out.entry(k.clone()).or_insert(0);
        if *v > *slot {
            *slot = *v;
        }
    }
    out
}

/// Compares `a` against `b`.
pub fn compare(a: &VClock, b: &VClock) -> Causality {
    let mut a_gt = false;
    let mut b_gt = false;
    for k in a.keys().chain(b.keys()) {
        let av = a.get(k).copied().unwrap_or(0);
        let bv = b.get(k).copied().unwrap_or(0);
        if av > bv {
            a_gt = true;
        }
        if bv > av {
            b_gt = true;
        }
        if a_gt && b_gt {
            return Causality::Concurrent;
        }
    }
    match (a_gt, b_gt) {
        (false, false) => Causality::Equal,
        (true, false) => Causality::After,
        (false, true) => Causality::Before,
        (true, true) => Causality::Concurrent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vv(pairs: &[(&str, u64)]) -> VClock {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn equal_clocks() {
        assert_eq!(
            compare(&vv(&[("a", 1), ("b", 2)]), &vv(&[("a", 1), ("b", 2)])),
            Causality::Equal
        );
        assert_eq!(compare(&VClock::new(), &VClock::new()), Causality::Equal);
        // A zero entry is indistinguishable from an absent one.
        assert_eq!(compare(&vv(&[("a", 0)]), &VClock::new()), Causality::Equal);
    }

    #[test]
    fn after_and_before() {
        let older = vv(&[("a", 1)]);
        let newer = vv(&[("a", 2)]);
        assert_eq!(compare(&newer, &older), Causality::After);
        assert_eq!(compare(&older, &newer), Causality::Before);
        assert_eq!(
            compare(&vv(&[("a", 1), ("b", 1)]), &vv(&[("a", 1)])),
            Causality::After
        );
    }

    #[test]
    fn concurrent_detected() {
        let x = vv(&[("a", 2), ("b", 1)]);
        let y = vv(&[("a", 1), ("b", 2)]);
        assert_eq!(compare(&x, &y), Causality::Concurrent);
        assert_eq!(compare(&y, &x), Causality::Concurrent);
        assert_eq!(
            compare(&vv(&[("a", 1)]), &vv(&[("b", 1)])),
            Causality::Concurrent
        );
    }

    #[test]
    fn inc_and_merge() {
        let mut a = VClock::new();
        inc(&mut a, "me");
        inc(&mut a, "me");
        assert_eq!(a.get("me"), Some(&2));
        let b = vv(&[("me", 1), ("you", 5)]);
        let m = merge(&a, &b);
        assert_eq!(m, vv(&[("me", 2), ("you", 5)]));
        assert_eq!(compare(&m, &a), Causality::After);
        assert_eq!(compare(&m, &b), Causality::After);
    }
}
