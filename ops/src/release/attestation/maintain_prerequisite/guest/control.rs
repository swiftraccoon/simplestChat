//! One entry origin, narrowed request intervals and synchronous signal custody.

use super::{Error, Result, protocol::Request};
use crate::owned_process::CompletionSignals;
use std::time::{Duration, Instant};

pub(super) struct Control {
    pub start: Instant,
    pub status: u8,
    pub checkpoint: &'static str,
    pub first: Option<Error>,
    pub cleanup_failed: bool,
    setup: bool,
    settling: bool,
    recovering: bool,
    setup_end: Instant,
    body: Instant,
    end: Instant,
    phase: Option<Instant>,
    signals: CompletionSignals,
}

impl Control {
    pub fn new(start: Instant) -> Result<Self> {
        Ok(Self {
            start,
            status: 0,
            checkpoint: "input",
            first: None,
            cleanup_failed: false,
            setup: true,
            settling: false,
            recovering: false,
            setup_end: start + Duration::from_secs(30),
            body: start + Duration::from_secs(1680),
            end: start + Duration::from_secs(1710),
            phase: None,
            signals: CompletionSignals::new().map_err(|_| Error::Io)?,
        })
    }
    pub fn record(&mut self, error: Error) {
        self.first.get_or_insert(error);
    }
    pub fn configure(&mut self, request: &Request) -> Result<()> {
        self.check()?;
        self.setup_end = self.start + Duration::from_secs(request.setup_seconds);
        self.body = self.start + Duration::from_secs(request.body_seconds);
        self.end = self.start + Duration::from_secs(request.end_seconds);
        if self.end <= self.body || self.end.duration_since(self.body) > Duration::from_secs(120) {
            return Err(Error::Input);
        }
        self.check()
    }
    pub fn end(&self) -> Instant {
        self.end
    }
    pub fn body_end(&self) -> Instant {
        self.body
    }
    pub fn is_recovering(&self) -> bool {
        self.recovering
    }
    pub fn phase_end(&self) -> Instant {
        let end = if self.settling {
            self.end
        } else if self.recovering {
            self.body
        } else if self.setup {
            self.setup_end.min(self.body)
        } else {
            self.body
        };
        if self.settling {
            end
        } else {
            self.phase.map_or(end, |phase| phase.min(end))
        }
    }
    pub fn check(&mut self) -> Result<()> {
        self.check_after_failure(None)
    }
    pub fn check_after_failure(&mut self, prior: Option<Error>) -> Result<()> {
        let signal = self.signals.status();
        let now = Instant::now();
        let expired = now < self.start || now >= self.phase_end();
        let status = if self.status == 0 {
            signal.unwrap_or(0)
        } else {
            self.status
        };
        if (expired || (!self.settling && !self.recovering && [124, 130, 143].contains(&status)))
            && let Some(error) = prior
        {
            self.record(error);
        }
        if let Some(status) = signal {
            if self.status == 0 {
                self.status = status;
            }
            self.record(if status == 130 {
                Error::Interrupt
            } else {
                Error::Terminate
            });
        }
        if expired {
            if self.status == 0 {
                self.status = 124;
            }
            self.record(Error::Deadline);
            return Err(Error::Deadline);
        }
        if !self.settling && !self.recovering {
            match self.status {
                130 => return Err(Error::Interrupt),
                143 => return Err(Error::Terminate),
                124 => return Err(Error::Deadline),
                _ => {}
            }
        }
        Ok(())
    }
    pub fn remaining(&mut self, maximum: Duration) -> Result<Duration> {
        self.check()?;
        let value = self
            .phase_end()
            .saturating_duration_since(Instant::now())
            .min(maximum);
        if value.is_zero() {
            Err(Error::Deadline)
        } else {
            Ok(value)
        }
    }
    pub fn push_phase(&mut self, maximum: Duration) -> Result<Option<Instant>> {
        self.check()?;
        let previous = self.phase;
        self.phase = Some((Instant::now() + maximum).min(self.phase_end()));
        self.check()?;
        Ok(previous)
    }
    pub fn pop_phase(&mut self, previous: Option<Instant>) -> Result<()> {
        let checked = self.check();
        self.phase = previous;
        checked
    }
    pub fn advance(&mut self) -> Result<()> {
        self.check()?;
        if !self.setup
            || self.cleanup_failed
            || self.body.saturating_duration_since(Instant::now()) < Duration::from_secs(1170)
        {
            return Err(Error::Deadline);
        }
        self.check()?;
        self.setup = false;
        Ok(())
    }
    pub fn begin_recovery(&mut self) {
        self.recovering = true;
        self.phase = None;
    }
    pub fn begin_settling(&mut self) {
        if let Err(error) = self.check() {
            self.record(error);
        }
        self.settling = true;
        self.phase = None;
    }
    pub fn finish(&mut self) -> Result<()> {
        match self.signals.finish() {
            Ok(Some(status)) => {
                if self.status == 0 {
                    self.status = status;
                }
                self.record(if status == 130 {
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
    fn expired_setup_never_widens_go_or_resets_the_original_final_end() {
        let mut owner = Control::new(Instant::now()).unwrap();
        let end = owner.end;
        owner.setup_end = Instant::now() - Duration::from_secs(1);
        assert_eq!(owner.advance(), Err(Error::Deadline));
        assert!(owner.setup);
        assert_eq!(owner.end, end);
        owner.begin_recovery();
        assert_eq!(owner.status, 124);
        owner.begin_settling();
        assert_eq!(owner.finish(), Err(Error::Deadline));
    }
    #[test]
    fn earlier_io_and_close_uncertainty_survive_later_expiry_and_final_settlement() {
        let mut owner = Control::new(Instant::now()).unwrap();
        owner.record(Error::Io);
        owner.cleanup_failed = true;
        owner.setup_end = Instant::now() - Duration::from_secs(1);
        assert_eq!(owner.check(), Err(Error::Deadline));
        assert_eq!(owner.first, Some(Error::Io));
        owner.begin_settling();
        assert_eq!(owner.finish(), Err(Error::Io));
        assert!(owner.cleanup_failed);
    }
    #[test]
    fn only_a_failing_completion_fence_latches_the_earlier_transient_error() {
        let mut owner = Control::new(Instant::now()).unwrap();
        let original_end = owner.end();
        assert_eq!(owner.check_after_failure(Some(Error::Io)), Ok(()));
        assert_eq!(owner.first, None);
        assert_eq!(owner.status, 0);
        owner.setup_end = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            owner.check_after_failure(Some(Error::Io)),
            Err(Error::Deadline)
        );
        assert_eq!(owner.first, Some(Error::Io));
        assert_eq!(owner.status, 124);
        assert_eq!(owner.end(), original_end);
        assert!(!owner.cleanup_failed);
        owner.begin_settling();
        assert_eq!(owner.finish(), Err(Error::Io));
    }
}
