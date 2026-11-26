//! Per-IP throttle for repeated failed logins.
//!
//! Credential verification uses Argon2id (deliberately slow), which makes
//! online password guessing expensive per attempt, but nothing stopped an
//! attacker from simply making a great many attempts. This caps the number of
//! *failed* `POST /authorize` attempts from one source IP within a sliding
//! window; past the cap, further attempts are rejected (`429`) WITHOUT running
//! the verify, so the cost of a guess campaign no longer scales with the
//! server's willingness to keep checking.
//!
//! Keyed by the peer IP, so one source's attempts never lock out another user
//! (a per-username lockout would let an attacker deny a victim service by
//! spamming their name). A successful login clears the IP's record, so a user
//! who simply mistyped a few times is not penalized on the next correct try.
//!
//! Note: behind a reverse proxy every request carries the proxy's IP, so all
//! clients would share one bucket. A deployment that fronts the server with a
//! proxy should rate-limit at the proxy; parsing `X-Forwarded-For` here would
//! be trivially spoofable without a trusted-proxy allowlist, so we don't.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

/// Failed attempts from one IP allowed within [`WINDOW_SECS`] before further
/// attempts are throttled.
const MAX_FAILURES: usize = 10;

/// Sliding window (seconds) over which failures are counted. Five minutes: a
/// human who fat-fingers a password a few times recovers quickly, while a
/// guessing campaign is held to [`MAX_FAILURES`] tries per window.
const WINDOW_SECS: u64 = 300;

/// Tracks recent failed-login timestamps per source IP.
#[derive(Default)]
pub struct LoginThrottle {
    failures: Mutex<HashMap<IpAddr, Vec<u64>>>,
}

impl LoginThrottle {
    /// An empty throttle with no recorded failures.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Vec<u64>>> {
        // A poisoned lock (a prior panic while held) still carries valid state
        // for a best-effort throttle; recover rather than propagate.
        self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether an attempt from `ip` is currently allowed: fewer than
    /// [`MAX_FAILURES`] failures recorded in the trailing [`WINDOW_SECS`].
    /// Expired timestamps are pruned as a side effect (bounding memory).
    pub fn allowed(&self, ip: IpAddr, now: u64) -> bool {
        let mut map = self.lock();
        match map.get_mut(&ip) {
            Some(times) => {
                times.retain(|&t| now.saturating_sub(t) < WINDOW_SECS);
                if times.is_empty() {
                    map.remove(&ip);
                    true
                } else {
                    times.len() < MAX_FAILURES
                }
            }
            None => true,
        }
    }

    /// Record a failed attempt from `ip` (pruning expired entries first).
    pub fn record_failure(&self, ip: IpAddr, now: u64) {
        let mut map = self.lock();
        let times = map.entry(ip).or_default();
        times.retain(|&t| now.saturating_sub(t) < WINDOW_SECS);
        times.push(now);
    }

    /// Forget an IP's failures (called on a successful login).
    pub fn clear(&self, ip: IpAddr) {
        self.lock().remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn allows_up_to_the_limit_then_throttles() {
        let t = LoginThrottle::new();
        let now = 1_000;
        let addr = ip(1);

        // The first MAX_FAILURES attempts are each allowed before they fail.
        for _ in 0..MAX_FAILURES {
            assert!(t.allowed(addr, now), "attempts under the cap are allowed");
            t.record_failure(addr, now);
        }
        // Having recorded MAX_FAILURES failures, the next attempt is throttled.
        assert!(
            !t.allowed(addr, now),
            "an IP over the failure cap is throttled"
        );
    }

    #[test]
    fn failures_age_out_of_the_window() {
        let t = LoginThrottle::new();
        let addr = ip(2);
        let start = 1_000;
        for _ in 0..MAX_FAILURES {
            t.record_failure(addr, start);
        }
        assert!(!t.allowed(addr, start), "throttled right after the burst");
        // Once the whole window has elapsed, the old failures are pruned.
        assert!(
            t.allowed(addr, start + WINDOW_SECS + 1),
            "the IP is allowed again after its failures age out"
        );
    }

    #[test]
    fn different_ips_are_independent() {
        let t = LoginThrottle::new();
        let now = 1_000;
        let attacker = ip(3);
        for _ in 0..MAX_FAILURES {
            t.record_failure(attacker, now);
        }
        assert!(!t.allowed(attacker, now), "the attacker IP is throttled");
        assert!(
            t.allowed(ip(4), now),
            "a different IP is unaffected (no cross-IP lockout)"
        );
    }

    #[test]
    fn a_success_clears_the_record() {
        let t = LoginThrottle::new();
        let now = 1_000;
        let addr = ip(5);
        for _ in 0..MAX_FAILURES {
            t.record_failure(addr, now);
        }
        assert!(!t.allowed(addr, now));
        t.clear(addr);
        assert!(
            t.allowed(addr, now),
            "a successful login clears the IP's failure history"
        );
    }
}
