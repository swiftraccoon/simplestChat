#![forbid(unsafe_code)]
//! Sign-in failure delays and a registration window, keyed by client address.
//!
//! Only failures from the same address cohort against the same account delay
//! password proof. A distributed failure stream must not lock out an owner at
//! an unrelated address. Bounded LRU tables evict cold records rather than
//! assigning strangers a shared overflow penalty. HTTP admission and password
//! work semaphores independently bound resource use under distributed traffic.

use lru::LruCache;
use std::hash::Hash;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FREE_PAIR_FAILURES: u32 = 3;
const MAX_PAIR_DELAY: Duration = Duration::from_secs(300);
const FAILURE_MEMORY: Duration = Duration::from_secs(3600);
const MAX_TRACKED_KEYS: usize = 10_000;

/// Keyed IPv6 clients by their /64, like every other address limit here.
pub(crate) fn cohort(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return IpAddr::V4(mapped);
            }
            let segments = address.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
    }
}

/// The wait after `failures`, once `free` of them have been forgiven: one
/// second, then doubling to `ceiling`.
fn delay_after(failures: u32, free: u32, ceiling: Duration) -> Duration {
    let excess = failures.saturating_sub(free);
    if excess == 0 {
        return Duration::ZERO;
    }
    let seconds = 1u64.checked_shl(excess - 1).unwrap_or(u64::MAX);
    Duration::from_secs(seconds).min(ceiling)
}

#[derive(Debug)]
struct Failures {
    count: u32,
    blocked_until: Instant,
    last_failure: Instant,
}

struct FailureTable<K> {
    entries: LruCache<K, Failures>,
    free: u32,
    ceiling: Duration,
}

impl<K: Eq + Hash> FailureTable<K> {
    fn new(free: u32, ceiling: Duration) -> Self {
        Self {
            entries: LruCache::new(NonZeroUsize::new(MAX_TRACKED_KEYS).unwrap()),
            free,
            ceiling,
        }
    }

    fn wait(&mut self, key: &K, now: Instant) -> Duration {
        self.entries
            .get(key)
            .filter(|entry| now.duration_since(entry.last_failure) < FAILURE_MEMORY)
            .map_or(Duration::ZERO, |entry| {
                entry.blocked_until.saturating_duration_since(now)
            })
    }

    fn record_failure(&mut self, key: K, now: Instant) {
        let entry = self.entries.get_or_insert_mut(key, || Failures {
            count: 0,
            blocked_until: now,
            last_failure: now,
        });
        if now.duration_since(entry.last_failure) >= FAILURE_MEMORY {
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
        entry.last_failure = now;
        entry.blocked_until = now + delay_after(entry.count, self.free, self.ceiling);
    }

    fn clear(&mut self, key: &K) {
        self.entries.pop(key);
    }
}

struct FailureState {
    pairs: FailureTable<(IpAddr, String)>,
}

/// Failure-charged delays for password sign-in and recovery redemption.
#[derive(Clone)]
pub struct FailureLimiter {
    state: Arc<Mutex<FailureState>>,
}

impl Default for FailureLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl FailureLimiter {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FailureState {
                pairs: FailureTable::new(FREE_PAIR_FAILURES, MAX_PAIR_DELAY),
            })),
        }
    }

    /// Whole seconds this client must still wait before trying this account,
    /// or `None` when it may try now. Never zero: a wait under a second rounds up.
    pub fn wait(&self, address: IpAddr, account: &str) -> Option<u64> {
        self.wait_at(address, account, Instant::now())
    }

    fn wait_at(&self, address: IpAddr, account: &str, now: Instant) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let pair = (cohort(address), account.to_owned());
        let wait = state.pairs.wait(&pair, now);
        if wait.is_zero() {
            return None;
        }
        let seconds = wait.as_secs();
        Some(if wait.subsec_nanos() > 0 {
            seconds + 1
        } else {
            seconds
        })
    }

    pub fn record_failure(&self, address: IpAddr, account: &str) {
        self.record_failure_at(address, account, Instant::now());
    }

    fn record_failure_at(&self, address: IpAddr, account: &str, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .pairs
            .record_failure((cohort(address), account.to_owned()), now);
    }

    /// A correct password proves the owner is present: forgive everything.
    pub fn record_success(&self, address: IpAddr, account: &str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.pairs.clear(&(cohort(address), account.to_owned()));
    }
}

#[derive(Debug)]
struct Window {
    started: Instant,
    used: u32,
}

/// Registrations and taken-email answers share one address-cohort window.
/// Cold entries may be evicted at the hard memory bound; unrelated addresses
/// never inherit another cohort's exhausted allowance.
pub struct WindowLimiter {
    entries: Mutex<LruCache<IpAddr, Window>>,
    window: Duration,
    allowance: u32,
}

impl WindowLimiter {
    pub fn new(allowance: u32, window: Duration) -> Self {
        Self {
            entries: Mutex::new(LruCache::new(NonZeroUsize::new(MAX_TRACKED_KEYS).unwrap())),
            window,
            allowance,
        }
    }

    pub fn allow(&self, address: IpAddr) -> bool {
        self.allow_at(address, Instant::now())
    }

