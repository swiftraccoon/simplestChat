//! Whole hosted schedule. These are admission/watchdog limits, not guarantees
//! about cold downloads, compiler I/O, OS stalls, filesystem close or reaping.

use super::failure;
use crate::tooling::Result;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const ENTRY: Duration = Duration::from_secs(3600);
pub(super) const FINAL: Duration = Duration::from_secs(300);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(45);
pub(super) const DOWNLOAD: Duration = Duration::from_secs(120);
pub(super) const PHASES: [Duration; 5] = [
    Duration::from_secs(300),
    Duration::from_secs(900),
    Duration::from_secs(300),
    Duration::from_secs(540),
    Duration::from_secs(1200),
];

pub(in crate::ci::ops_artifact) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
pub(super) struct NativeClock;
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub(super) struct Plan {
    pub clock: Arc<dyn Clock>,
    start: Instant,
    limit: Duration,
    latest: std::cell::Cell<Instant>,
    next: std::cell::Cell<usize>,
}
impl Plan {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let start = clock.now();
        Self {
            clock,
            start,
            limit: ENTRY,
            latest: start.into(),
            next: 0.into(),
        }
    }
    pub fn signer(clock: Arc<dyn Clock>) -> Self {
        Self {
            limit: FINAL,
            ..Self::new(clock)
        }
    }
    pub fn now(&self) -> Result<Instant> {
        let now = self.clock.now();
        if now < self.latest.get() {
            return Err(failure(1));
        }
        self.latest.set(now);
        Ok(now)
    }
    pub fn end(&self) -> Instant {
        self.start + self.limit
    }
    /// Diagnostic-only age of the last existing validated owner sample. Borrowed
    /// stages can run between samples. Reading it observes no clock and changes
    /// no admission, deadline, phase or monotonicity state.
    pub fn observed_elapsed(&self) -> Duration {
        self.latest.get().saturating_duration_since(self.start)
    }
    pub fn check(&self) -> Result<()> {
        if self.now()? >= self.end() {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn begin(&self, index: usize) -> Result<Instant> {
        if self.next.get() != index || index >= PHASES.len() {
            return Err(failure(1));
        }
        let tail: Duration = PHASES[index..].iter().copied().sum::<Duration>() + FINAL;
        let now = self.now()?;
        if self.end().saturating_duration_since(now) < tail {
            return Err(failure(124));
        }
        Ok(now + PHASES[index])
    }
    pub fn complete(&self, index: usize, phase_end: Instant) -> Result<()> {
        if self.next.get() != index {
            return Err(failure(1));
        }
        let now = self.now()?;
        if now >= phase_end {
            return Err(failure(124));
        }
        self.next.set(index + 1);
        self.check()
    }
    pub fn finalizing(&self) -> Result<()> {
        if self.next.get() != PHASES.len() {
            return Err(failure(1));
        }
        self.check()
    }
    pub fn body(&self, end: Instant, maximum: Duration) -> Result<(Duration, Instant)> {
        let now = self.now()?;
        let available = end
            .checked_duration_since(now)
            .and_then(|d| d.checked_sub(SETTLEMENT + Duration::from_secs(1)))
            .filter(|d| !d.is_zero())
            .ok_or_else(|| failure(124))?;
        let body = available.min(maximum);
        let latest = end
            .checked_sub(body + SETTLEMENT)
            .ok_or_else(|| failure(124))?;
        if now >= latest {
            return Err(failure(124));
        }
        Ok((body, latest))
    }
}
