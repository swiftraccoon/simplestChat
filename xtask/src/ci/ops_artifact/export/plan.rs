//! Separate export owner: full existing acquisition plus export/replay/close debt.
//! These cooperative admission ceilings do not bound blocked OS calls or reaping.

use super::failure;
use crate::tooling::Result;
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const ENTRY: Duration = Duration::from_secs(1860);
pub(super) const PHASES: [Duration; 7] = [
    Duration::from_secs(30),
    Duration::from_secs(1440),
    Duration::from_secs(180),
    Duration::from_secs(60),
    Duration::from_secs(25),
    Duration::from_secs(60),
    Duration::from_secs(20),
];
pub(super) struct NativeClock;
impl super::super::acquire::AcquisitionClock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
pub(super) struct Plan {
    pub clock: Arc<dyn super::super::acquire::AcquisitionClock>,
    start: Instant,
    last: Cell<Instant>,
    next: Cell<usize>,
}
impl Plan {
    #[cfg(test)]
    pub fn new(clock: Arc<dyn super::super::acquire::AcquisitionClock>) -> Self {
        let start = clock.now();
        Self::from_start(clock, start)
    }
    pub fn from_start(
        clock: Arc<dyn super::super::acquire::AcquisitionClock>,
        start: Instant,
    ) -> Self {
        Self {
            clock,
            start,
            last: start.into(),
            next: 0.into(),
        }
    }
    pub fn now(&self) -> Result<Instant> {
        let now = self.clock.now();
        if now < self.start || now < self.last.get() {
            return Err(failure(1));
        }
        self.last.set(now);
        Ok(now)
    }
    pub fn check(&self) -> Result<()> {
        if self.now()? >= self.start + ENTRY {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn origin(&self) -> Instant {
        self.start
    }
    pub fn end(&self) -> Instant {
        self.start + ENTRY
    }
    pub fn begin(&self, phase: usize) -> Result<Instant> {
        if phase >= PHASES.len() || self.next.get() != phase {
            return Err(failure(1));
        }
        let now = self.now()?;
        let tail: Duration = PHASES[phase..].iter().copied().sum();
        if (self.start + ENTRY).saturating_duration_since(now) < tail {
            return Err(failure(124));
        }
        Ok(now + PHASES[phase])
    }
    pub fn complete(&self, phase: usize, end: Instant) -> Result<()> {
        if phase >= PHASES.len() || self.next.get() != phase {
            return Err(failure(1));
        }
        if self.now()? >= end {
            return Err(failure(124));
        }
        self.next.set(phase + 1);
        self.check()
    }
}
