//! Pure lease state machine for strict exclusive checkout.
//!
//! Invariant: this module performs zero I/O and never reads a real clock —
//! `now` is injected into every transition, so the machine is fully
//! deterministic and unit-testable. All nodes applying the same events reach
//! the same winner because ties resolve on the total order `(lamport, id)`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::consts::{
    ACQUIRE_TIMEOUT, LEASE_RENEW, LEASE_TTL, MAX_LEASE_TTL, MAX_TRACKED_LEASES, MIN_LEASE_TTL,
};
use crate::proto::DenyReason;
use crate::state::RelPath;

/// Canonical endpoint id string (lowercase hex).
pub type Id = String;

/// Lease timing knobs; constructor parameters so tests can shorten them.
#[derive(Debug, Clone, Copy)]
pub struct LockTimings {
    pub ttl: Duration,
    pub renew: Duration,
    pub acquire_timeout: Duration,
}

impl Default for LockTimings {
    fn default() -> Self {
        Self {
            ttl: LEASE_TTL,
            renew: LEASE_RENEW,
            acquire_timeout: ACQUIRE_TIMEOUT,
        }
    }
}

/// State of one path. Absence from the table means `Free`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    PendingLocal {
        lamport: u64,
        needed: BTreeSet<Id>,
        granted: BTreeSet<Id>,
        deadline: Instant,
    },
    Held {
        holder: Id,
        lamport: u64,
        expires: Instant,
        /// When this node first recorded the current holder's lease. Preserved
        /// across renewals by the same holder; reset when the holder changes.
        /// Used only for the `age` shown by `tazamun locks`.
        since: Instant,
    },
}

/// Decision for an incoming remote lock request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Grant,
    /// Grant them and abort our own pending request (we lost the tie).
    GrantAndAbortMine,
    Deny(DenyReason),
}

/// Returned by [`LockTable::on_grant`] when the final grant arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acquired;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    #[error("lease already held by {0}")]
    AlreadyHeld(Id),
    #[error("a lock request for this path is already pending")]
    AlreadyPending,
}

/// Outcome of a [`LockTable::sweep`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    /// Held leases that expired: `(path, holder)`.
    pub expired: Vec<(RelPath, Id)>,
    /// Local pending requests whose deadline passed.
    pub timed_out: Vec<RelPath>,
}

/// The lease table for every path in the session.
#[derive(Debug)]
pub struct LockTable {
    me: Id,
    timings: LockTimings,
    states: BTreeMap<RelPath, LockState>,
    /// DoS bound: the most distinct paths the table will ever track. A new-path
    /// insertion at capacity is refused (an existing path still updates), so a
    /// hostile `Index` lease flood or `LockReq` storm cannot grow it without
    /// limit. See [`MAX_TRACKED_LEASES`].
    max_leases: usize,
}

impl LockTable {
    pub fn new(me: Id, timings: LockTimings) -> Self {
        Self {
            me,
            timings,
            states: BTreeMap::new(),
            max_leases: MAX_TRACKED_LEASES,
        }
    }

    /// True when the table already tracks `path`; used to decide whether a new
    /// insertion would breach the [`MAX_TRACKED_LEASES`] cap.
    fn at_capacity_for_new(&self, path: &RelPath) -> bool {
        !self.states.contains_key(path) && self.states.len() >= self.max_leases
    }

    /// Test-only: shrink the tracked-lease cap so the bound is exercised
    /// without inserting thousands of entries.
    #[cfg(test)]
    fn with_max_leases(mut self, n: usize) -> Self {
        self.max_leases = n;
        self
    }

    pub fn me(&self) -> &Id {
        &self.me
    }

    pub fn timings(&self) -> LockTimings {
        self.timings
    }

    fn ttl_from_ms(&self, ttl_ms: u64) -> Duration {
        // CONSISTENCY RULE: TTL is lease-scoped — the *holder's* configured TTL
        // governs each lease and rides the wire, so nodes may run different
        // local configs without protocol divergence. A receiver honors the wire
        // value, clamped defensively to an absolute [MIN, MAX] range (not
        // relative to local config) so a hostile `ttl_ms = 0` cannot make a
        // lease instantaneous and a huge value cannot park it forever.
        Duration::from_millis(ttl_ms).clamp(MIN_LEASE_TTL, MAX_LEASE_TTL)
    }

