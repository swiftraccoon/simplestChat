//! Portable Unix ownership of one reserved child group and INT/TERM registration.
//!
//! Callers establish `process_group(0)` and exclusive wait ownership before
//! transferring a standard child here. Keep its leader unreaped until every
//! exact-group signal is complete. A destructor is a fallback, never a positive
//! cleanup result. This module does not select a tool, deadline or command.
//!
//! This Linux/macOS example assumes an entered Tokio runtime with signal and
//! time drivers enabled. It is compiled, not executed, by documentation tests.
//! The cooperative deadline includes preparation; blocked kernel/reap latency
//! is outside a hard wall bound. Metadata and cleanup observations authenticate
//! neither an executable nor a service.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use simplestchat_ops::owned_process::{Group, Signals, exited, finish_before, reserved};
//! use std::{io, os::unix::process::CommandExt, process::{Command, Stdio}};
//! use std::time::{Duration, Instant};
//!
//! let end = Instant::now() + Duration::from_secs(30);
//! let body_end = end - Duration::from_secs(5); // Retain cleanup inside the same interval.
//! let mut signals = Signals::new()?;
//! if Instant::now() >= body_end {
//!     return Err(io::Error::other("operation expired before spawn").into());
//! }
//! let child = Command::new("/usr/bin/true")
//!     .process_group(0)
//!     .stdin(Stdio::null())
//!     .stdout(Stdio::null())
//!     .stderr(Stdio::null())
//!     .spawn()?;
//! let mut group = Group::new(child);
//! let pid = group.pid();
//! let observed: Result<(), Box<dyn std::error::Error>> = async {
//!     if !reserved(pid) { // NOWAIT: no consuming wait releases the identity.
//!         return Err(io::Error::other("child reservation unproven").into());
//!     }
//!     let body_deadline = tokio::time::Instant::from_std(body_end);
//!     tokio::select! {
//!         result = tokio::time::timeout_at(body_deadline, exited(pid)) => {
//!             result??;
//!             Ok(())
//!         },
//!         _ = signals.next() => Err(io::Error::other("operation interrupted").into()),
//!     }
//! }.await;
//! // Every admitted peer is attempted before propagating the first error.
//! // Complete group signals BEFORE consuming its reserved leader's reap.
//! let retired = finish_before(pid, end).await;
//! let reaped = group.reap().await;
//! let interrupted = signals.pending().await;
//! observed?;
//! if !retired {
//!     return Err(io::Error::other("group retirement unproven").into());
//! }
//! let status = reaped?;
//! if !status.success() {
//!     return Err(io::Error::other("child command failed").into());
//! }
//! if interrupted.is_some() || Instant::now() >= end {
//!     return Err(io::Error::other("operation interrupted or expired").into());
//! }
//! # Ok(())
//! # }
//! ```

use rustix::process::{Pid, Signal as ProcessSignal, WaitId, WaitIdOptions};
use std::time::{Duration, Instant};
use tokio::signal::unix::{Signal, SignalKind};

#[path = "owned_process_cleanup.rs"]
mod cleanup;
pub use cleanup::{finish, finish_before};

/// Fixed, privacy-safe failures of the concrete child owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// Signal registration or a child-system operation failed.
    Supervision,
    /// The retained leader could not be observed without reaping.
    Wait,
    /// The exclusive consuming reap failed.
    Reap,
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Supervision => "owned child supervision failed",
            Self::Wait => "owned child wait failed",
            Self::Reap => "owned child reap failed",
        })
    }
}
impl std::error::Error for Error {}

