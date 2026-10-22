//! Owned child processes for the test harness: clean environments, process
//! groups, bounded termination and the status a shell would report.
//!
//! Test commands use the shared unreaped stdChild owner; group signals never
//! depend on PATH or follow release of the leader's identity reservation.

use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;

/// Run one test command in a fresh group. Even an ordinarily exited leader may
/// have descendants; finish that reserved group before reaping the leader.
pub(crate) async fn run_test_child(
    command: std::process::Command,
    grace: Duration,
    signals: &mut crate::owned_process::Signals,
) -> anyhow::Result<i32> {
    run_test_child_with_cleanup(command, grace, signals, crate::owned_process::finish).await
}

async fn run_test_child_with_cleanup<F>(
    mut command: std::process::Command,
    grace: Duration,
    signals: &mut crate::owned_process::Signals,
    finish: impl FnOnce(rustix::process::Pid) -> F,
) -> anyhow::Result<i32>
where
    F: std::future::Future<Output = bool>,
{
    use crate::owned_process::{Group, exited};
    use std::os::unix::process::CommandExt;

    if let Some(code) = signals.pending().await {
        return Ok(i32::from(code));
    }
    command.process_group(0);
    let mut owner = Group::new(command.spawn()?);
    let pid = owner.pid();
    // Standard Command retains its configured file descriptors after spawn.
    drop(command);
    let primary = tokio::select! {
        biased;
        code = signals.next() => {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
            let _ = tokio::time::timeout(grace, exited(pid)).await;
            Ok(Some(i32::from(code)))
        },
        result = exited(pid) => result.map(|()| None),
    };
    let cleaned = finish(pid).await;
    let terminal = tokio::time::timeout(Duration::from_secs(5), exited(pid)).await;
    if !matches!(terminal, Ok(Ok(()))) {
        eprintln!("Owned test command termination could not be confirmed.");
        // Leave the guard armed for its bounded final attempt. Preserve a
        // known interrupt status, but uncertain cleanup can never pass.
        return Ok(primary.ok().flatten().unwrap_or(1));
    }
    let reaped = match owner.reap().await {
        Ok(status) => status,
        Err(error) => {
            return match primary {
                Ok(Some(code)) => Ok(code),
                _ => Err(error.into()),
            };
        }
    };
    let mut status = primary?.unwrap_or_else(|| shell_status(reaped));
    if status == 0
        && let Some(code) = signals.pending().await
    {
        status = i32::from(code);
    }
    if !cleaned {
        eprintln!("Owned test command group cleanup could not be confirmed.");
    }
    Ok(if status == 0 && !cleaned { 1 } else { status })
}

/// The status a shell reports for a finished child: its exit code, or 128 plus
/// the signal that ended it.
pub(crate) fn shell_status(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// Thirty-two random bytes as lowercase hex, from the operating system.
pub(crate) fn random_hex() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
