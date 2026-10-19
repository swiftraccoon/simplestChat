//! Token-bucket rate limits for one connection, counted in microseconds of
//! capacity so sub-second refills stay exact without floating point.

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
}
