//! Fresh service groups retain their leader identity until final group cleanup.
//!
//! The harness registers cancellation before acquiring services and owns calling
//! `stop` on every acquired service. Dropping a service or its stopping future
//! leaves the shared guard armed. This covers descendants remaining in the
//! fresh service group, not children that deliberately create another session
//! or privileged process group.

use crate::owned_process::{self, Group};
use anyhow::Result;
use rustix::process::{Pid, Signal};
use std::os::unix::process::CommandExt;
use std::process::{ChildStdout, Command};
use std::time::Duration;

const TERMINAL_BOUND: Duration = Duration::from_secs(5);

/// Leader shutdown and final group cleanup are distinct evidence. An ordinary
/// leader exit can still leave descendants, so `wait_status == Some(0)` alone
/// never proves successful service retirement.
#[derive(Debug, Default)]
pub(crate) struct Termination {
    pub term_attempted: bool,
    pub term_sent: bool,
    pub kill_attempted: bool,
    pub kill_sent: bool,
    pub wait_status: Option<i32>,
    pub group_cleanup_passed: bool,
}

/// Exactly one newly spawned standard Child, in a fresh private process group.
/// Only NOWAIT observations occur before the final group signal; no background
/// reaper can release this leader's identity while cleanup still needs it.
pub(crate) struct Service {
    group: Group,
}

impl Service {
    pub(crate) fn spawn(mut command: Command) -> Result<Self> {
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|_| anyhow::anyhow!("Cannot start the owned test service."))?;
        // A standard Command retains configured descriptors after spawning.
        drop(command);
        Ok(Self {
            group: Group::new(child),
        })
    }

    /// The retained leader identity, suitable for this invocation's receipts.
    pub(crate) fn pid(&self) -> u32 {
        self.group.pid().as_raw_nonzero().get().unsigned_abs()
    }

    /// Observe termination without reaping or releasing the group reservation.
    pub(crate) fn exited(&self) -> Result<bool> {
        Ok(owned_process::exited_now(self.group.pid())?)
    }

    /// Transfer a configured stdout pipe to a bounded reader. The caller owns
    /// cancelling that reader; this service still owns the process group.
    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.group.child().stdout.take()
    }

    /// Give a live leader TERM and its grace interval, then escalate the leader
    /// if needed. Always finish the reserved group, including after natural
    /// completion, and confirm termination before disarming the guard to reap.
    /// A failed observation, cleanup or reap leaves positive cleanup unproven.
    pub(crate) async fn stop(self, grace: Duration) -> Termination {
        self.stop_with_cleanup(grace, owned_process::finish).await
    }

    async fn stop_with_cleanup<F>(
        mut self,
        grace: Duration,
        finish: impl FnOnce(Pid) -> F,
    ) -> Termination
    where
        F: std::future::Future<Output = bool>,
    {
        let pid = self.group.pid();
        let mut record = Termination::default();
        let mut observation_failed = false;
        match self.exited() {
            Ok(false) => {
                record.term_attempted = true;
                record.term_sent = signal_leader(pid, Signal::TERM);
                let ended = tokio::time::timeout(grace, owned_process::exited(pid)).await;
                if !matches!(ended, Ok(Ok(()))) {
                    observation_failed = matches!(ended, Ok(Err(_)));
                    record.kill_attempted = true;
                    record.kill_sent = signal_leader(pid, Signal::KILL);
                }
            }
            Ok(true) => {}
            Err(_) => observation_failed = true,
        }
        // This is not leader escalation: it retires remaining group members
        // even when the leader exited zero before stop was called.
        record.group_cleanup_passed = finish(pid).await && !observation_failed;
        let terminal = tokio::time::timeout(TERMINAL_BOUND, owned_process::exited(pid)).await;
        if !matches!(terminal, Ok(Ok(()))) {
            record.group_cleanup_passed = false;
            // Do not disarm on uncertain termination. Group::drop makes its
            // bounded final attempt while the leader identity is still held.
            return record;
        }
        match self.group.reap().await {
            Ok(status) => record.wait_status = Some(super::process::shell_status(status)),
            Err(_) => record.group_cleanup_passed = false,
        }
        record
    }
}

fn signal_leader(pid: Pid, signal: Signal) -> bool {
    owned_process::reserved(pid) && rustix::process::kill_process(pid, signal).is_ok()
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
