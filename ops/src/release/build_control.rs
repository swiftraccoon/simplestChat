//! One original monotonic owner and the release operation's fixed failures.

use crate::owned_process::Signals;
use std::time::{Duration, Instant};

pub(super) type Result<T> = std::result::Result<T, Failure>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Input,
    Source,
    Docker,
    Archive,
    Changed,
    Io,
    Close,
    Deadline,
    Interrupt,
    Terminate,
    Command,
    Bounds,
    Utf8,
    Guard,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Input => "release_build_input_refused",
            Self::Source => "release_build_source_refused",
            Self::Docker => "release_build_docker_refused",
            Self::Archive => "release_build_archive_refused",
            Self::Changed => "release_build_original_changed",
            Self::Io => "release_build_io_failed",
            Self::Close => "release_build_close_failed",
            Self::Deadline => "release_build_deadline",
            Self::Interrupt => "release_build_interrupted",
            Self::Terminate => "release_build_terminated",
            Self::Command => "release_build_command_failed",
            Self::Bounds => "release_build_output_limited",
            Self::Utf8 => "release_build_inspection_utf8",
            Self::Guard => "release_build_workload_guard_refused",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for Failure {}

pub(super) const SETUP: Duration = Duration::from_secs(120);
pub(super) const VERIFICATION: Duration = Duration::from_secs(140);
pub(super) const TAIL: Duration = Duration::from_secs(40);
pub(super) const RETIRE_AND_DRAIN: Duration = Duration::from_secs(26);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mode {
    Build,
    Export,
}

pub(super) struct Owner {
    #[cfg(test)]
    started: Instant,
    end: Instant,
    signals: Signals,
    primary: Option<Failure>,
    #[cfg(test)]
    elapsed: Option<Duration>,
}
impl Owner {
    pub(super) fn deadline(started: Instant, mode: Mode, seconds: u64) -> Result<Instant> {
        let overhead = match mode {
            Mode::Build => 300,
            Mode::Export => 60,
        };
        let duration = Duration::from_secs(seconds.checked_add(overhead).ok_or(Failure::Input)?);
        started.checked_add(duration).ok_or(Failure::Input)
    }
    pub fn new(started: Instant, mode: Mode, seconds: u64) -> Result<Self> {
        Ok(Self {
            #[cfg(test)]
            started,
            end: Self::deadline(started, mode, seconds)?,
            signals: Signals::new().map_err(|_| Failure::Io)?,
            primary: None,
            #[cfg(test)]
            elapsed: None,
        })
    }
    pub fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(elapsed) = self.elapsed {
            return self.started + elapsed;
        }
        Instant::now()
    }
    pub fn end(&self) -> Instant {
        self.end
    }
    #[cfg(test)]
    pub fn started(&self) -> Instant {
        self.started
    }
    pub fn record(&mut self, failure: Failure) {
        self.primary.get_or_insert(failure);
    }
    pub fn primary(&self) -> Option<Failure> {
        self.primary
    }
    pub fn check(&mut self) -> Result<()> {
        if self.primary.is_none() {
            if let Some(code) = self.signals.pending_now() {
                self.record(if code == 130 {
                    Failure::Interrupt
                } else {
                    Failure::Terminate
                });
            }
            if self.now() >= self.end {
                self.record(Failure::Deadline);
            }
        }
        self.primary.map_or(Ok(()), Err)
    }
    /// A phase remains inside the original operation and includes its own peers.
    pub fn cleanup_before(&self, end: Instant) -> Result<()> {
        if self.now() >= end.min(self.end) {
            Err(Failure::Deadline)
        } else {
            Ok(())
        }
    }
    pub fn phase(&mut self, maximum: Duration, future: Duration) -> Result<Instant> {
        self.check()?;
        let end = self
            .end
            .checked_sub(future)
            .ok_or(Failure::Deadline)?
            .min(self.now().checked_add(maximum).ok_or(Failure::Deadline)?);
        self.before(end)?;
        Ok(end)
    }
    pub fn before(&mut self, end: Instant) -> Result<()> {
        self.check()?;
        if self.now() >= end.min(self.end) {
            self.record(Failure::Deadline);
        }
        self.primary.map_or(Ok(()), Err)
    }
    pub fn body(&mut self, phase: Instant, cap: Duration, future: Duration) -> Result<Instant> {
        self.check()?;
        let end = phase
            .min(
                self.end
                    .checked_sub(future + TAIL)
                    .ok_or(Failure::Deadline)?,
            )
            .min(self.now().checked_add(cap).ok_or(Failure::Deadline)?);
        self.before(end)?;
        Ok(end)
    }
    #[cfg(test)]
    pub fn advance(&mut self, duration: Duration) {
        self.elapsed = Some(self.elapsed.unwrap_or_default() + duration);
    }
    #[cfg(test)]
    pub fn expire(&mut self) {
        self.elapsed = Some(self.end.duration_since(self.started));
    }
}