    /// Drops an expired `Held` entry before inspecting `path`.
    fn prune_expired(&mut self, path: &RelPath, now: Instant) {
        if let Some(LockState::Held { expires, .. }) = self.states.get(path)
            && *expires <= now
        {
            self.states.remove(path);
        }
    }

    /// Begins a local acquire: records the pending request awaiting one grant
    /// from every voter. An empty voter set is the caller's error (the daemon
    /// enforces REACHABILITY before calling this).
    pub fn start_request(
        &mut self,
        path: &RelPath,
        lamport: u64,
        voters: BTreeSet<Id>,
        now: Instant,
    ) -> Result<(), StartError> {
        self.prune_expired(path, now);
        match self.states.get(path) {
            Some(LockState::Held { holder, .. }) => {
                return Err(StartError::AlreadyHeld(holder.clone()));
            }
            Some(LockState::PendingLocal { .. }) => return Err(StartError::AlreadyPending),
            None => {}
        }
        self.states.insert(
            path.clone(),
            LockState::PendingLocal {
                lamport,
                needed: voters,
                granted: BTreeSet::new(),
                deadline: now + self.timings.acquire_timeout,
            },
        );
        Ok(())
    }

    /// Records a grant from `from`; returns `Some(Acquired)` when every needed
    /// voter has granted, transitioning the path to `Held` by us.
    pub fn on_grant(&mut self, path: &RelPath, from: &Id, now: Instant) -> Option<Acquired> {
        let LockState::PendingLocal {
            lamport,
            needed,
            granted,
            ..
        } = self.states.get_mut(path)?
        else {
            return None;
        };
        if !needed.contains(from) {
            return None;
        }
        granted.insert(from.clone());
        if granted.is_superset(needed) {
            let lamport = *lamport;
            self.states.insert(
                path.clone(),
                LockState::Held {
                    holder: self.me.clone(),
                    lamport,
                    expires: now + self.timings.ttl,
                    since: now,
                },
            );
            Some(Acquired)
        } else {
            None
        }
    }

    /// Aborts our pending request after a deny. Returns true if one existed.
    pub fn on_deny(&mut self, path: &RelPath) -> bool {
        if matches!(self.states.get(path), Some(LockState::PendingLocal { .. })) {
            self.states.remove(path);
            true
        } else {
            false
        }
    }

    /// Handles a remote `LockReq`.
    pub fn on_remote_request(
        &mut self,
        path: &RelPath,
        holder: &Id,
        lamport: u64,
        ttl_ms: u64,
        now: Instant,
    ) -> Decision {
        self.prune_expired(path, now);
        let ttl = self.ttl_from_ms(ttl_ms);
        if self.at_capacity_for_new(path) {
            // DoS bound: at the tracked-lease cap, decline a brand-new path
            // rather than grow the table. Existing paths are unaffected.
            return Decision::Deny(DenyReason::Unavailable);
        }
        match self.states.get(path) {
            Some(LockState::Held {
                holder: h,
                lamport: held_lamport,
                since,
                ..
            }) => {
                if h == holder {
                    // Idempotent re-request refreshes the lease (age preserved).
                    let held_lamport = *held_lamport;
                    let since = *since;
                    self.states.insert(
                        path.clone(),
                        LockState::Held {
                            holder: holder.clone(),
                            lamport: held_lamport.max(lamport),
                            expires: now + ttl,
                            since,
                        },
                    );
                    Decision::Grant
                } else {
                    Decision::Deny(DenyReason::Held { by: h.clone() })
                }
            }
            Some(LockState::PendingLocal {
                lamport: my_lamport,
                ..
            }) => {
                // Deterministic tie-break: lowest (lamport, id) wins on every
                // node, so exactly one side aborts.
                if (lamport, holder.as_str()) < (*my_lamport, self.me.as_str()) {
                    self.states.insert(
                        path.clone(),
                        LockState::Held {
                            holder: holder.clone(),
                            lamport,
                            expires: now + ttl,
                            since: now,
                        },
                    );
                    Decision::GrantAndAbortMine
                } else {
                    Decision::Deny(DenyReason::TieLost)
                }
            }
            None => {
                self.states.insert(
                    path.clone(),
                    LockState::Held {
                        holder: holder.clone(),
                        lamport,
                        expires: now + ttl,
                        since: now,
                    },
                );
                Decision::Grant
            }
        }
    }

    /// Handles a remote `LockRelease`.
    pub fn on_release(&mut self, path: &RelPath, from: &Id) {
        if let Some(LockState::Held { holder, .. }) = self.states.get(path)
            && holder == from
        {
            self.states.remove(path);
        }
    }

