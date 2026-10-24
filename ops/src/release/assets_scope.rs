//! Lexical borrowing of the existing release owner, never another clock.

use super::{Result, fail};
use crate::release::{
    attestation::control::{Cause, Owner, Window},
    workload,
};
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Body,
    Recovery,
    Final,
}

pub(super) struct Scope<'a> {
    owner: Option<RefCell<&'a mut Owner>>,
    mode: Cell<Mode>,
}
/// Restore the lexical phase after retiring known asset transaction remnants.
pub(super) struct Recovery<'a> {
    mode: &'a Cell<Mode>,
    previous: Mode,
}
impl Drop for Recovery<'_> {
    fn drop(&mut self) {
        self.mode.set(self.previous);
    }
}
impl Scope<'_> {
    pub fn standalone() -> Self {
        Self {
            owner: None,
            mode: Cell::new(Mode::Body),
        }
    }
    pub fn check(&self) -> Result<()> {
        if let Some(cell) = &self.owner {
            let mut owner = cell.borrow_mut();
            let result = match self.mode.get() {
                Mode::Body => owner.check(Window::Body),
                Mode::Final => owner.final_work(),
                Mode::Recovery => owner.final_work().and_then(|()| {
                    owner
                        .cleanup_before(owner.body_end())
                        .inspect_err(|&cause| owner.record(cause))
                }),
            };
            result.map_err(|_| fail("asset_operation_failed"))?;
        }
        Ok(())
    }
    /// Only fixed known transaction rollback/remnant cleanup uses this phase.
    /// It retains the first cause and stops at B/signals, never borrowing M.
    pub fn recovery(&self) -> Recovery<'_> {
        let previous = self.mode.get();
        if !matches!(previous, Mode::Final) {
            self.mode.set(Mode::Recovery);
        }
        Recovery {
            mode: &self.mode,
            previous,
        }
    }
    pub fn recovery_if(&self, failed: bool) -> Option<Recovery<'_>> {
        failed.then(|| self.recovery())
    }
    pub fn record(&self, cause: Cause) {
        if let Some(owner) = &self.owner {
            owner.borrow_mut().record(cause);
        }
    }
    /// Check before the physical operation. Callers register newly owned files
    /// or namespace effects before their final positive observation fence.
    pub fn io<T>(&self, work: impl FnOnce() -> std::io::Result<T>) -> Result<T> {
        self.check()?;
        self.observed_io(work())
    }
    pub fn observed_io<T>(&self, value: std::io::Result<T>) -> Result<T> {
        let value = value.map_err(|_| {
            self.record(Cause::Io);
            fail("asset_operation_failed")
        })?;
        self.check()?;
        Ok(value)
    }
    pub fn admit_io<T>(&self, work: impl FnOnce() -> std::io::Result<T>) -> Result<T> {
        self.check()?;
        work().map_err(|_| {
            self.record(Cause::Io);
            fail("asset_operation_failed")
        })
    }
    pub fn sys<T>(&self, work: impl FnOnce() -> rustix::io::Result<T>) -> Result<T> {
        self.admit_io(|| work().map_err(std::io::Error::from))
    }
    pub fn snapshot(
        &self,
        event: crate::files::SnapshotEvent,
        observation: std::result::Result<(), crate::Error>,
    ) -> std::result::Result<(), crate::Error> {
        match observation {
            Err(error) => {
                let cause = if matches!(error, crate::Error::Changed) {
                    Cause::Changed
                } else {
                    match event {
                        crate::files::SnapshotEvent::Close => Cause::Close,
                        crate::files::SnapshotEvent::Check => Cause::Policy,
                        _ => Cause::Io,
                    }
                };
                self.record(cause);
                Err(error)
            }
            Ok(()) => self.check().map_err(|_| crate::Error::Input),
        }
    }
    pub fn lease(&self, value: std::result::Result<(), workload::Failure>) -> Result<()> {
        value.map_err(|error| {
            self.record(match error {
                workload::Failure::Io => Cause::Io,
                workload::Failure::Changed => Cause::Changed,
                workload::Failure::Close => Cause::Close,
                workload::Failure::Busy => Cause::Policy,
            });
            fail("asset_operation_failed")
        })
    }
    /// Latch a policy refusal before attempting a later checked close/rollback.
    pub fn observed<T>(&self, value: &Result<T>) {
        if value.is_err() {
            self.record(Cause::Policy);
        }
    }
    /// Close always runs, including after B/N; its result is never drop proof.
    pub fn closed(&self, value: Result<()>) -> Result<()> {
        if value.is_err() {
            self.record(Cause::Close);
        }
        value
    }
    pub fn result<T>(&self, value: Result<T>) -> Result<T> {
        self.observed(&value);
        value.and_then(|value| {
            self.check()?;
            Ok(value)
        })
    }
}
impl<'a> Scope<'a> {
    pub fn owned(owner: &'a mut Owner, mode: Mode) -> Self {
        Self {
            owner: Some(RefCell::new(owner)),
            mode: Cell::new(mode),
        }
    }
}

#[cfg(test)]
#[path = "assets_scope_tests.rs"]
mod tests;
