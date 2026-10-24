//! Single calling-thread signal mask and monotonic admission. Safe nix owns the
//! Linux signalfd ABI. No handler, hand FFI, child, pump or detached worker exists.

use super::{Failure, Result};
use std::time::{Duration, Instant};

pub(super) const ENTRY: Duration = Duration::from_secs(120);
const TAIL: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Body,
    Finalize,
}

#[cfg(target_os = "linux")]
struct Signals {
    file: Option<nix::sys::signalfd::SignalFd>,
    previous: Option<nix::sys::signal::SigSet>,
    // Mask restoration belongs to the same calling thread; prevent an internal
    // future refactor from sending this guard to a worker just because its FD
    // alone implements Send/Sync.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
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
        let file = SignalFd::with_flags(&selected, SfdFlags::SFD_CLOEXEC | SfdFlags::SFD_NONBLOCK);
        match file {
            Ok(file) => Ok(Self {
                file: Some(file),
                previous: Some(previous),
                _thread: std::marker::PhantomData,
            }),
            Err(_) => {
                let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
                Err(Failure::Unproven)
            }
        }
    }
    fn pending(&self) -> Result<()> {
        let mut first = None;
        for _ in 0..64 {
            match self.file.as_ref().ok_or(Failure::Unproven)?.read_signal() {
                Ok(None) => return first.map_or(Ok(()), Err),
                Ok(Some(value)) => {
                    let failure = match value.ssi_signo {
                        2 => Failure::Interrupted,
                        15 => Failure::Terminated,
                        _ => Failure::Unproven,
                    };
                    first.get_or_insert(failure);
                }
                Err(_) => return Err(first.unwrap_or(Failure::Unproven)),
            }
        }
        Err(first.unwrap_or(Failure::Unproven))
    }
    fn finish(mut self) -> Result<()> {
        use nix::sys::signal::{SigmaskHow, pthread_sigmask};
        let pending = self.pending();
        let closed = self
            .file
            .take()
            .map(|value| {
                let fd: std::os::fd::OwnedFd = value.into();
                nix::unistd::close(fd).map_err(|_| Failure::Unproven)
            })
            .unwrap_or(Ok(()));
        let previous = self.previous.take().ok_or(Failure::Unproven)?;
        let restored = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None)
            .map_err(|_| Failure::Unproven);
        pending.and(closed).and(restored)
    }
}
#[cfg(target_os = "linux")]
impl Drop for Signals {
    fn drop(&mut self) {
        // Emergency unwinding is not a successful finalization receipt.
        if let Some(previous) = &self.previous {
            let _ = nix::sys::signal::pthread_sigmask(
                nix::sys::signal::SigmaskHow::SIG_SETMASK,
                Some(previous),
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
        Err(Failure::Unproven)
    }
    fn pending(&self) -> Result<()> {
        Ok(())
    }
    fn finish(self) -> Result<()> {
        Ok(())
    }
}

pub(super) struct Owner {
    signals: Option<Signals>,
    start: Instant,
    last: Instant,
    first: Option<Failure>,
    #[cfg(test)]
    pub now: Option<std::rc::Rc<dyn Fn() -> Instant>>,
    #[cfg(test)]
    pub pending: Option<std::rc::Rc<dyn Fn() -> Result<()>>>,
}
impl Owner {
    pub fn native() -> Result<Self> {
        let start = Instant::now();
        Ok(Self {
            signals: Some(Signals::new()?),
            start,
            last: start,
            first: None,
            #[cfg(test)]
            now: None,
            #[cfg(test)]
            pending: None,
        })
    }
    pub fn check(&mut self, phase: Phase) -> Result<()> {
        let pending = self
            .signals
            .as_ref()
            .map(Signals::pending)
            .unwrap_or(Ok(()));
        #[cfg(test)]
        let pending = pending.and(self.pending.as_ref().map(|f| f()).unwrap_or(Ok(())));
        if let Err(error) = pending {
            self.first.get_or_insert(error);
        }
        let now = Instant::now();
        #[cfg(test)]
        let now = self.now.as_ref().map(|f| f()).unwrap_or(now);
        let span = match phase {
            Phase::Body => ENTRY - TAIL,
            Phase::Finalize => ENTRY,
        };
        if now < self.last || now < self.start {
            self.first.get_or_insert(Failure::Unproven);
        } else if now.duration_since(self.start) >= span {
            self.first.get_or_insert(Failure::Deadline);
        }
        self.last = now;
        self.first.map_or(Ok(()), Err)
    }
    pub fn finish(mut self) -> Result<()> {
        let checked = self.check(Phase::Finalize);
        let settled = self.signals.take().map(Signals::finish).unwrap_or(Ok(()));
        checked.and(settled)
    }
    #[cfg(test)]
    pub fn fixture() -> Self {
        let start = Instant::now();
        Self {
            signals: None,
            start,
            last: start,
            first: None,
            now: None,
            pending: None,
        }
    }
    #[cfg(test)]
    pub fn start(&self) -> Instant {
        self.start
    }
}