    /// Handles a remote `LockRenew`.
    pub fn on_renew(&mut self, path: &RelPath, from: &Id, ttl_ms: u64, now: Instant) {
        let ttl = self.ttl_from_ms(ttl_ms);
        if let Some(LockState::Held {
            holder, expires, ..
        }) = self.states.get_mut(path)
            && holder == from
        {
            *expires = now + ttl;
        }
    }

    /// Refreshes our own lease locally (called alongside broadcasting renew).
    pub fn renew_own(&mut self, path: &RelPath, now: Instant) {
        let me = self.me.clone();
        let ttl = self.timings.ttl;
        if let Some(LockState::Held {
            holder, expires, ..
        }) = self.states.get_mut(path)
            && *holder == me
        {
            *expires = now + ttl;
        }
    }

    /// Ingests a lease advertised in an `Index` message. On conflicting held
    /// claims the lower `(lamport, id)` wins.
    pub fn observe_lease(
        &mut self,
        path: &RelPath,
        holder: &Id,
        lamport: u64,
        expires_in_ms: u64,
        now: Instant,
    ) {
        self.prune_expired(path, now);
        if self.at_capacity_for_new(path) {
            // DoS bound: do not begin tracking a new observed lease past the
            // cap (an `Index` can advertise a flood of hostile `LeaseInfo`).
            return;
        }
        let expires = now + self.ttl_from_ms(expires_in_ms);
        match self.states.get(path) {
            None => {
                self.states.insert(
                    path.clone(),
                    LockState::Held {
                        holder: holder.clone(),
                        lamport,
                        expires,
                        since: now,
                    },
                );
            }
            Some(LockState::Held {
                holder: h,
                lamport: l,
                since,
                ..
            }) => {
                let same_holder = h == holder;
                if same_holder || (lamport, holder.as_str()) < (*l, h.as_str()) {
                    // Preserve the observed age on a same-holder refresh; reset
                    // it when a different holder's lower claim takes over.
                    let since = if same_holder { *since } else { now };
                    self.states.insert(
                        path.clone(),
                        LockState::Held {
                            holder: holder.clone(),
                            lamport,
                            expires,
                            since,
                        },
                    );
                }
            }
            // A pending local request is resolved by grants/denies, not by
            // gossip observation.
            Some(LockState::PendingLocal { .. }) => {}
        }
    }

    /// A voter vanished: every pending request that still needed it aborts
    /// (strict mode). Leases *held* by the vanished peer survive until TTL
    /// expiry as reconnect grace. Returns the aborted paths.
    pub fn on_peer_down(&mut self, id: &Id) -> Vec<RelPath> {
        let aborted: Vec<RelPath> = self
            .states
            .iter()
            .filter_map(|(p, s)| match s {
                LockState::PendingLocal {
                    needed, granted, ..
                } if needed.contains(id) && !granted.contains(id) => Some(p.clone()),
                _ => None,
            })
            .collect();
        for p in &aborted {
            self.states.remove(p);
        }
        aborted
    }

    /// Expires stale leases and timed-out pending requests.
    pub fn sweep(&mut self, now: Instant) -> Swept {
        let mut out = Swept::default();
        let paths: Vec<RelPath> = self.states.keys().cloned().collect();
        for p in paths {
            match self.states.get(&p) {
                Some(LockState::Held {
                    holder, expires, ..
                }) if *expires <= now => {
                    out.expired.push((p.clone(), holder.clone()));
                    self.states.remove(&p);
                }
                Some(LockState::PendingLocal { deadline, .. }) if *deadline <= now => {
                    out.timed_out.push(p.clone());
                    self.states.remove(&p);
                }
                _ => {}
            }
        }
        out
    }

    pub fn is_held_by_me(&self, path: &RelPath) -> bool {
        matches!(
            self.states.get(path),
            Some(LockState::Held { holder, .. }) if *holder == self.me
        )
    }

    pub fn holder(&self, path: &RelPath) -> Option<&Id> {
        match self.states.get(path) {
            Some(LockState::Held { holder, .. }) => Some(holder),
            _ => None,
        }
    }

