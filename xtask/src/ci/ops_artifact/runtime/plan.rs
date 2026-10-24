//! A single monotonic entry, with complete remaining phase and retirement slots.

use super::failure;
use crate::tooling::Result;
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const ENTRY: Duration = Duration::from_secs(4200);
pub(super) const KEEPER: Duration = Duration::from_secs(1800);
pub(super) const GRACE: Duration = Duration::from_secs(120);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(20);
pub(super) const PHASES: [Duration; 7] = [
    Duration::from_secs(150),
    Duration::from_secs(1440),
    Duration::from_secs(180),
    Duration::from_secs(1970),
    Duration::from_secs(120),
    Duration::from_secs(25),
    Duration::from_secs(15),
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
    last: Cell<Instant>,
    next: Cell<usize>,
}
impl Plan {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let start = clock.now();
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
    pub fn end(&self) -> Instant {
        self.start + ENTRY
    }
    pub fn check(&self) -> Result<()> {
        if self.now()? >= self.end() {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn begin(&self, phase: usize) -> Result<Instant> {
        if phase >= PHASES.len() || self.next.get() != phase {
            return Err(failure(1));
        }
        let now = self.now()?;
        let tail: Duration = PHASES[phase..].iter().copied().sum();
        if self.end().saturating_duration_since(now) < tail {
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
    pub fn dispatch(&self, end: Instant, body: Duration, grace: Duration) -> Result<Instant> {
        let latest = end
            .checked_sub(body + grace + SETTLEMENT)
            .ok_or_else(|| failure(124))?;
        if self.now()? > latest {
            return Err(failure(124));
        }
        Ok(latest)
    }
}
