//! Inherited browser logs and cancellation of the exact owned harness group.
//!
//! The harness gets INT/TERM first and owns normal service cleanup. A NOWAIT
//! leader observation reserves its PID until the final group signal, including
//! when it has already exited and left descendants. Only then may it be reaped.

use super::Invocation;
pub(in crate::ci) use crate::owned_process::{Group, Signals, exited};
use crate::tooling::{self, Failure, Result};
use rustix::process::Signal as ProcessSignal;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

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
    let mut group = Group::new(child);
    let pid = group.pid();
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
