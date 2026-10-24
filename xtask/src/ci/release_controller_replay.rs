//! A fixed internal child can block on inherited stdout; its collector cannot.

use super::{
    files,
    plan::{self, Context},
};
use crate::{
    owned_process::{self, Group, Signals},
    tooling::{self, Failure, Result},
};
use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::Instant,
};
use tokio::io::AsyncWriteExt;

pub(super) async fn replay(
    context: &Context,
    bytes: &[u8],
    signals: &mut Signals,
    deadline: Instant,
) -> Result<()> {
    let executable = std::env::current_exe().map_err(|_| plan::failure())?;
    let proof = super::source::Source::open(&executable, plan::MAX_EXECUTABLE_BYTES)?;
    let mut command = tooling::command(
        &context.root,
        &context.environment,
        executable,
        ["ci", "release-controller", "__replay"],
    );
    command.stdout(Stdio::inherit()).stderr(Stdio::null());
    let result = run(command, bytes, signals, deadline).await;
    let closed = proof.finish();
    result.and(closed)
}
pub(in crate::ci) async fn run(
    command: Command,
    bytes: &[u8],
    signals: &mut Signals,
    deadline: Instant,
) -> Result<()> {
    run_with_finalizer(
        command,
        bytes,
        signals,
        deadline,
        &crate::ci::process::Finalize,
    )
    .await
}
pub(in crate::ci) async fn run_with_finalizer(
    mut command: Command,
    bytes: &[u8],
    signals: &mut Signals,
    deadline: Instant,
    finalizer: &impl crate::ci::process::Finalizer,
) -> Result<()> {
    if bytes.len() as u64 > plan::MAX_LOG {
        return Err(plan::failure());
    }
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(code, "Release controller replay interrupted."));
    }
    replay_budget(deadline)?;
    command.process_group(0).stdin(Stdio::piped());
    replay_budget(deadline)?;
    let child = command.spawn().map_err(|_| plan::failure())?;
    let mut group = Group::new(child);
    let pid = group.pid();
    drop(command);
    let mut input = Some(
        tokio::process::ChildStdin::from_std(group.child().stdin.take().ok_or_else(plan::failure)?)
            .map_err(|_| plan::failure())?,
    );
    let completed = async {
        input
            .as_mut()
            .unwrap()
            .write_all(bytes)
            .await
            .map_err(|_| plan::failure())?;
        let owned = input
            .take()
            .unwrap()
            .into_owned_fd()
            .map_err(|_| plan::failure())?;
        files::close(std::fs::File::from(owned))?;
        owned_process::exited(pid).await
    };
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .and_then(|v| v.checked_sub(plan::RESERVE))
        .unwrap_or_default();
    let result = tokio::select! {
        biased;
        code = signals.next() => Err(Failure::new(code, "Release controller replay interrupted.")),
        result = tokio::time::timeout(remaining, completed) => match result {
            Ok(result) => result,
            Err(_) => Err(Failure::new(124, "Release controller replay timed out.")),
        },
    };
    let closed = input
        .take()
        .map(|input| {
            input
                .into_owned_fd()
                .map_err(|_| plan::failure())
                .and_then(|fd| files::close(std::fs::File::from(fd)))
        })
        .transpose();
    if result.is_err() {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        let _ = tokio::time::timeout(plan::GRACE, owned_process::exited(pid)).await;
    }
    let (settled, reaped) = finalizer.finish(&mut group).await;
    result?;
    closed?;
    let status = tooling::shell_status(reaped.map_err(|_| plan::failure())?);
    if settled && status == 0 {
        Ok(())
    } else {
        Err(plan::failure())
    }
}
fn replay_budget(deadline: Instant) -> Result<()> {
    if deadline
        .checked_duration_since(Instant::now())
        .and_then(|value| value.checked_sub(plan::RESERVE))
        .is_none_or(|value| value.is_zero())
    {
        return Err(Failure::new(124, "Release controller replay timed out."));
    }
    Ok(())
}
/// Only this reserved child may block on the inherited runner output pipe.
/// Its parent remains cancellable and owns its final group retirement/reap.
pub(super) fn child() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut bytes = 0_u64;
    let mut buffer = [0; 8192];
    loop {
        let size = input.read(&mut buffer).map_err(|_| plan::failure())?;
        if size == 0 {
            break;
        }
        bytes = bytes.checked_add(size as u64).ok_or_else(plan::failure)?;
        if bytes > plan::MAX_LOG {
            return Err(plan::failure());
        }
        output
            .write_all(&buffer[..size])
            .map_err(|_| plan::failure())?;
    }
    output.flush().map_err(|_| plan::failure())
}
