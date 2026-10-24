//! One original monotonic deadline, first cause and native signal registration.

use crate::owned_process::Signals;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cause {
    Input,
    Policy,
    Archive,
    Changed,
    Io,
    Close,
    Deadline,
    Interrupt,
    Terminate,
}
pub(super) type Result<T> = std::result::Result<T, Cause>;
#[derive(Clone, Copy)]
pub(super) enum Window {
    Body,
    Final,
}
pub(super) struct Owner {
    start: Instant,
    end: Instant,
    body: Instant,
    signals: Signals,
    primary: Option<Cause>,
    #[cfg(test)]
    pub elapsed: Option<Duration>,
    #[cfg(test)]
    pub today: Option<chrono::NaiveDate>,
    #[cfg(test)]
    pub utc_error: bool,
}
impl Owner {
    pub fn new(start: Instant, whole: Duration, reserve: Duration) -> Result<Self> {
        if reserve.is_zero() || reserve >= whole {
            return Err(Cause::Input);
        }
        let end = start.checked_add(whole).ok_or(Cause::Input)?;
        let body = end.checked_sub(reserve).ok_or(Cause::Input)?;
        Ok(Self {
            start,
            end,
            body,
            signals: Signals::new().map_err(|_| Cause::Io)?,
            primary: None,
            #[cfg(test)]
            elapsed: None,
            #[cfg(test)]
            today: None,
            #[cfg(test)]
            utc_error: false,
        })
    }
    pub fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(elapsed) = self.elapsed {
            return self.start + elapsed;
        }
        Instant::now()
    }
    pub fn record(&mut self, error: Cause) {
        self.primary.get_or_insert(error);
    }
    pub fn error(&self) -> Option<Cause> {
        self.primary
    }
    pub fn check(&mut self, window: Window) -> Result<()> {
        if self.primary.is_none() {
            if let Some(signal) = self.signals.pending_now() {
                self.record(if signal == 130 {
                    Cause::Interrupt
                } else {
                    Cause::Terminate
                });
            }
            if self.now() < self.start
                || self.now()
                    >= match window {
                        Window::Body => self.body,
                        Window::Final => self.end,
                    }
            {
                self.record(Cause::Deadline);
            }
        }
        self.primary.map_or(Ok(()), Err)
    }
    pub fn utc_date(&mut self) -> Result<chrono::NaiveDate> {
        self.check(Window::Body)?;
        #[cfg(test)]
        if self.utc_error {
            self.record(Cause::Io);
            return Err(Cause::Io);
        }
        #[cfg(test)]
        if let Some(date) = self.today {
            return Ok(date);
        }
        let seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => i64::try_from(value.as_secs()).map_err(|_| Cause::Io)?,
            Err(error) => {
                let value = error.duration();
                i64::try_from(value.as_secs())
                    .ok()
                    .and_then(i64::checked_neg)
                    .and_then(|seconds| seconds.checked_sub(i64::from(value.subsec_nanos() != 0)))
                    .ok_or(Cause::Io)?
            }
        };
        chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
            .map(|value| value.date_naive())
            .ok_or(Cause::Io)
    }
}
pub(super) fn io<T>(owner: &mut Owner, value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| {
        owner.record(Cause::Io);
        Cause::Io
    })
}
pub(super) fn failed<T>(owner: &mut Owner, error: Cause) -> Result<T> {
    owner.record(error);
    Err(error)
}
