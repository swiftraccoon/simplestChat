use std::time::{Duration, Instant};

const LAYER_UPDATE_INTERVAL: Duration = Duration::from_secs(2);

/// Coalesces estimates without losing the final change inside the update interval.
/// The deadline also bounds retries when a worker request fails or applies only
/// part of a participant's consumers. No history or per-event task is retained.
pub(super) struct BandwidthTierUpdate {
    desired: Option<u8>,
    applied: Option<u8>,
    next_attempt_at: Instant,
}

impl BandwidthTierUpdate {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            desired: None,
            applied: None,
            next_attempt_at: now + LAYER_UPDATE_INTERVAL,
        }
    }

    pub(super) fn observe(&mut self, bitrate: u32) {
        // Preserve the measured downgrade tiers; this only changes scheduling.
        self.desired = Some(if bitrate < 200_000 {
            0
        } else if bitrate < 600_000 {
            1
        } else {
            2
        });
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.desired
            .filter(|desired| Some(*desired) != self.applied)
            .map(|_| self.next_attempt_at)
    }

    pub(super) fn begin_attempt(&mut self, now: Instant) -> Option<u8> {
        let deadline = self.deadline()?;
        if now < deadline {
            return None;
        }
        self.next_attempt_at = now + LAYER_UPDATE_INTERVAL;
        self.desired
    }

    pub(super) fn complete_attempt(&mut self, tier: u8, succeeded: bool) {
        if succeeded {
            self.applied = Some(tier);
        } else {
            // A failed participant-wide write may already have changed some
            // consumers and the stored ceiling. Even the old tier needs a
            // retry if the estimate returns to it after that partial write.
            self.applied = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_recovery_inside_interval_keeps_a_timer_deadline() {
        let now = Instant::now();
        let mut update = BandwidthTierUpdate::new(now);
        update.observe(150_000);
        assert_eq!(update.begin_attempt(now + Duration::from_secs(2)), Some(0));
        update.complete_attempt(0, true);
        assert_eq!(update.deadline(), None);

        // There is deliberately no second high-bitrate event after this one.
        update.observe(900_000);
        assert_eq!(update.begin_attempt(now + Duration::from_secs(3)), None);
        assert_eq!(update.deadline(), Some(now + Duration::from_secs(4)));
        assert_eq!(update.begin_attempt(now + Duration::from_secs(4)), Some(2));
        update.complete_attempt(2, true);
        assert_eq!(update.deadline(), None);
    }

    #[test]
    fn failed_apply_remains_pending_and_retries_at_most_every_two_seconds() {
        let now = Instant::now();
        let mut update = BandwidthTierUpdate::new(now);
        update.observe(900_000);
        assert_eq!(update.begin_attempt(now + Duration::from_secs(2)), Some(2));
        update.complete_attempt(2, false);
        assert_eq!(update.deadline(), Some(now + Duration::from_secs(4)));
        assert_eq!(update.begin_attempt(now + Duration::from_secs(3)), None);
        assert_eq!(update.begin_attempt(now + Duration::from_secs(4)), Some(2));
        update.complete_attempt(2, true);
        assert_eq!(update.deadline(), None);
    }

    #[test]
    fn newest_estimate_replaces_pending_work_but_partial_failure_needs_repair() {
        let now = Instant::now();
        let mut update = BandwidthTierUpdate::new(now);
        update.observe(900_000);
        assert_eq!(update.begin_attempt(now + Duration::from_secs(2)), Some(2));
        update.complete_attempt(2, true);
        update.observe(150_000);
        update.observe(400_000);
        assert_eq!(update.begin_attempt(now + Duration::from_secs(4)), Some(1));
        update.complete_attempt(1, false);
        update.observe(900_000);
        assert_eq!(update.deadline(), Some(now + Duration::from_secs(6)));
        assert_eq!(update.begin_attempt(now + Duration::from_secs(6)), Some(2));
        update.complete_attempt(2, true);
        update.observe(150_000);
        update.observe(900_000);
        assert_eq!(update.deadline(), None);
    }

    #[test]
    fn existing_tier_boundaries_are_preserved() {
        let now = Instant::now();
        for (bitrate, tier) in [(199_999, 0), (200_000, 1), (599_999, 1), (600_000, 2)] {
            let mut update = BandwidthTierUpdate::new(now);
            update.observe(bitrate);
            assert_eq!(
                update.begin_attempt(now + Duration::from_secs(2)),
                Some(tier)
            );
        }
    }
}
