//! One calling-thread signal owner and original Linux monotonic action budget.

use super::{
    Failure, Result,
    protocol::{Action, Mode},
};
#[cfg(target_os = "linux")]
use std::{marker::PhantomData, rc::Rc};

const SECOND: u64 = 1_000_000_000;

pub(super) fn now() -> Result<u64> {
    let value = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let seconds = u64::try_from(value.tv_sec).map_err(|_| Failure::Unproven)?;
    let nanos = u64::try_from(value.tv_nsec).map_err(|_| Failure::Unproven)?;
    if nanos >= SECOND {
        return Err(Failure::Unproven);
    }
    seconds
        .checked_mul(SECOND)
        .and_then(|v| v.checked_add(nanos))
        .ok_or(Failure::Unproven)
}

#[cfg(target_os = "linux")]
struct Signals {
    fd: Option<nix::sys::signalfd::SignalFd>,
    previous: Option<nix::sys::signal::SigSet>,
    _thread: PhantomData<Rc<()>>,
}
#[cfg(target_os = "linux")]
impl Signals {
    fn new() -> Result<Self> {
        use nix::sys::{
            signal::{SigSet, SigmaskHow, Signal, pthread_sigmask},
            signalfd::{SfdFlags, SignalFd},
        };
        let mut selected = SigSet::empty();
        selected.add(Signal::SIGINT);
        selected.add(Signal::SIGTERM);
        let mut previous = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&selected), Some(&mut previous))
            .map_err(|_| Failure::Unproven)?;
        match SignalFd::with_flags(&selected, SfdFlags::SFD_CLOEXEC | SfdFlags::SFD_NONBLOCK) {
            Ok(fd) => Ok(Self {
                fd: Some(fd),
                previous: Some(previous),
                _thread: PhantomData,
            }),
            Err(_) => {
                let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
                Err(Failure::Unproven)
            }
        }
    }

    fn pending(&self) -> Result<()> {
        let mut primary = None;
        for _ in 0..64 {
            match self.fd.as_ref().ok_or(Failure::Unproven)?.read_signal() {
                Ok(None) => return primary.map_or(Ok(()), Err),
                Ok(Some(value)) => {
                    primary.get_or_insert(match value.ssi_signo {
                        2 => Failure::Interrupted,
                        15 => Failure::Terminated,
                        _ => Failure::Unproven,
                    });
                }
                Err(_) => return Err(primary.unwrap_or(Failure::Unproven)),
            }
        }
        Err(primary.unwrap_or(Failure::Unproven))
    }

    fn finish(mut self) -> Result<()> {
        use nix::sys::signal::{SigmaskHow, pthread_sigmask};
        let pending = self.pending();
        let closed = self
            .fd
            .take()
            .map(|fd| {
                let fd: std::os::fd::OwnedFd = fd.into();
                nix::unistd::close(fd).map_err(|_| Failure::Close)
            })
            .unwrap_or(Ok(()));
        let restored = self
            .previous
            .take()
            .ok_or(Failure::Unproven)
            .and_then(|previous| {
                pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None)
                    .map_err(|_| Failure::Unproven)
            });
        pending.and(closed).and(restored)
    }
}
#[cfg(target_os = "linux")]
impl Drop for Signals {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            let _ = nix::sys::signal::pthread_sigmask(
                nix::sys::signal::SigmaskHow::SIG_SETMASK,
                Some(&previous),
                None,
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
struct Signals;
#[cfg(not(target_os = "linux"))]
impl Signals {
    fn new() -> Result<Self> {
        Err(Failure::Platform)
    }
    fn pending(&self) -> Result<()> {
        Ok(())
    }
    fn finish(self) -> Result<()> {
        Ok(())
    }
}

pub(super) struct Owner {
    origin: u64,
    last: u64,
    active: u64,
    end: u64,
    mode: Mode,
    action: Option<Action>,
    signals: Option<Signals>,
    primary: Option<Failure>,
    cleaning: bool,
    #[cfg(test)]
    clock: Option<std::rc::Rc<std::cell::Cell<u64>>>,
    #[cfg(test)]
    pending_control: Option<std::rc::Rc<std::cell::Cell<Option<Failure>>>>,
}

/// Same original clock authority after all owned signal handles have settled.
/// It is private, nonserializable and cannot be rebuilt from a wire receipt.
pub(super) struct Fence {
    origin: u64,
    last: u64,
    end: u64,
    #[cfg(test)]
    clock: Option<std::rc::Rc<std::cell::Cell<u64>>>,
}
impl Fence {
    pub fn check(&mut self) -> Result<()> {
        #[cfg(test)]
        let current = self
            .clock
            .as_ref()
            .map_or_else(now, |clock| Ok(clock.get()))?;
        #[cfg(not(test))]
        let current = now()?;
        if current < self.last || current < self.origin {
            return Err(Failure::Unproven);
        }
        self.last = current;
        if current >= self.end {
            Err(Failure::Deadline)
        } else {
            Ok(())
        }
    }
}

impl Owner {
    #[cfg(test)]
    pub fn fixture(clock: std::rc::Rc<std::cell::Cell<u64>>) -> Self {
        #[cfg(target_os = "linux")]
        let signals = Signals::new().unwrap();
        #[cfg(not(target_os = "linux"))]
        let signals = Signals;
        let origin = clock.get();
        Self {
            origin,
            last: origin,
            active: origin + 60 * SECOND,
            end: origin + 90 * SECOND,
            mode: Mode::Invoke,
            action: None,
            signals: Some(signals),
            primary: None,
            cleaning: false,
            clock: Some(clock),
            pending_control: None,
        }
    }
    fn sample(&self) -> Result<u64> {
        #[cfg(test)]
        if let Some(clock) = &self.clock {
            return Ok(clock.get());
        }
        now()
    }
    #[cfg(test)]
    pub fn fixture_control(&mut self, control: std::rc::Rc<std::cell::Cell<Option<Failure>>>) {
        self.pending_control = Some(control);
    }
    fn pending(&self) -> Result<()> {
        #[cfg(test)]
        if let Some(control) = &self.pending_control
            && let Some(error) = control.take()
        {
            return Err(error);
        }
        self.signals.as_ref().ok_or(Failure::Unproven)?.pending()
    }
    pub fn new(origin: u64, mode: Mode) -> Result<Self> {
        let current = now()?;
        if origin == 0 || origin > current {
            return Err(Failure::Deadline);
        }
        let (body, whole) = if mode == Mode::Stage {
            (300, 360)
        } else {
            (60, 90)
        };
        let mut value = Self {
            origin,
            last: current,
            active: origin.checked_add(body * SECOND).ok_or(Failure::Bounds)?,
            end: origin.checked_add(whole * SECOND).ok_or(Failure::Bounds)?,
            mode,
            action: None,
            signals: Some(Signals::new()?),
            primary: None,
            cleaning: false,
            #[cfg(test)]
            clock: None,
            #[cfg(test)]
            pending_control: None,
        };
        let admitted = value.check(false);
        match admitted {
            Ok(()) => Ok(value),
            Err(error) => {
                let _ = value.finish();
                Err(error)
            }
        }
    }

    /// Domain-selected fixed end; the bootstrap origin never changes at exec.
    pub fn admit(&mut self, action: Action) -> Result<()> {
        self.check(false)?;
        if self.action.is_some() {
            return Err(Failure::Unproven);
        }
        let (body, whole) = match action {
            Action::Reserve => (1740, 1800),
            Action::Archive => (300, 360),
            Action::Recover => (600, 660),
            _ if self.mode == Mode::Stage => (300, 360),
            _ => (60, 90),
        };
        self.active = self
            .origin
            .checked_add(body * SECOND)
            .ok_or(Failure::Bounds)?;
        self.end = self
            .origin
            .checked_add(whole * SECOND)
            .ok_or(Failure::Bounds)?;
        self.action = Some(action);
        self.check(false)
    }

    pub fn check(&mut self, finalizing: bool) -> Result<()> {
        if self.cleaning && !finalizing {
            return self.cleanup_time();
        }
        let signals = self.pending();
        let clock = self.sample();
        let observed = match clock {
            Ok(current) if current >= self.last && current >= self.origin => {
                self.last = current;
                if current >= if finalizing { self.end } else { self.active } {
                    Err(Failure::Deadline)
                } else {
                    Ok(())
                }
            }
            _ => Err(Failure::Unproven),
        };
        if let Err(error) = signals.and(observed) {
            self.primary.get_or_insert(error);
        }
        self.primary.map_or(Ok(()), Err)
    }

    pub fn fixed_end(&self, seconds: u64) -> Result<u64> {
        self.origin
            .checked_add(seconds.checked_mul(SECOND).ok_or(Failure::Bounds)?)
            .ok_or(Failure::Bounds)
    }

    pub fn active_end(&self) -> u64 {
        self.active
    }
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn latch(&mut self, error: Failure) {
        self.primary.get_or_insert(error);
    }
    pub fn failure(&self) -> Option<Failure> {
        self.primary
    }
    pub fn begin_cleanup(&mut self) {
        self.cleaning = true;
    }
    pub fn child_mask(&self) -> Result<nix::sys::signal::SigSet> {
        #[cfg(target_os = "linux")]
        {
            let mut mask = self
                .signals
                .as_ref()
                .and_then(|v| v.previous)
                .ok_or(Failure::Unproven)?;
            mask.remove(nix::sys::signal::Signal::SIGINT);
            mask.remove(nix::sys::signal::Signal::SIGTERM);
            Ok(mask)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(Failure::Platform)
        }
    }
    pub fn ready(&mut self) -> Result<()> {
        self.check(false)?;
        if self.action != Some(Action::Reserve) || self.sample()? >= self.fixed_end(120)? {
            self.latch(Failure::Deadline);
            return Err(Failure::Deadline);
        }
        Ok(())
    }
    pub fn begin_release(&mut self) -> Result<u64> {
        self.check(false)?;
        if self.action != Some(Action::Reserve) {
            return Err(Failure::Unproven);
        }
        let end = self
            .sample()?
            .checked_add(30 * SECOND)
            .ok_or(Failure::Bounds)?
            .min(self.fixed_end(1770)?);
        self.active = end;
        Ok(end)
    }
    pub fn worker(&mut self, runtime: u64) -> Result<()> {
        self.check(false)?;
        if self.action.is_some() || !(120..=21600).contains(&runtime) {
            return Err(Failure::Request);
        }
        self.active = self.fixed_end(runtime.checked_add(600).ok_or(Failure::Bounds)?)?;
        self.end = self.fixed_end(runtime.checked_add(660).ok_or(Failure::Bounds)?)?;
        self.action = Some(Action::Start);
        self.check(false)
    }

    /// Cleanup can observe time/signals without turning a known failure into a pass.
    pub fn cleanup_time(&mut self) -> Result<()> {
        let current = self.sample()?;
        if current < self.last || current < self.origin {
            return Err(Failure::Unproven);
        }
        if current >= self.end {
            return Err(Failure::Deadline);
        }
        self.last = current;
        if let Err(error) = self.pending() {
            self.primary.get_or_insert(error);
        }
        Ok(())
    }

    pub fn finish_fenced(mut self) -> Result<Fence> {
        let observed = self.check(true);
        let signals = self.signals.take().map(Signals::finish).unwrap_or(Ok(()));
        let mut fence = Fence {
            origin: self.origin,
            last: self.last,
            end: self.end,
            #[cfg(test)]
            clock: self.clock.clone(),
        };
        observed?;
        signals?;
        fence.check()?;
        Ok(fence)
    }
    pub fn finish(self) -> Result<()> {
        self.finish_fenced().map(|_| ())
    }
}
