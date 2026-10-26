//! One original outer interval and synchronous completion signal owner.

use super::{Error, Result};
use simplestchat_ops::owned_process::CompletionSignals;
use std::time::{Duration, Instant};

pub(super) const WHOLE: Duration = Duration::from_secs(1800);
pub(super) const FINAL: Duration = Duration::from_secs(120);
pub(super) const SETUP: Duration = Duration::from_secs(510);
pub(super) const DEBT: Duration = Duration::from_secs(1170);

pub(super) struct Control {
    pub start: Instant,
    pub setup: bool,
    pub settling: bool,
    pub first: Option<Error>,
    pub status: u8,
    pub cleanup_failed: bool,
    signals: CompletionSignals,
}
impl Control {
    pub fn new(start: Instant) -> Result<Self> {
        Ok(Self {
            start,
            setup: true,
            settling: false,
            first: None,
            status: 0,
            cleanup_failed: false,
            signals: CompletionSignals::new().map_err(|_| Error::Io)?,
        })
    }
    pub fn record(&mut self, error: Error) {
        self.first.get_or_insert(error);
    }
    pub fn end(&self) -> Instant {
        self.start + WHOLE
    }
    pub fn whole_body_end(&self) -> Instant {
        self.end() - FINAL
    }
    pub fn body_end(&self) -> Instant {
        if self.settling {
            self.end()
        } else if self.setup {
            (self.start + SETUP).min(self.whole_body_end())
        } else {
            self.whole_body_end()
        }
    }
    pub fn check(&mut self) -> Result<()> {
        self.check_live(true)
    }
    /// Fixed known-work retirement may observe the live phase despite a latched
    /// primary. It never widens the phase, clears a cause or authorizes success.
    pub fn check_retirement(&mut self) -> Result<()> {
        self.check_live(false)
    }
    fn check_live(&mut self, body: bool) -> Result<()> {
        if let Some(code) = self.signals.status() {
            if self.status == 0 {
                self.status = code;
            }
            self.record(if code == 130 {
                Error::Interrupt
            } else {
                Error::Terminate
            });
        }
        let now = Instant::now();
        if now < self.start || now >= self.body_end() {
            self.record(Error::Deadline);
            return Err(Error::Deadline);
        }
        if body
            && !self.settling
            && let Some(first) = self.first
        {
            return Err(first);
        }
        Ok(())
    }
    pub fn remaining(&mut self, cap: Duration) -> Result<Duration> {
        self.check()?;
        let left = self.body_end().saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(Error::Deadline)
        } else {
            Ok(left.min(cap))
        }
    }
    pub fn advance(&mut self) -> Result<()> {
        self.check()?;
        if !self.setup
            || self
                .whole_body_end()
                .saturating_duration_since(Instant::now())
                < DEBT
        {
            return Err(Error::Deadline);
        }
        self.check()?;
        self.setup = false;
        Ok(())
    }
    pub fn settle(&mut self) {
        if let Err(error) = self.check() {
            self.record(error);
        }
        self.settling = true;
    }
    pub fn finish(&mut self) -> Result<()> {
        match self.signals.finish() {
            Ok(Some(code)) => {
                if self.status == 0 {
                    self.status = code;
                }
                self.record(if code == 130 {
                    Error::Interrupt
                } else {
                    Error::Terminate
                });
            }
            Ok(None) => {}
            Err(_) => self.record(Error::Close),
        }
        if let Err(error) = self.check() {
            self.record(error);
        }
        self.first.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_expiry_cannot_widen_or_borrow_another_runtime_profile() {
        let mut owner = Control::new(Instant::now() - SETUP).unwrap();
        assert!(owner.advance().is_err());
        assert!(owner.setup);
        owner.settle();
        assert_eq!(owner.end() - owner.start, WHOLE);
        assert_eq!(
            owner.whole_body_end() - owner.start,
            Duration::from_secs(1680)
        );
        assert_eq!(owner.finish(), Err(Error::Deadline));
    }
}