    fn allow_at(&self, address: IpAddr, now: Instant) -> bool {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let window = entries.get_or_insert_mut(cohort(address), || Window {
            started: now,
            used: 0,
        });
        if now.duration_since(window.started) >= self.window {
            window.started = now;
            window.used = 0;
        }
        if window.used >= self.allowance {
            return false;
        }
        window.used += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7));
    const OTHER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 9));

    #[test]
    fn delays_start_after_free_failures_double_and_stop_at_the_ceiling() {
        let limiter = FailureLimiter::new();
        let start = Instant::now();
        for _ in 0..FREE_PAIR_FAILURES {
            limiter.record_failure_at(HOME, "a@example.test", start);
            assert_eq!(limiter.wait_at(HOME, "a@example.test", start), None);
        }
        let mut expected = 1;
        for _ in 0..12 {
            limiter.record_failure_at(HOME, "a@example.test", start);
            assert_eq!(
                limiter.wait_at(HOME, "a@example.test", start),
                Some(expected.min(MAX_PAIR_DELAY.as_secs()))
            );
            expected *= 2;
        }
        // A wait counts down and rounds up to whole seconds.
        assert_eq!(
            limiter.wait_at(HOME, "a@example.test", start + Duration::from_millis(500)),
            Some(MAX_PAIR_DELAY.as_secs())
        );
        assert_eq!(
            limiter.wait_at(HOME, "a@example.test", start + MAX_PAIR_DELAY),
            None
        );
        // Other accounts and address cohorts remain able to prove credentials.
        assert_eq!(limiter.wait_at(HOME, "b@example.test", start), None);
        assert_eq!(limiter.wait_at(OTHER, "a@example.test", start), None);
    }

    #[test]
    fn a_success_forgives_every_earlier_failure() {
        let limiter = FailureLimiter::new();
        let start = Instant::now();
        for _ in 0..8 {
            limiter.record_failure_at(HOME, "a@example.test", start);
        }
        assert!(limiter.wait_at(HOME, "a@example.test", start).is_some());
        limiter.record_success(HOME, "a@example.test");
        assert_eq!(limiter.wait_at(HOME, "a@example.test", start), None);
        limiter.record_failure_at(HOME, "a@example.test", start);
        assert_eq!(limiter.wait_at(HOME, "a@example.test", start), None);
    }

    #[test]
    fn failures_from_other_addresses_cannot_hold_an_owner_out() {
        let limiter = FailureLimiter::new();
        let start = Instant::now();
        for round in 0..100 {
            let now = start + Duration::from_secs(round);
            for host in 1..=40u8 {
                let address = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, host));
                limiter.record_failure_at(address, "victim@example.test", now);
            }
            assert_eq!(limiter.wait_at(HOME, "victim@example.test", now), None);
        }
    }

    #[test]
    fn ipv6_addresses_share_a_cohort_and_failures_expire_from_memory() {
        let limiter = FailureLimiter::new();
        let start = Instant::now();
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let second: IpAddr = "2001:db8:1:2:ffff::2".parse().unwrap();
        for _ in 0..5 {
            limiter.record_failure_at(first, "a@example.test", start);
        }
        assert_eq!(limiter.wait_at(second, "a@example.test", start), Some(2));
        let later = start + FAILURE_MEMORY;
        assert_eq!(limiter.wait_at(second, "a@example.test", later), None);
        limiter.record_failure_at(first, "a@example.test", later);
        assert_eq!(limiter.wait_at(first, "a@example.test", later), None);
    }

    #[test]
    fn a_full_table_evicts_cold_keys_without_penalizing_unrelated_newcomers() {
        let mut table: FailureTable<u32> = FailureTable::new(1, Duration::from_secs(8));
        let start = Instant::now();
        for key in 0..MAX_TRACKED_KEYS as u32 {
            table.record_failure(key, start);
        }
        table.record_failure(0, start);
        table.record_failure(0, start);
        table.record_failure(u32::MAX, start);
        table.record_failure(u32::MAX, start);
        assert_eq!(table.entries.len(), MAX_TRACKED_KEYS);
        assert!(
            table.entries.peek(&1).is_none(),
            "least recently used key evicted"
        );
        assert_eq!(table.wait(&0, start), Duration::from_secs(2));
        assert_eq!(table.wait(&(u32::MAX - 1), start), Duration::ZERO);
        assert_eq!(table.wait(&u32::MAX, start), Duration::from_secs(1));
        table.record_failure(u32::MAX, start + FAILURE_MEMORY);
        assert_eq!(table.entries.peek(&u32::MAX).unwrap().count, 1);
    }

    #[test]
    fn full_registration_memory_does_not_share_a_denial_with_new_addresses() {
        let limiter = WindowLimiter::new(1, Duration::from_secs(3600));
        let now = Instant::now();
        for key in 0..MAX_TRACKED_KEYS as u32 {
            assert!(limiter.allow_at(IpAddr::V4(key.into()), now));
        }
        assert!(limiter.allow_at(HOME, now));
        assert!(!limiter.allow_at(HOME, now));
        assert!(limiter.allow_at(OTHER, now));
        assert_eq!(limiter.entries.lock().unwrap().len(), MAX_TRACKED_KEYS);
    }

    #[test]
    fn a_registration_window_admits_its_allowance_then_reopens() {
        let limiter = WindowLimiter::new(2, Duration::from_secs(3600));
        let start = Instant::now();
        assert!(limiter.allow_at(HOME, start));
        assert!(limiter.allow_at(HOME, start + Duration::from_secs(1)));
        assert!(!limiter.allow_at(HOME, start + Duration::from_secs(2)));
        assert!(limiter.allow_at(OTHER, start + Duration::from_secs(2)));
        // The window is dropped when it ends, and the next use opens a new one.
        assert!(limiter.allow_at(HOME, start + Duration::from_secs(3600)));
        assert!(limiter.allow_at(HOME, start + Duration::from_secs(3601)));
        assert!(!limiter.allow_at(HOME, start + Duration::from_secs(3602)));
    }
}
