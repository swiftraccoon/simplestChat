//! One original monotonic deadline, first cause and native signal registration.

#[path = "control_signals.rs"]
pub(super) mod signals;
use signals::Signals;
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
    Zip,
    Bounds,
    Command,
}
pub(super) type Result<T> = std::result::Result<T, Cause>;
#[derive(Clone, Copy)]
pub(super) enum Window {
    Body,
    Final,
}
#[cfg(test)]
pub(super) enum LateClock {
    Failure,
    Date(chrono::NaiveDate),
}
pub(super) struct Owner {
    start: Instant,
    end: Instant,
    body: Instant,
    receiver_protocol: Option<Instant>,
    signals: Signals,
    primary: Option<Cause>,
    #[cfg(test)]
    pub elapsed: Option<Duration>,
    #[cfg(test)]
    pub today: Option<chrono::NaiveDate>,
    #[cfg(test)]
    pub utc_error: bool,
    #[cfg(test)]
    pub late_clock: Option<LateClock>,
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
            receiver_protocol: None,
            signals: Signals::new().map_err(|_| Cause::Io)?,
            primary: None,
            #[cfg(test)]
            elapsed: None,
            #[cfg(test)]
            today: None,
            #[cfg(test)]
            utc_error: false,
            #[cfg(test)]
            late_clock: None,
        })
    }
    /// Consume the original owner only after runtime/output/umask restoration.
    /// Every registered signal is removed before retained flags and the original
    /// deadline are sampled. An earlier primary refusal is never replaced.
    pub fn finish(mut self) -> Result<()> {
        #[cfg(test)]
        if let Some(signal) = FINISH_SIGNAL.with(std::cell::Cell::take) {
            let mask = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
            let _ = nix::sys::stat::umask(mask);
            FINISH_MASK.set(Some(mask));
            if signal_hook::low_level::raise(signal).is_err() {
                self.record(Cause::Io);
            }
        }
        if !self.signals.unregister() {
            self.record(Cause::Close);
        }
        let result = self.check(Window::Final);
        #[cfg(test)]
        FINISHED.with(|finished| finished.set(Some(result)));
        result
    }
    pub fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(elapsed) = self.elapsed {
            return self.start + elapsed;
        }
        Instant::now()
    }
    pub fn end(&self) -> Instant {
        self.end
    }
    pub fn body_end(&self) -> Instant {
        self.body
    }
    /// Narrow the existing body checkpoints while the admitted SSH protocol
    /// still owes its original transport tail. This does not change B or N.
    pub fn begin_receiver_protocol(&mut self, end: Instant) -> Result<()> {
        self.check(Window::Body)?;
        if self.receiver_protocol.is_some() || end > self.body {
            return failed(self, Cause::Input);
        }
        self.receiver_protocol = Some(end);
        self.check(Window::Body)
    }
    /// Fence the last protocol observation before allowing its reserved tail.
    /// A previous failure remains primary; this grants only cleanup time.
    pub fn end_receiver_protocol(&mut self) {
        if self.receiver_protocol.is_some() {
            let _ = self.check(Window::Body);
            self.receiver_protocol = None;
        }
    }
    pub fn cleanup_before(&self, end: Instant) -> Result<()> {
        if self.now() >= end.min(self.end) {
            Err(Cause::Deadline)
        } else {
            Ok(())
        }
    }
    pub fn record(&mut self, error: Cause) {
        self.primary.get_or_insert(error);
    }
    /// Final report work may proceed after a primary failure. This never clears
    /// that cause, grants body authority, or replaces the original deadline.
    pub fn final_work(&mut self) -> Result<()> {
        if let Some(signal) = self.signals.pending_now() {
            let cause = if signal == 130 {
                Cause::Interrupt
            } else {
                Cause::Terminate
            };
            self.record(cause);
            return Err(cause);
        }
        if self.now() < self.start || self.now() >= self.end {
            self.record(Cause::Deadline);
            return Err(Cause::Deadline);
        }
        Ok(())
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
                        Window::Body => self.receiver_protocol.unwrap_or(self.body).min(self.body),
                        Window::Final => self.end,
                    }
            {
                self.record(Cause::Deadline);
            }
        }
        self.primary.map_or(Ok(()), Err)
    }
    #[cfg(test)]
    pub fn observe_at(&mut self, instant: Instant) {
        self.elapsed = Some(instant.saturating_duration_since(self.start));
    }
    #[cfg(test)]
    pub fn expire_body(&mut self) {
        self.elapsed = Some(self.body.duration_since(self.start));
    }
    #[cfg(test)]
    pub fn after_verifiers(&mut self) {
        match self.late_clock.take() {
            Some(LateClock::Failure) => self.utc_error = true,
            Some(LateClock::Date(date)) => self.today = Some(date),
            None => {}
        }
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

#[cfg(test)]
thread_local! {
    pub(super) static FINISH_SIGNAL: std::cell::Cell<Option<i32>> = const {
        std::cell::Cell::new(None)
    };
    pub(super) static FINISH_MASK: std::cell::Cell<Option<nix::sys::stat::Mode>> = const {
        std::cell::Cell::new(None)
    };
    pub(super) static FINISHED: std::cell::Cell<Option<Result<()>>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
#[path = "control_signal_tests.rs"]
pub(super) mod signal_tests;
