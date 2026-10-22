//! Whole-operation scheduling and descriptor reservations, not OS guarantees.

use super::failure;
use crate::tooling::Result;
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const ENTRY: Duration = Duration::from_secs(600);
pub(super) const HASH: Duration = Duration::from_secs(120);
pub(super) const REPLAY: Duration = Duration::from_secs(25);
pub(super) const FILES: Duration = Duration::from_secs(20);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(45);
pub(super) const GRACE: Duration = Duration::from_secs(5);
pub(super) const CAPS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(120),
];

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
pub(super) struct NativeClock;
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub(super) struct Context {
    pub clock: Arc<dyn Clock>,
    start: Instant,
    last: Cell<Instant>,
    pub hash_used: Cell<Duration>,
    calls: Cell<usize>,
}
impl Context {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let start = clock.now();
        Self {
            clock,
            start,
            last: Cell::new(start),
            hash_used: Cell::new(Duration::ZERO),
            calls: Cell::new(0),
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
    pub fn remaining(&self) -> Result<Duration> {
        self.end()
            .checked_duration_since(self.now()?)
            .filter(|v| !v.is_zero())
            .ok_or_else(|| failure(124))
    }
    pub fn tail(&self) -> Result<Duration> {
        if self.calls.get() > CAPS.len() {
            return Err(failure(1));
        }
        let mut remaining = HASH.saturating_sub(self.hash_used.get()) + REPLAY + FILES;
        for cap in &CAPS[self.calls.get()..] {
            remaining += *cap + SETTLEMENT;
        }
        Ok(remaining)
    }
    pub fn admit(&self) -> Result<()> {
        if self.remaining()? < self.tail()? {
            return Err(failure(124));
        }
        Ok(())
    }
    pub fn command(&self, index: usize) -> Result<Duration> {
        if self.calls.get() != index || index >= CAPS.len() {
            return Err(failure(1));
        }
        self.admit()?;
        Ok(CAPS[index])
    }
    pub fn dispatch_window(&self) -> Result<(Instant, Instant)> {
        let now = self.now()?;
        let latest = self
            .end()
            .checked_sub(self.tail()?)
            .ok_or_else(|| failure(124))?;
        if now > latest {
            return Err(failure(124));
        }
        Ok((now, latest))
    }
    pub fn complete_command(&self, index: usize) -> Result<()> {
        if self.calls.get() != index {
            return Err(failure(1));
        }
        self.calls.set(index + 1);
        self.admit()
    }
    pub fn hash_window(&self) -> Result<(Instant, Duration)> {
        let start = self.now()?;
        let remaining = self
            .end()
            .checked_duration_since(start)
            .ok_or_else(|| failure(124))?;
        if remaining < self.tail()? {
            return Err(failure(124));
        }
        let slot = HASH
            .checked_sub(self.hash_used.get())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| failure(124))?;
        Ok((start, slot))
    }
    pub fn replay_deadline(&self) -> Result<Instant> {
        let now = self.now()?;
        let left = self
            .end()
            .checked_duration_since(now)
            .ok_or_else(|| failure(124))?;
        if left <= FILES {
            return Err(failure(124));
        }
        Ok(now + REPLAY.min(left - FILES))
    }
}
