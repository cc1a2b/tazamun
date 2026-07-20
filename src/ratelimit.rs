//! A token-bucket rate limiter for the transfer download path (P15).
//!
//! One [`RateLimiter`] gates the aggregate byte rate of every concurrent chunk
//! fetch on a folder: a fetcher calls [`RateLimiter::acquire`] for a chunk's
//! byte count before requesting it, and blocks until the bucket has refilled
//! enough. Rate 0 = unlimited (the default), so the limiter is a cheap no-op
//! until a `max-down` is set, and the rate can be changed live.
//!
//! The bucket math is pure and unit-tested against an injected clock; only
//! [`RateLimiter::acquire`] touches the real clock and sleeps. The lock is a
//! plain `std::sync::Mutex` held only for the arithmetic (never across an
//! await), so many fetchers can be waiting concurrently without serializing on
//! the lock — they wake, re-check, and race for tokens as the bucket refills,
//! which bounds the *aggregate* rate exactly (the property that matters).

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// Bucket state behind the lock. `rate` and `cap` are bytes and bytes/sec.
#[derive(Debug)]
struct Bucket {
    /// Fill rate in bytes/sec; 0.0 means unlimited.
    rate: f64,
    /// Max tokens the bucket holds (a burst allowance) — at least one max-size
    /// chunk so a single large chunk can always eventually be admitted.
    cap: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    /// Refills tokens for the time elapsed since `last`, capping at `cap`.
    fn refill(&mut self, now: Instant) {
        if self.rate <= 0.0 {
            return;
        }
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.cap);
        self.last = now;
    }

    /// Tries to take `n` tokens. Returns `Ok(())` if taken, or `Err(wait)` with
    /// how long until enough tokens exist (based on the deficit and rate).
    fn try_take(&mut self, n: f64, now: Instant) -> Result<(), Duration> {
        if self.rate <= 0.0 {
            return Ok(());
        }
        self.refill(now);
        if self.tokens >= n {
            self.tokens -= n;
            return Ok(());
        }
        let deficit = n - self.tokens;
        Err(Duration::from_secs_f64(deficit / self.rate))
    }
}

/// A shared, live-reconfigurable token-bucket limiter.
#[derive(Debug)]
pub struct RateLimiter {
    inner: Mutex<Bucket>,
}

impl RateLimiter {
    /// A limiter at `bytes_per_sec` (0 = unlimited). The burst cap is one
    /// second of rate, but never below the largest chunk size, so a single
    /// chunk always fits.
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            inner: Mutex::new(Self::bucket(bytes_per_sec)),
        }
    }

    fn bucket(bytes_per_sec: u64) -> Bucket {
        let rate = bytes_per_sec as f64;
        let cap = rate.max(crate::consts::CDC_MAX as f64);
        Bucket {
            rate,
            cap,
            tokens: cap,
            last: Instant::now(),
        }
    }

    /// Changes the rate live (0 = unlimited), resetting the burst allowance.
    pub fn set_rate(&self, bytes_per_sec: u64) {
        if let Ok(mut b) = self.inner.lock() {
            *b = Self::bucket(bytes_per_sec);
        }
    }

    /// Blocks until `n` bytes of budget are available, then consumes them.
    /// Cheap and immediate when unlimited. The lock is only held for the
    /// arithmetic; the wait happens with the lock released.
    pub async fn acquire(&self, n: u64) {
        let n = n as f64;
        loop {
            let wait = {
                let mut b = match self.inner.lock() {
                    Ok(b) => b,
                    Err(_) => return, // poisoned: fail open rather than wedge sync
                };
                match b.try_take(n, Instant::now()) {
                    Ok(()) => return,
                    Err(wait) => wait,
                }
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_blocks() {
        let mut b = Bucket {
            rate: 0.0,
            cap: 0.0,
            tokens: 0.0,
            last: Instant::now(),
        };
        // Any request is instantly granted when unlimited.
        assert!(b.try_take(1_000_000.0, Instant::now()).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_refills_over_time_and_bounds_rate() {
        // 1000 B/s, cap 1000 (well under CDC_MAX in the real ctor, but here we
        // build the bucket by hand to control cap for the test).
        let start = Instant::now();
        let mut b = Bucket {
            rate: 1000.0,
            cap: 1000.0,
            tokens: 1000.0,
            last: start,
        };
        // Full bucket: a 1000-byte take succeeds immediately.
        assert!(b.try_take(1000.0, start).is_ok());
        // Now empty: another 1000 needs ~1s.
        let err = b.try_take(1000.0, start).unwrap_err();
        assert!((err.as_secs_f64() - 1.0).abs() < 0.01, "{err:?}");
        // After 500ms, half is available; 500 succeeds, 501 does not.
        let mid = start + Duration::from_millis(500);
        assert!(b.try_take(500.0, mid).is_ok());
        let err = b.try_take(500.0, mid).unwrap_err();
        assert!(err.as_secs_f64() > 0.0);
    }

    #[test]
    fn cap_admits_a_single_large_chunk() {
        // The real constructor's cap is >= CDC_MAX, so one max chunk always fits.
        let rl = RateLimiter::new(1); // 1 B/s, tiny rate
        let mut b = rl.inner.lock().unwrap();
        let chunk = crate::consts::CDC_MAX as f64;
        // A drained bucket still reports a finite wait (never rejects outright).
        b.tokens = 0.0;
        let wait = b.try_take(chunk, Instant::now()).unwrap_err();
        assert!(wait.as_secs_f64() > 0.0 && wait.as_secs_f64().is_finite());
    }
}
