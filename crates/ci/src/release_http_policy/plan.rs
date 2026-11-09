//! One original entry clock; reservations count unfinished phases exactly once.

use super::{Result, fail};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
pub(super) struct RealClock;
impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Clone)]
pub(super) struct Time {
    pub clock: Arc<dyn Clock>,
    pub start: Instant,
    pub end: Instant,
    last: Arc<Mutex<Instant>>,
}
impl Time {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let start = clock.now();
        Self {
            clock,
            start,
            end: start + Duration::from_secs(180),
            last: Arc::new(Mutex::new(start)),
        }
    }
    pub fn now(&self) -> Result<Instant> {
        let now = self.clock.now();
        let mut last = self.last.lock().map_err(|_| fail(1, "http_policy_clock"))?;
        if now < self.start || now < *last {
            return Err(fail(1, "http_policy_clock"));
        }
        *last = now;
        if now >= self.end {
            return Err(fail(124, "http_policy_deadline"));
        }
        Ok(now)
    }
    pub fn admit(&self, seconds: u64) -> Result<Instant> {
        let now = self.now()?;
        if self.end.duration_since(now) < Duration::from_secs(seconds) {
            return Err(fail(124, "http_policy_deadline"));
        }
        Ok(now)
    }
    pub fn docker(&self, index: usize) -> Result<Instant> {
        let future_http = if index < 2 { 25 } else { 0 };
        self.admit((4 - index as u64) * 26 + future_http + 35)?;
        Ok(self.end - Duration::from_secs((4 - index as u64) * 26 + future_http + 35))
    }
    pub fn http(&self) -> Result<Instant> {
        Ok(self.admit(112)? + Duration::from_secs(25))
    }
    pub fn replay(&self) -> Result<Instant> {
        Ok((self.now()? + Duration::from_secs(25)).min(self.end))
    }
}
