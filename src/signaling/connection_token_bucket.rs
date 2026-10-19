//! Token-bucket rate limits for one connection, counted in microseconds of
//! capacity so sub-second refills stay exact without floating point, and the
//! violation windows that close a connection after repeated refusals.

use std::time::Instant;

/// One token's worth of capacity.
pub(super) const TOKEN_US: u64 = 1_000_000;

/// Capacity that refills at `refill_rate` tokens per second up to `capacity_us`;
/// every admitted event spends one token.
#[derive(Debug, Clone)]
pub(super) struct TokenBucket {
    tokens_us: u64,
    capacity_us: u64,
    refill_rate: u64,
    last_refill: Instant,
}

impl TokenBucket {
    /// A full bucket as of `now`.
    pub(super) fn full(now: Instant, refill_rate: u64, capacity_us: u64) -> Self {
        Self {
            tokens_us: capacity_us,
            capacity_us,
            refill_rate,
            last_refill: now,
        }
    }

    /// Refills for the time since the last call and spends one token when one is there.
    pub(super) fn try_consume(&mut self, now: Instant) -> bool {
        let elapsed_us =
            u64::try_from(now.duration_since(self.last_refill).as_micros()).unwrap_or(u64::MAX);
        self.last_refill = now;
        self.tokens_us = self
            .tokens_us
            .saturating_add(elapsed_us.saturating_mul(self.refill_rate))
            .min(self.capacity_us);
        if self.tokens_us < TOKEN_US {
            return false;
        }
        self.tokens_us -= TOKEN_US;
        true
    }

    #[cfg(test)]
    pub(super) fn capacity_us(&self) -> u64 {
        self.capacity_us
    }
}

/// Refusals counted within a rolling window: the count starts over when a
/// refusal arrives once the window has elapsed since the first one.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ViolationWindow {
    started: Option<Instant>,
    violations: u32,
}

impl ViolationWindow {
    /// Records a refusal at `now` and returns the count within the current window.
    pub(super) fn record(&mut self, now: Instant, window: std::time::Duration) -> u32 {
        if self
            .started
            .is_none_or(|started| now.duration_since(started) >= window)
        {
            self.started = Some(now);
            self.violations = 0;
        }
        self.violations = self.violations.saturating_add(1);
        self.violations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn token_bucket_refills_and_remains_bounded() {
        let start = Instant::now();
        let mut bucket = TokenBucket::full(start, 2, 2 * TOKEN_US);
        assert!(bucket.try_consume(start));
        assert!(bucket.try_consume(start));
        assert!(!bucket.try_consume(start));
        // Half a second at two tokens per second refills one token.
        assert!(bucket.try_consume(start + Duration::from_millis(500)));
        assert!(!bucket.try_consume(start + Duration::from_millis(500)));
        // A long pause refills to the capacity, never beyond it.
        assert!(bucket.try_consume(start + Duration::from_secs(10)));
        assert!(bucket.try_consume(start + Duration::from_secs(10)));
        assert!(!bucket.try_consume(start + Duration::from_secs(10)));
    }

    #[test]
    fn violation_window_restarts_once_the_window_has_elapsed() {
        let started = Instant::now();
        let window = std::time::Duration::from_secs(10);
        let mut refusals = ViolationWindow::default();
        assert_eq!(refusals.record(started, window), 1);
        assert_eq!(
            refusals.record(started + std::time::Duration::from_secs(9), window),
            2
        );
        assert_eq!(
            refusals.record(started + std::time::Duration::from_secs(10), window),
            1
        );
        assert_eq!(
            refusals.record(started + std::time::Duration::from_secs(11), window),
            2
        );
    }
}
