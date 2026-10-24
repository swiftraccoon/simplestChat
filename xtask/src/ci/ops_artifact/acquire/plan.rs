//! One remaining-tail schedule through acquisition and retained verification.

use super::failure;
use crate::tooling::Result;
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const ENTRY: Duration = Duration::from_secs(1440);
pub(super) const ARCHIVE: Duration = Duration::from_secs(180);
pub(super) const VERIFY: Duration = Duration::from_secs(600);
pub(super) const REPLAY: Duration = Duration::from_secs(25);
pub(super) const FILES: Duration = Duration::from_secs(20);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(45);
pub(super) const GRACE: Duration = Duration::from_secs(5);
pub(super) const CAPS: [Duration; 5] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(180),
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
pub(super) struct Context {
    pub clock: Arc<dyn Clock>,
    start: Instant,
    end: Instant,
    last: Cell<Instant>,
    calls: Cell<usize>,
    archive_used: Cell<Duration>,
    verified: Cell<bool>,
    replayed: Cell<bool>,
}
impl Context {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let start = clock.now();
        Self {
            clock,
            start,
            end: start + ENTRY,
            last: Cell::new(start),
            calls: Cell::new(0),
            archive_used: Cell::new(Duration::ZERO),
            verified: Cell::new(false),
            replayed: Cell::new(false),
        }
    }
    pub(super) fn bounded(clock: Arc<dyn Clock>, end: Instant) -> Self {
        let mut context = Self::new(clock);
        context.end = context.end.min(end);
        context
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
        self.end
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
        let mut tail = ARCHIVE.saturating_sub(self.archive_used.get()) + FILES;
        if !self.replayed.get() {
            tail += REPLAY;
        }
        if !self.verified.get() {
            tail += VERIFY;
        }
        for cap in &CAPS[self.calls.get()..] {
            tail += *cap + SETTLEMENT;
        }
        Ok(tail)
    }
    pub fn admit(&self) -> Result<()> {
        if self.remaining()? < self.tail()? {
            Err(failure(124))
        } else {
            Ok(())
        }
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
    pub fn archive_window(&self) -> Result<(Instant, Duration)> {
        let start = self.now()?;
        if self.end().saturating_duration_since(start) < self.tail()? {
            return Err(failure(124));
        }
        let slot = ARCHIVE
            .checked_sub(self.archive_used.get())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| failure(124))?;
        Ok((start, slot))
    }
    pub fn complete_archive(&self, elapsed: Duration) -> Result<()> {
        let used = self
            .archive_used
            .get()
            .checked_add(elapsed)
            .ok_or_else(|| failure(124))?;
        self.archive_used.set(used);
        if used > ARCHIVE {
            return Err(failure(124));
        }
        self.admit()
    }
    pub fn verification_window(&self) -> Result<Instant> {
        if self.calls.get() != CAPS.len() || self.verified.get() {
            return Err(failure(1));
        }
        self.admit()?;
        let now = self.now()?;
        if self.end().saturating_duration_since(now) < VERIFY + REPLAY + FILES {
            return Err(failure(124));
        }
        Ok(now + VERIFY)
    }
    pub fn complete_verification(&self) -> Result<()> {
        if self.verified.replace(true) {
            return Err(failure(1));
        }
        self.remaining().map(|_| ())
    }
    pub fn replay_deadline(&self) -> Result<Instant> {
        if self.replayed.get() {
            return Err(failure(1));
        }
        self.admit()?;
        let now = self.now()?;
        let left = self
            .end()
            .checked_duration_since(now)
            .ok_or_else(|| failure(124))?;
        let after = ARCHIVE.saturating_sub(self.archive_used.get()) + FILES;
        if left <= after {
            return Err(failure(124));
        }
        Ok(now + REPLAY.min(left - after))
    }
    pub fn complete_replay(&self) -> Result<()> {
        if self.replayed.replace(true) {
            return Err(failure(1));
        }
        self.admit()
    }
}
