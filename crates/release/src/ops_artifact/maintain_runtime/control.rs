//! One original outer interval and synchronous completion signal owner.

use super::{Error, Result, options::Purpose};
use simplestchat_ops::owned_process::CompletionSignals;
use std::time::{Duration, Instant};

pub(super) const WHOLE: Duration = Duration::from_secs(1800);
pub(super) const FINAL: Duration = Duration::from_secs(120);
pub(super) const SETUP: Duration = Duration::from_secs(510);
pub(super) const DEBT: Duration = Duration::from_secs(1170);

#[derive(Clone, Copy)]
struct Profile {
    whole: Duration,
    finalization: Duration,
    setup: Duration,
    debt: Duration,
}
impl Profile {
    fn for_purpose(purpose: Purpose) -> Self {
        match purpose {
            Purpose::Prerequisite => Self {
                whole: WHOLE,
                finalization: FINAL,
                setup: SETUP,
                debt: DEBT,
            },
            Purpose::Host => Self {
                whole: Duration::from_secs(3780),
                finalization: Duration::from_secs(120),
                setup: Duration::from_secs(1560),
                // Whole maintain1800 + post-observation120 + VM cleanup120.
                debt: Duration::from_secs(2040),
            },
            Purpose::Deploy => Self {
                whole: Duration::from_secs(4500),
                finalization: Duration::from_secs(180),
                setup: Duration::from_secs(600),
                // Both native1800 envelopes and the original observation120.
                debt: Duration::from_secs(3720),
            },
        }
    }
}

pub(super) struct Control {
    pub start: Instant,
    pub setup: bool,
    pub settling: bool,
    pub first: Option<Error>,
    pub status: u8,
    pub cleanup_failed: bool,
    purpose: Purpose,
    profile: Profile,
    signals: CompletionSignals,
}
impl Control {
    #[cfg(test)]
    pub fn new(start: Instant) -> Result<Self> {
        Self::for_purpose(start, Purpose::Prerequisite)
    }
    pub fn for_purpose(start: Instant, purpose: Purpose) -> Result<Self> {
        Ok(Self {
            start,
            setup: true,
            settling: false,
            first: None,
            status: 0,
            cleanup_failed: false,
            purpose,
            profile: Profile::for_purpose(purpose),
            signals: CompletionSignals::new().map_err(|_| Error::Io)?,
        })
    }
    pub fn record(&mut self, error: Error) {
        self.first.get_or_insert(error);
    }
    pub fn end(&self) -> Instant {
        self.start + self.profile.whole
    }
    pub fn whole_body_end(&self) -> Instant {
        self.end() - self.profile.finalization
    }
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }
    pub fn setup_limit(&self) -> Duration {
        self.profile.setup
    }
    pub fn debt(&self) -> Duration {
        self.profile.debt
    }
    pub fn whole(&self) -> Duration {
        self.profile.whole
    }
    pub fn response_debt(&self) -> Duration {
        match self.purpose {
            Purpose::Prerequisite => DEBT,
            Purpose::Host => Duration::from_secs(1920),
            Purpose::Deploy => Duration::from_secs(3720),
        }
    }
    pub fn body_end(&self) -> Instant {
        if self.settling {
            self.end()
        } else if self.setup {
            (self.start + self.profile.setup).min(self.whole_body_end())
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
                < self.profile.debt
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
    #[test]
    fn host_profile_is_explicit_and_preserves_every_prerequisite_ceiling() {
        let start = Instant::now();
        let mut original = Control::new(start).unwrap();
        let mut host = Control::for_purpose(start, Purpose::Host).unwrap();
        assert_eq!(original.end() - start, Duration::from_secs(1800));
        assert_eq!(original.body_end() - start, Duration::from_secs(510));
        assert_eq!(original.debt(), Duration::from_secs(1170));
        assert_eq!(host.end() - start, Duration::from_secs(3780));
        assert_eq!(host.whole_body_end() - start, Duration::from_secs(3660));
        assert_eq!(host.body_end() - start, Duration::from_secs(1560));
        assert_eq!(host.debt(), Duration::from_secs(2040));
        assert_eq!(host.response_debt(), Duration::from_secs(1920));
        assert_eq!(host.purpose(), Purpose::Host);
        assert_eq!(original.finish(), Ok(()));
        assert_eq!(host.finish(), Ok(()));
    }
    #[test]
    fn deploy_reserves_two_complete_native_calls_without_borrowing_setup() {
        let start = Instant::now();
        let mut deploy = Control::for_purpose(start, Purpose::Deploy).unwrap();
        assert_eq!(deploy.end() - start, Duration::from_secs(4500));
        assert_eq!(deploy.whole_body_end() - start, Duration::from_secs(4320));
        assert_eq!(deploy.body_end() - start, Duration::from_secs(600));
        assert_eq!(deploy.debt(), Duration::from_secs(3720));
        assert_eq!(deploy.response_debt(), Duration::from_secs(3720));
        deploy.advance().unwrap();
        assert_eq!(deploy.body_end(), deploy.whole_body_end());
        let repeated = deploy.advance().unwrap_err();
        assert_eq!(repeated, Error::Deadline);
        deploy.record(repeated);
        deploy.settle();
        assert_eq!(deploy.finish(), Err(Error::Deadline));

        let mut expired =
            Control::for_purpose(Instant::now() - Duration::from_secs(600), Purpose::Deploy)
                .unwrap();
        assert_eq!(expired.advance(), Err(Error::Deadline));
        assert!(expired.setup);
        expired.settle();
        assert_eq!(expired.end() - expired.start, Duration::from_secs(4500));
        assert_eq!(expired.finish(), Err(Error::Deadline));
    }
}
