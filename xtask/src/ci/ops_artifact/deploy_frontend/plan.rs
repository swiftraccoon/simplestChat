//! One original monotonic envelope, including each remaining phase's debt.
//!
//! The new full-seam Ansible ceiling covers its added authenticated Ops delivery.
//! It does not change the frozen app-only frontend or any existing leaf limit.
//! File work is cooperative: blocked OS I/O and reaping are not hard wall bounds.

use super::failure;
use crate::tooling::Result;
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const COMMAND: Duration = Duration::from_secs(30);
pub(super) const GRACE: Duration = Duration::from_secs(5);
pub(super) const CAPTURE: Duration = Duration::from_secs(20);
pub(super) const PLAYBOOK: Duration = Duration::from_secs(7200);
pub(super) const PLAYBOOK_GRACE: Duration = Duration::from_secs(240);
pub(super) const FINAL: Duration = Duration::from_secs(60);
const FIXED: [u64; 6] = [420, 360, 540, 7490, 220, 60];

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
struct NativeClock;
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
pub(super) struct Plan {
    pub clock: Arc<dyn Clock>,
    origin: Instant,
    last: Cell<Instant>,
    phases: [Duration; 6],
    next: Cell<usize>,
    active: Cell<Option<Instant>>,
    end: Instant,
}
impl Plan {
    pub fn native(start: Instant, wait: u64) -> Self {
        Self::from_start(Arc::new(NativeClock), start, wait)
    }
    fn from_start(clock: Arc<dyn Clock>, origin: Instant, wait: u64) -> Self {
        let mut phases = FIXED.map(Duration::from_secs);
        phases[1] += Duration::from_secs(wait);
        let total: Duration = phases.iter().copied().sum();
        Self {
            clock,
            origin,
            last: origin.into(),
            phases,
            next: 0.into(),
            active: None.into(),
            end: origin + total + Duration::from_secs(30),
        }
    }
    pub fn origin(&self) -> Instant {
        self.origin
    }
    pub fn end(&self) -> Instant {
        self.end
    }
    pub fn now(&self) -> Result<Instant> {
        let now = self.clock.now();
        if now < self.origin || now < self.last.get() {
            return Err(failure(1));
        }
        self.last.set(now);
        if now >= self.end {
            return Err(failure(124));
        }
        Ok(now)
    }
    pub fn begin(&self, phase: usize) -> Result<Instant> {
        if phase >= self.phases.len() || phase != self.next.get() {
            return Err(failure(1));
        }
        let now = self.now()?;
        if let Some(end) = self.active.get() {
            if now >= end {
                return Err(failure(124));
            }
            return Ok(end);
        }
        let debt: Duration = self.phases[phase..].iter().copied().sum();
        if self.end.saturating_duration_since(now) < debt {
            return Err(failure(124));
        }
        let end = now + self.phases[phase];
        self.active.set(Some(end));
        Ok(end)
    }
    pub fn complete(&self, phase: usize, end: Instant) -> Result<()> {
        if phase != self.next.get() || self.active.get() != Some(end) {
            return Err(failure(1));
        }
        if self.now()? >= end {
            return Err(failure(124));
        }
        self.next.set(phase + 1);
        self.active.set(None);
        Ok(())
    }
    /// One observation admits all command and finalizer debt; no later clock
    /// observation may reset the enclosing phase or give away its tail.
    pub fn dispatch(&self, end: Instant, body: Duration, grace: Duration) -> Result<Instant> {
        let now = self.now()?;
        if self.active.get().is_none_or(|active| end > active) {
            return Err(failure(1));
        }
        let debt = body + grace + CAPTURE;
        if end > self.end || end.saturating_duration_since(now) < debt {
            return Err(failure(124));
        }
        Ok(end - debt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct ClockAt(std::sync::Mutex<Instant>);
    impl Clock for ClockAt {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }
    #[test]
    fn original_phase_and_remaining_debt_survive_advancing_and_backwards_clocks() {
        let start = Instant::now();
        let clock = Arc::new(ClockAt(std::sync::Mutex::new(start)));
        let plan = Plan::from_start(clock.clone(), start, 0);
        let end = plan.begin(0).unwrap();
        *clock.0.lock().unwrap() = end - Duration::from_secs(1);
        assert_eq!(plan.dispatch(end, COMMAND, GRACE).unwrap_err().code, 124);
        plan.complete(0, end).unwrap();
        *clock.0.lock().unwrap() = end + Duration::from_secs(31);
        assert_eq!(plan.begin(1).unwrap_err().code, 124);
        *clock.0.lock().unwrap() = start;
        assert_eq!(plan.now().unwrap_err().code, 1);
    }
    #[test]
    fn exact_whole_command_tail_is_admitted_but_one_nanosecond_less_is_refused() {
        let start = Instant::now();
        let clock = Arc::new(ClockAt(std::sync::Mutex::new(start)));
        let plan = Plan::from_start(clock.clone(), start, 0);
        plan.begin(0).unwrap();
        let end = start + COMMAND + GRACE + CAPTURE;
        assert_eq!(plan.dispatch(end, COMMAND, GRACE).unwrap(), start);
        *clock.0.lock().unwrap() += Duration::from_nanos(1);
        assert_eq!(plan.dispatch(end, COMMAND, GRACE).unwrap_err().code, 124);
    }
}
