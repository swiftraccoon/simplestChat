//! Inherited browser logs and cancellation of the exact owned harness group.
//!
//! The harness gets INT/TERM first and owns normal service cleanup. A NOWAIT
//! leader observation reserves its PID until the final group signal, including
//! when it has already exited and left descendants. Only then may it be reaped.

use super::Invocation;
use crate::tooling::{self, Failure, Result};
use rustix::process::{Pid, Signal as ProcessSignal, WaitId, WaitIdOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::signal::unix::{Signal, SignalKind};

pub(in crate::ci) struct Signals {
    interrupt: Option<Signal>,
    terminate: Option<Signal>,
    injected: Option<tokio::sync::mpsc::UnboundedReceiver<u8>>,
}

impl Signals {
    pub(in crate::ci) fn new() -> Result<Self> {
        Ok(Self {
            interrupt: Some(tokio::signal::unix::signal(SignalKind::interrupt()).map_err(io)?),
            terminate: Some(tokio::signal::unix::signal(SignalKind::terminate()).map_err(io)?),
            injected: None,
        })
    }

    #[cfg(test)]
    pub(in crate::ci) fn injected(receiver: tokio::sync::mpsc::UnboundedReceiver<u8>) -> Self {
        Self {
            interrupt: None,
            terminate: None,
            injected: Some(receiver),
        }
    }

    pub(in crate::ci) async fn next(&mut self) -> u8 {
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

    pub(in crate::ci) async fn pending(&mut self) -> Option<u8> {
        tokio::select! {
            biased;
            code = self.next() => Some(code),
            _ = tokio::task::yield_now() => None,
        }
    }
}

fn io(_: std::io::Error) -> Failure {
    Failure::new(1, "Browser harness supervision failed.")
}

pub(in crate::ci) async fn exited(pid: Pid) -> Result<()> {
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(Failure::new(1, "Browser harness wait failed.")),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) fn reserved(pid: Pid) -> bool {
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
pub(in crate::ci) struct Group {
    child: Option<std::process::Child>,
    pid: Pid,
}

impl Group {
    pub(in crate::ci) fn new(child: std::process::Child) -> Self {
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(Pid::from_raw)
            .expect("spawned owned child has a positive process identity");
        Self {
            child: Some(child),
            pid,
        }
    }

    pub(in crate::ci) fn pid(&self) -> Pid {
        self.pid
    }

    pub(in crate::ci) fn child(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("owned unreaped child")
    }

    pub(in crate::ci) async fn reap(&mut self) -> Result<std::process::ExitStatus> {
        let mut child = self.child.take().expect("owned unreaped child");
        tokio::task::spawn_blocking(move || child.wait())
            .await
            .map_err(|_| Failure::new(1, "Browser harness reap failed."))?
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

pub(super) async fn run(
    root: &Path,
    invocation: &Invocation,
    signals: &mut Signals,
    cleanup_grace: Duration,
) -> Result<()> {
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(code, "Browser suite interrupted."));
    }
    let mut command = tooling::command(
        root,
        &invocation.environment,
        &invocation.executable,
        &invocation.arguments,
    );
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = command.spawn().map_err(|error| {
        Failure::new(
            match error.kind() {
                std::io::ErrorKind::NotFound => 127,
                std::io::ErrorKind::PermissionDenied => 126,
                _ => 1,
            },
            "Cannot start the owned browser test harness.",
        )
    })?;
    let pid = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .expect("spawned owned child has a positive process identity");
    let mut group = Group {
        child: Some(child),
        pid,
    };
    drop(command);
    let result = tokio::select! {
        biased;
        code = signals.next() => {
            let signal = if code == 130 { ProcessSignal::INT } else { ProcessSignal::TERM };
            let _ = rustix::process::kill_process(pid, signal);
            // Do not re-signal on a second interruption: the harness must have
            // its normal cleanup interval before this owner escalates.
            let _ = tokio::time::timeout(cleanup_grace, exited(pid)).await;
            Err(Failure::new(code, "Browser suite interrupted."))
        },
        result = exited(pid) => result,
    };
    // The unreaped leader reserves the process-group identity, so this cannot
    // target a later process even if only a stray descendant remains alive.
    let cleaned = super::cleanup::finish(pid).await;
    // No group signals can follow this reap, including on an error path.
    let status = group.reap().await;
    result?;
    let code = tooling::shell_status(status?);
    if code == 0 && cleaned {
        Ok(())
    } else if code == 0 {
        Err(Failure::new(1, "Browser harness group cleanup failed."))
    } else {
        Err(Failure::new(
            code,
            format!("Browser harness failed (status {code})."),
        ))
    }
}
