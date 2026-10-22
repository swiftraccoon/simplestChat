//! Reserved process-group ownership shared by CI and the test environment.
//!
//! Standard Child has no background reaper. Observe the leader with NOWAIT,
//! finish the exact reserved group, then disarm the owner before reaping it.

use crate::tooling::{Failure, Result};
use rustix::process::{Pid, Signal as ProcessSignal, WaitId, WaitIdOptions};
use std::time::{Duration, Instant};
use tokio::signal::unix::{Signal, SignalKind};

#[path = "owned_process_cleanup.rs"]
mod cleanup;
pub(crate) use cleanup::finish;
#[cfg(test)]
pub(crate) use cleanup::proof;

pub(crate) struct Signals {
    interrupt: Option<Signal>,
    terminate: Option<Signal>,
    injected: Option<tokio::sync::mpsc::UnboundedReceiver<u8>>,
}

impl Signals {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            interrupt: Some(tokio::signal::unix::signal(SignalKind::interrupt()).map_err(io)?),
            terminate: Some(tokio::signal::unix::signal(SignalKind::terminate()).map_err(io)?),
            injected: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn injected(receiver: tokio::sync::mpsc::UnboundedReceiver<u8>) -> Self {
        Self {
            interrupt: None,
            terminate: None,
            injected: Some(receiver),
        }
    }

    pub(crate) async fn next(&mut self) -> u8 {
        if let Some(receiver) = &mut self.injected {
            return match receiver.recv().await {
                Some(code) => code,
                None => std::future::pending().await,
            };
        }
        tokio::select! {
            _ = self.interrupt.as_mut().expect("registered interrupt").recv() => 130,
            _ = self.terminate.as_mut().expect("registered terminate").recv() => 143,
        }
    }

    pub(crate) async fn pending(&mut self) -> Option<u8> {
        tokio::select! {
            biased;
            code = self.next() => Some(code),
            _ = tokio::task::yield_now() => None,
        }
    }
}

fn io(_: std::io::Error) -> Failure {
    Failure::new(1, "Owned child supervision failed.")
}

pub(crate) async fn exited(pid: Pid) -> Result<()> {
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(Failure::new(1, "Owned child wait failed.")),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(crate) fn reserved(pid: Pid) -> bool {
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
pub(crate) struct Group {
    child: Option<std::process::Child>,
    pid: Pid,
}

impl Group {
    /// The caller must have spawned this standard Child with process_group(0).
    /// Keep that group private until finish; no waiter may reap it first.
    pub(crate) fn new(child: std::process::Child) -> Self {
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(Pid::from_raw)
            .expect("spawned owned child has a positive process identity");
        Self {
            child: Some(child),
            pid,
        }
    }

    pub(crate) fn pid(&self) -> Pid {
        self.pid
    }

    pub(crate) fn child(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("owned unreaped child")
    }

    pub(crate) async fn reap(&mut self) -> Result<std::process::ExitStatus> {
        let mut child = self.child.take().expect("owned unreaped child");
        tokio::task::spawn_blocking(move || child.wait())
            .await
            .map_err(|_| Failure::new(1, "Owned child reap failed."))?
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