    pub fn self_held_paths(&self) -> Vec<RelPath> {
        self.states
            .iter()
            .filter_map(|(p, s)| match s {
                LockState::Held { holder, .. } if *holder == self.me => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    /// All currently held leases as `(path, holder, lamport, expires_in, age)`,
    /// where `age` is how long this node has observed the current holder's
    /// lease.
    pub fn held_leases(&self, now: Instant) -> Vec<(RelPath, Id, u64, Duration, Duration)> {
        self.states
            .iter()
            .filter_map(|(p, s)| match s {
                LockState::Held {
                    holder,
                    lamport,
                    expires,
                    since,
                } => Some((
                    p.clone(),
                    holder.clone(),
                    *lamport,
                    expires.saturating_duration_since(now),
                    now.saturating_duration_since(*since),
                )),
                _ => None,
            })
            .collect()
    }

    pub fn pending_lamport(&self, path: &RelPath) -> Option<u64> {
        match self.states.get(path) {
            Some(LockState::PendingLocal { lamport, .. }) => Some(*lamport),
            _ => None,
        }
    }

    /// For a pending local request, the `(voters_needed, voters_granted)` sets
    /// so the daemon can name which peers have not yet answered.
    pub fn pending_votes(&self, path: &RelPath) -> Option<(BTreeSet<Id>, BTreeSet<Id>)> {
        match self.states.get(path) {
            Some(LockState::PendingLocal {
                needed, granted, ..
            }) => Some((needed.clone(), granted.clone())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::index::sanitize_rel_path;

    const A: &str = "aaaa";
    const B: &str = "bbbb";
    const C: &str = "cccc";

    fn table(me: &str) -> LockTable {
        LockTable::new(
            me.to_string(),
            LockTimings {
                ttl: Duration::from_secs(90),
                renew: Duration::from_secs(30),
                acquire_timeout: Duration::from_secs(8),
            },
        )
    }

    fn p(s: &str) -> RelPath {
        sanitize_rel_path(s).unwrap()
    }

    fn voters(ids: &[&str]) -> BTreeSet<Id> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn grant_when_free() {
        let mut t = table(A);
        let now = Instant::now();
        let d = t.on_remote_request(&p("f"), &B.to_string(), 1, 90_000, now);
        assert_eq!(d, Decision::Grant);
        assert_eq!(t.holder(&p("f")), Some(&B.to_string()));
    }

    #[test]
    fn deny_while_held() {
        let mut t = table(A);
        let now = Instant::now();
        assert_eq!(
            t.on_remote_request(&p("f"), &B.to_string(), 1, 90_000, now),
            Decision::Grant
        );
        assert_eq!(
            t.on_remote_request(&p("f"), &C.to_string(), 2, 90_000, now),
            Decision::Deny(DenyReason::Held { by: B.to_string() })
        );
    }

    #[test]
    fn full_local_acquire_needs_every_voter() {
        let mut t = table(A);
        let now = Instant::now();
        t.start_request(&p("f"), 5, voters(&[B, C]), now).unwrap();
        assert_eq!(t.on_grant(&p("f"), &B.to_string(), now), None);
        // A grant from a non-voter never completes the acquire.
        assert_eq!(t.on_grant(&p("f"), &"zzzz".to_string(), now), None);
        assert_eq!(t.on_grant(&p("f"), &C.to_string(), now), Some(Acquired));
        assert!(t.is_held_by_me(&p("f")));
    }

    #[test]
    fn tie_break_lower_lamport_wins() {
        // Us pending with lamport 5; remote requests with lamport 3 → they win.
        let mut t = table(B);
        let now = Instant::now();
        t.start_request(&p("f"), 5, voters(&[A]), now).unwrap();
        let d = t.on_remote_request(&p("f"), &A.to_string(), 3, 90_000, now);
        assert_eq!(d, Decision::GrantAndAbortMine);
        assert_eq!(t.holder(&p("f")), Some(&A.to_string()));

        // And with a higher remote lamport → we win, they lose the tie.
        let mut t = table(B);
        t.start_request(&p("g"), 5, voters(&[A]), now).unwrap();
        let d = t.on_remote_request(&p("g"), &A.to_string(), 7, 90_000, now);
        assert_eq!(d, Decision::Deny(DenyReason::TieLost));
        assert_eq!(t.pending_lamport(&p("g")), Some(5));
    }

    #[test]
    fn tie_break_equal_lamport_lower_id_wins() {
        // Equal lamport: id "aaaa" < "bbbb", so A wins on B's node…
        let mut t = table(B);
        let now = Instant::now();
        t.start_request(&p("f"), 5, voters(&[A]), now).unwrap();
        assert_eq!(
            t.on_remote_request(&p("f"), &A.to_string(), 5, 90_000, now),
            Decision::GrantAndAbortMine
        );
        // …and B loses on A's node, so both agree on the winner.
        let mut t = table(A);
        t.start_request(&p("f"), 5, voters(&[B]), now).unwrap();
        assert_eq!(
            t.on_remote_request(&p("f"), &B.to_string(), 5, 90_000, now),
            Decision::Deny(DenyReason::TieLost)
        );
    }

    #[test]
    fn voter_disconnect_aborts_pending() {
        let mut t = table(A);
        let now = Instant::now();
        t.start_request(&p("f"), 1, voters(&[B, C]), now).unwrap();
        assert_eq!(t.on_grant(&p("f"), &B.to_string(), now), None);
        let aborted = t.on_peer_down(&C.to_string());
        assert_eq!(aborted, vec![p("f")]);
        assert_eq!(t.pending_lamport(&p("f")), None);
        // A voter that already granted does not abort the request.
        t.start_request(&p("g"), 2, voters(&[B, C]), now).unwrap();
        assert_eq!(t.on_grant(&p("g"), &B.to_string(), now), None);
        assert!(t.on_peer_down(&B.to_string()).is_empty());
    }

    #[test]
    fn ttl_expiry_frees_lease() {
        // TTL >= MIN_LEASE_TTL so it is honored verbatim (not clamped up).
        let mut t = table(A);
        let now = Instant::now();
        t.on_remote_request(&p("f"), &B.to_string(), 1, 12_000, now);
        let swept = t.sweep(now + Duration::from_millis(11_999));
        assert!(swept.expired.is_empty());
        let swept = t.sweep(now + Duration::from_millis(12_001));
        assert_eq!(swept.expired, vec![(p("f"), B.to_string())]);
        // Freed: a new request is granted.
        assert_eq!(
            t.on_remote_request(
                &p("f"),
                &C.to_string(),
                2,
                12_000,
                now + Duration::from_secs(13)
            ),
            Decision::Grant
        );
    }

    #[test]
    fn idempotent_rerequest_renews() {
        let mut t = table(A);
        let now = Instant::now();
        t.on_remote_request(&p("f"), &B.to_string(), 1, 12_000, now);
        // Same holder re-requests later: still Grant, expiry pushed out.
        let later = now + Duration::from_millis(1_500);
        assert_eq!(
            t.on_remote_request(&p("f"), &B.to_string(), 4, 12_000, later),
            Decision::Grant
        );
        let swept = t.sweep(now + Duration::from_millis(12_500));
        assert!(swept.expired.is_empty(), "renewed lease must not expire");
        let swept = t.sweep(later + Duration::from_millis(12_001));
        assert_eq!(swept.expired.len(), 1);
    }

    #[test]
    fn renew_extends_and_release_frees() {
        let mut t = table(A);
        let now = Instant::now();
        t.on_remote_request(&p("f"), &B.to_string(), 1, 2_000, now);
        t.on_renew(&p("f"), &B.to_string(), 2_000, now + Duration::from_secs(1));
        assert!(
            t.sweep(now + Duration::from_millis(2_500))
                .expired
                .is_empty()
        );
        // Release by a non-holder is ignored; by the holder it frees.
        t.on_release(&p("f"), &C.to_string());
        assert_eq!(t.holder(&p("f")), Some(&B.to_string()));
        t.on_release(&p("f"), &B.to_string());
        assert_eq!(t.holder(&p("f")), None);
    }

    #[test]
    fn observe_lease_conflict_keeps_lower() {
        let mut t = table(A);
        let now = Instant::now();
        t.observe_lease(&p("f"), &C.to_string(), 5, 90_000, now);
        // Lower (lamport, id) claim replaces the current one…
        t.observe_lease(&p("f"), &B.to_string(), 3, 90_000, now);
        assert_eq!(t.holder(&p("f")), Some(&B.to_string()));
        // …and a higher claim is ignored.
        t.observe_lease(&p("f"), &C.to_string(), 9, 90_000, now);
        assert_eq!(t.holder(&p("f")), Some(&B.to_string()));
    }

    #[test]
    fn hostile_wire_ttl_is_clamped_to_absolute_band() {
        // A malicious peer sending ttl_ms = 0 must not make the lease
        // instantaneous: it clamps up to MIN_LEASE_TTL.
        let mut t = table(A);
        let now = Instant::now();
        t.on_remote_request(&p("f"), &B.to_string(), 1, 0, now);
        assert!(
            t.sweep(now + MIN_LEASE_TTL - Duration::from_millis(1))
                .expired
                .is_empty(),
            "a zero wire TTL must still last at least MIN_LEASE_TTL"
        );
        assert_eq!(
            t.sweep(now + MIN_LEASE_TTL + Duration::from_millis(1))
                .expired
                .len(),
            1
        );

        // An enormous wire TTL cannot park the lease past MAX_LEASE_TTL.
        let mut t = table(A);
        t.on_remote_request(&p("g"), &B.to_string(), 1, u64::MAX, now);
        assert!(
            t.sweep(now + MAX_LEASE_TTL - Duration::from_secs(1))
                .expired
                .is_empty()
        );
        assert_eq!(
            t.sweep(now + MAX_LEASE_TTL + Duration::from_secs(1))
                .expired
                .len(),
            1
        );
    }

    #[test]
    fn lease_age_tracks_holder_and_survives_renew() {
        let mut t = table(A);
        let now = Instant::now();
        t.on_remote_request(&p("f"), &B.to_string(), 1, 90_000, now);
        // Age grows with the clock while the same holder renews.
        let later = now + Duration::from_secs(5);
        t.on_renew(&p("f"), &B.to_string(), 90_000, later);
        let leases = t.held_leases(now + Duration::from_secs(10));
        let (_, holder, _, _, age) = &leases[0];
        assert_eq!(holder, B);
        assert_eq!(*age, Duration::from_secs(10));
        // A lower-(lamport,id) different holder taking over resets the age.
        t.observe_lease(
            &p("f"),
            &C.to_string(),
            0,
            90_000,
            now + Duration::from_secs(10),
        );
        let leases = t.held_leases(now + Duration::from_secs(12));
        let (_, holder, _, _, age) = &leases[0];
        assert_eq!(holder, C);
        assert_eq!(*age, Duration::from_secs(2));
    }

    #[test]
    fn pending_deadline_times_out_via_sweep() {
        let mut t = table(A);
        let now = Instant::now();
        t.start_request(&p("f"), 1, voters(&[B]), now).unwrap();
        let swept = t.sweep(now + Duration::from_secs(9));
        assert_eq!(swept.timed_out, vec![p("f")]);
    }

    #[test]
    fn tracked_lease_cap_refuses_new_paths_but_updates_existing() {
        // Cap of 2 tracked paths. A third distinct remote request is denied
        // with `Unavailable` rather than growing the table.
        let mut t = table(A).with_max_leases(2);
        let now = Instant::now();
        assert_eq!(
            t.on_remote_request(&p("a"), &B.to_string(), 1, 90_000, now),
            Decision::Grant
        );
        assert_eq!(
            t.on_remote_request(&p("b"), &B.to_string(), 1, 90_000, now),
            Decision::Grant
        );
        assert_eq!(
            t.on_remote_request(&p("c"), &B.to_string(), 1, 90_000, now),
            Decision::Deny(DenyReason::Unavailable)
        );
        // An already-tracked path still refreshes at capacity (same holder).
        assert_eq!(
            t.on_remote_request(&p("a"), &B.to_string(), 2, 90_000, now),
            Decision::Grant
        );
        // A freed slot admits a new path again.
        t.on_release(&p("a"), &B.to_string());
        assert_eq!(
            t.on_remote_request(&p("c"), &B.to_string(), 1, 90_000, now),
            Decision::Grant
        );
    }

    #[test]
    fn observed_lease_flood_is_capped() {
        // A hostile Index advertising many leases cannot grow the table past
        // the cap; existing entries still update.
        let mut t = table(A).with_max_leases(2);
        let now = Instant::now();
        t.observe_lease(&p("a"), &B.to_string(), 1, 90_000, now);
        t.observe_lease(&p("b"), &B.to_string(), 1, 90_000, now);
        t.observe_lease(&p("c"), &B.to_string(), 1, 90_000, now); // dropped
        assert_eq!(t.holder(&p("a")), Some(&B.to_string()));
        assert_eq!(t.holder(&p("b")), Some(&B.to_string()));
        assert_eq!(t.holder(&p("c")), None);
        // Existing path still takes a lower-(lamport,id) takeover at capacity.
        t.observe_lease(&p("a"), &"aaaa".to_string(), 0, 90_000, now);
        assert_eq!(t.holder(&p("a")), Some(&"aaaa".to_string()));
    }
}
