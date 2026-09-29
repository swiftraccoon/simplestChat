#![forbid(unsafe_code)]
//! Sign-in failure delays and a registration window, keyed by client address.
//!
//! Every sign-in used to spend one of an account's twenty requests per minute,
//! so anyone who knew an email address could hold its owner out of password
//! sign-in for as long as they cared to keep sending. Here only failures count.
//! Each address cohort serves a delay that doubles with every further failure
//! against one account, and the account itself has a second, low-ceilinged
//! schedule so a spread of addresses is slowed without keeping the owner out
//! for more than half a minute. A success clears both.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Failures one address may make against one account before it waits.
const FREE_PAIR_FAILURES: u32 = 3;
/// The longest wait one address serves for one account.
const MAX_PAIR_DELAY: Duration = Duration::from_secs(300);
/// Failures against an account from anywhere before every address waits.
const FREE_ACCOUNT_FAILURES: u32 = 10;
/// The longest wait strangers can impose on an account's owner.
const MAX_ACCOUNT_DELAY: Duration = Duration::from_secs(30);
/// A key's failures are forgotten this long after its last one.
const FAILURE_MEMORY: Duration = Duration::from_secs(3600);
/// Live keys per table; beyond this, unknown keys share one allowance.
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
    entries: HashMap<K, Failures>,
    /// A shared entry for new keys while the table is full: an identity churn
    /// can never evict a live key's record to start afresh.
    overflow: Option<Failures>,
    free: u32,
    ceiling: Duration,
}

impl<K> FailureTable<K>
where
    K: Eq + Hash + Clone,
{
    fn new(free: u32, ceiling: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            overflow: None,
            free,
            ceiling,
        }
    }

    fn forget_expired(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.duration_since(entry.last_failure) < FAILURE_MEMORY);
        if self
            .overflow
            .as_ref()
            .is_some_and(|entry| now.duration_since(entry.last_failure) >= FAILURE_MEMORY)
        {
            self.overflow = None;
        }
    }

    fn wait(&self, key: &K, now: Instant) -> Duration {
        let entry = match self.entries.get(key) {
            Some(entry) => Some(entry),
            None if self.entries.len() >= MAX_TRACKED_KEYS => self.overflow.as_ref(),
            None => None,
        };
        entry
            .filter(|entry| now.duration_since(entry.last_failure) < FAILURE_MEMORY)
            .map_or(Duration::ZERO, |entry| {
                entry.blocked_until.saturating_duration_since(now)
            })
    }

    fn record_failure(&mut self, key: K, now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_TRACKED_KEYS {
            self.forget_expired(now);
        }
        let entry = if self.entries.contains_key(&key) || self.entries.len() < MAX_TRACKED_KEYS {
            self.entries.entry(key).or_insert(Failures {
                count: 0,
                blocked_until: now,
                last_failure: now,
            })
        } else {
            self.overflow.get_or_insert(Failures {
                count: 0,
                blocked_until: now,
                last_failure: now,
            })
        };
        if now.duration_since(entry.last_failure) >= FAILURE_MEMORY {
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
        entry.last_failure = now;
        entry.blocked_until = now + delay_after(entry.count, self.free, self.ceiling);
    }

    fn clear(&mut self, key: &K) {
        self.entries.remove(key);
    }
}

struct FailureState {
    pairs: FailureTable<(IpAddr, String)>,
    accounts: FailureTable<String>,
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
                accounts: FailureTable::new(FREE_ACCOUNT_FAILURES, MAX_ACCOUNT_DELAY),
            })),
        }
    }

    /// Whole seconds this client must still wait before trying this account,
    /// or `None` when it may try now. Never zero: a wait under a second rounds up.
    pub fn wait(&self, address: IpAddr, account: &str) -> Option<u64> {
        self.wait_at(address, account, Instant::now())
    }

    fn wait_at(&self, address: IpAddr, account: &str, now: Instant) -> Option<u64> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let pair = (cohort(address), account.to_owned());
        let wait = state
            .pairs
            .wait(&pair, now)
            .max(state.accounts.wait(&pair.1, now));
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
        state.accounts.record_failure(account.to_owned(), now);
    }

    /// A correct password proves the owner is present: forgive everything.
    pub fn record_success(&self, address: IpAddr, account: &str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.pairs.clear(&(cohort(address), account.to_owned()));
        state.accounts.clear(&account.to_owned());
    }
}