/// One native INT/TERM registration retained by the whole calling operation.
/// Moving this owner does not register another handler or clear pending signals.
pub struct Signals {
    interrupt: Signal,
    terminate: Signal,
}
impl std::fmt::Debug for Signals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Both fields are registered by construction. Never inspect or poll the
        // receivers merely to render diagnostic state.
        formatter
            .debug_struct("Signals")
            .field("interrupt_registered", &true)
            .field("terminate_registered", &true)
            .finish()
    }
}
impl Signals {
    /// Register on the caller's existing Tokio Unix runtime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Supervision`] if INT or TERM registration fails.
    ///
    /// # Panics
    ///
    /// Panics without an entered Tokio runtime with its signal driver enabled.
    pub fn new() -> std::result::Result<Self, Error> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(SignalKind::interrupt()).map_err(io)?,
            terminate: tokio::signal::unix::signal(SignalKind::terminate()).map_err(io)?,
        })
    }
    /// Observe the next original owner signal as its conventional shell code.
    pub async fn next(&mut self) -> u8 {
        tokio::select! {
            _ = self.interrupt.recv() => 130,
            _ = self.terminate.recv() => 143,
        }
    }
    /// Synchronously poll the same registered INT/TERM streams.
    /// The calling runtime must keep its signal driver running during long
    /// synchronous checkpoints; this does not create a second registration.
    pub fn pending_now(&mut self) -> Option<u8> {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        if self.interrupt.poll_recv(&mut context).is_ready() {
            Some(130)
        } else if self.terminate.poll_recv(&mut context).is_ready() {
            Some(143)
        } else {
            None
        }
    }
    /// Poll pending owner signals without replacing this registration.
    pub async fn pending(&mut self) -> Option<u8> {
        tokio::select! {
            biased;
            code = self.next() => Some(code),
            _ = tokio::task::yield_now() => None,
        }
    }
}
fn io(_: std::io::Error) -> Error {
    Error::Supervision
}

/// Wait for terminal observation while retaining the child/group reservation.
///
/// # Errors
///
/// Returns [`Error::Wait`] if the exclusive unreaped child cannot be observed.
///
/// # Panics
///
/// Panics if waiting needs a Tokio timer and no enabled timer driver is entered.
pub async fn exited(pid: Pid) -> std::result::Result<(), Error> {
    loop {
        if exited_now(pid)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Observe a retained child without releasing its PID or group reservation.
///
/// # Errors
///
/// Returns [`Error::Wait`] if the exclusive unreaped child cannot be observed.
pub fn exited_now(pid: Pid) -> std::result::Result<bool, Error> {
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(value) => return Ok(value.is_some()),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(Error::Wait),
        }
    }
}

/// Return whether this caller still holds an unreaped child reservation.
pub fn reserved(pid: Pid) -> bool {
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(_) => return true,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return false,
        }
    }
}

/// Standard Child has no background reaper. Disarm before waiting so a late
/// destructor can never signal a process-group ID after its reservation ends.
pub struct Group {
    child: Option<std::process::Child>,
    pid: Pid,
}
impl std::fmt::Debug for Group {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Group")
            .field("pid", &self.pid.as_raw_nonzero().get())
            .field("reap_pending", &self.child.is_some())
            .finish()
    }
}

impl Group {
    /// The caller must have spawned this standard Child with process_group(0).
    /// Keep that group private until finish; no waiter may reap it first.
    ///
    /// # Panics
    ///
    /// Panics if the standard child's ID is not a positive supported Unix PID.
    /// This constructor does not validate the caller's exclusive-wait or group
    /// setup preconditions and performs no signal or wait itself.
    pub fn new(child: std::process::Child) -> Self {
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(Pid::from_raw)
            .expect("spawned owned child has a positive process identity");
        Self {
            child: Some(child),
            pid,
        }
    }

    /// The retained positive leader/group identity.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Borrow the exclusive child for pipe transfer, never for another wait.
    ///
    /// # Panics
    ///
    /// Panics if this owner was already disarmed for its consuming reap.
    pub fn child(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("owned unreaped child")
    }

    /// Consume the child only after final group signals and terminal observation.
    /// The caller must await this reap to completion; Drop is not completion.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Reap`] if the blocking wait task cannot complete, or
    /// [`Error::Supervision`] if the standard child's consuming wait fails.
    ///
    /// # Panics
    ///
    /// Panics if this owner was already disarmed, or if polled outside an entered
    /// Tokio runtime. Kernel/reap latency is outside a hard wall bound.
    pub async fn reap(&mut self) -> std::result::Result<std::process::ExitStatus, Error> {
        let mut child = self.child.take().expect("owned unreaped child");
        tokio::task::spawn_blocking(move || child.wait())
            .await
            .map_err(|_| Error::Reap)?
            .map_err(io)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if reserved(self.pid) {
            let _ = rustix::process::kill_process_group(self.pid, ProcessSignal::KILL);
        }
        // No group signal follows try_wait, which can release the reservation.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
#[path = "owned_process_tests.rs"]
mod tests;