#[derive(Debug)]
struct Window {
    started: Instant,
    used: u32,
}

/// Registrations (and taken-email answers) one address cohort may receive per
/// window. The window starts at its first use and is dropped, never reset,
/// when it ends, so the reclamation order is the insertion order.
pub struct WindowLimiter {
    entries: Mutex<WindowState>,
    window: Duration,
    allowance: u32,
}

struct WindowState {
    entries: HashMap<IpAddr, Window>,
    order: VecDeque<(IpAddr, Instant)>,
    overflow: Option<Window>,
}

impl WindowLimiter {
    pub fn new(allowance: u32, window: Duration) -> Self {
        Self {
            entries: Mutex::new(WindowState {
                entries: HashMap::new(),
                order: VecDeque::new(),
                overflow: None,
            }),
            window,
            allowance,
        }
    }

    pub fn allow(&self, address: IpAddr) -> bool {
        self.allow_at(address, Instant::now())
    }

    fn allow_at(&self, address: IpAddr, now: Instant) -> bool {
        let address = cohort(address);
        let mut state = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while let Some((key, started)) = state.order.front().copied() {
            if now.duration_since(started) < self.window {
                break;
            }
            state.order.pop_front();
            state.entries.remove(&key);
        }
        if state
            .overflow
            .as_ref()
            .is_some_and(|window| now.duration_since(window.started) >= self.window)
        {
            state.overflow = None;
        }
        let window = if state.entries.contains_key(&address) {
            state.entries.get_mut(&address).expect("checked above")
        } else if state.entries.len() < MAX_TRACKED_KEYS {
            state.order.push_back((address, now));
            state.entries.entry(address).or_insert(Window {
                started: now,
                used: 0,
            })
        } else {
            state.overflow.get_or_insert(Window {
                started: now,
                used: 0,
            })
        };
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
        // Other accounts are untouched; another address meets only the account
        // schedule (fifteen failures: five past its allowance, sixteen seconds).
        assert_eq!(limiter.wait_at(HOME, "b@example.test", start), None);
        assert_eq!(limiter.wait_at(OTHER, "a@example.test", start), Some(16));
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
    fn strangers_spread_over_addresses_slow_an_account_but_never_past_its_ceiling() {
        let limiter = FailureLimiter::new();
        let start = Instant::now();
        for host in 1..=40u8 {
            let address = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, host));
            limiter.record_failure_at(address, "victim@example.test", start);
        }
        // Each of those addresses is below its own free allowance; the account
        // schedule alone makes the owner wait, and only for the ceiling.
        assert_eq!(
            limiter.wait_at(HOME, "victim@example.test", start),
            Some(MAX_ACCOUNT_DELAY.as_secs())
        );
        assert_eq!(
            limiter.wait_at(HOME, "victim@example.test", start + MAX_ACCOUNT_DELAY),
            None
        );
        // The owner's own success from home clears the account schedule too.
        limiter.record_success(HOME, "victim@example.test");
        assert_eq!(limiter.wait_at(HOME, "victim@example.test", start), None);
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
    fn a_full_table_shares_one_allowance_for_newcomers_and_keeps_live_records() {
        let mut table: FailureTable<u32> = FailureTable::new(1, Duration::from_secs(8));
        let start = Instant::now();
        for key in 0..MAX_TRACKED_KEYS as u32 {
            table.record_failure(key, start);
        }
        table.record_failure(0, start);
        table.record_failure(0, start);
        assert_eq!(table.wait(&0, start), Duration::from_secs(2));
        // Newcomers now share the overflow record.
        table.record_failure(u32::MAX, start);
        table.record_failure(u32::MAX - 1, start);
        assert_eq!(table.wait(&(u32::MAX - 2), start), Duration::from_secs(1));
        assert_eq!(table.wait(&0, start), Duration::from_secs(2));
        // Once the old records expire, newcomers get their own again.
        let later = start + FAILURE_MEMORY;
        table.record_failure(u32::MAX, later);
        assert_eq!(table.entries.len(), 1);
        assert_eq!(table.wait(&u32::MAX, later), Duration::ZERO);
        assert_eq!(table.entries[&u32::MAX].count, 1);
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
