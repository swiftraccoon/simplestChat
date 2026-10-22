//! A collector validates retained receipt/log bytes, not the detached worker PID.

use super::{
    files::Owned,
    plan::{self, Context},
    protocol::{Action, Terminal},
    replay, worker,
};
use crate::{
    ci::release_checks::files::Executable,
    owned_process::Signals,
    tooling::{Failure, Result},
};
use std::time::{Duration, Instant};

pub(super) async fn wait(
    context: &Context,
    signals: &mut Signals,
    deadline: Instant,
) -> Result<()> {
    let owned = Owned::open(context)?;
    let result = collect(context, &owned, signals, deadline, &mut NativeOutput).await;
    let closed = owned.finish();
    result.and(closed)?;
    fence(signals, deadline).await
}
pub(super) trait Output {
    async fn replay(
        &mut self,
        context: &Context,
        bytes: &[u8],
        signals: &mut Signals,
        deadline: Instant,
    ) -> Result<()>;
}
struct NativeOutput;
impl Output for NativeOutput {
    async fn replay(
        &mut self,
        context: &Context,
        bytes: &[u8],
        signals: &mut Signals,
        deadline: Instant,
    ) -> Result<()> {
        replay::replay(context, bytes, signals, deadline).await
    }
}
pub(super) async fn collect(
    context: &Context,
    owned: &Owned,
    signals: &mut Signals,
    deadline: Instant,
    output: &mut impl Output,
) -> Result<()> {
    let (intent, digest) = owned.intent(context)?;
    let terminal = loop {
        let interrupted = signals.pending().await;
        let timed_out = Instant::now() >= deadline - plan::RESERVE;
        if interrupted.is_some() || timed_out {
            let _ = worker::request(owned, &intent, &digest, Action::Cancel);
            // Requesting cancellation does not prove the worker received it.
            while Instant::now() < deadline {
                match owned.spool.json::<Terminal>("terminal.json") {
                    Ok(None) => {}
                    Ok(Some(_)) | Err(_) => break,
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            return Err(Failure::new(
                interrupted.unwrap_or(124),
                "Release controller collection interrupted or timed out.",
            ));
        }
        if let Some(terminal) = owned.spool.json::<Terminal>("terminal.json")? {
            break terminal;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    if terminal.intent_sha256 != digest
        || !terminal.log_closed
        || terminal.log_bytes > plan::MAX_LOG
        || terminal.status_bytes > 16
    {
        return Err(plan::failure());
    }
    // Both bounded files are descriptor-bound, read and explicitly closed before
    // any log byte is emitted. A terminal JSON object cannot authenticate a
    // replaced/partial log. Keep the original raw bytes, not a text reconstruction.
    let log = owned
        .parent
        .read(plan::LOG, plan::MAX_LOG)?
        .ok_or_else(plan::failure)?;
    let status = owned
        .parent
        .read(plan::STATUS, 16)?
        .ok_or_else(plan::failure)?;
    if log.len() as u64 != terminal.log_bytes
        || plan::digest(&log) != terminal.log_sha256
        || status.len() as u64 != terminal.status_bytes
        || plan::digest(&status) != terminal.status_sha256
        || status != format!("{}\n", terminal.installer_status).as_bytes()
    {
        return Err(plan::failure());
    }
    if let Some(code) = signals.pending().await {
        let _ = worker::request(owned, &intent, &digest, Action::Cancel);
        return Err(Failure::new(
            code,
            "Release controller collection interrupted.",
        ));
    }
    output.replay(context, &log, signals, deadline).await?;
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(
            code,
            "Release controller collection interrupted.",
        ));
    }
    if Instant::now() >= deadline {
        return Err(Failure::new(
            124,
            "Release controller collection timed out.",
        ));
    }
    // Match the original shell test: any completed installer failure returns1,
    // although its original status is retained in the private terminal evidence.
    if !terminal.prepared
        || terminal.installer_status != 0
        || terminal.interruption.is_some()
        || !terminal.cli_groups_settled
    {
        return Err(plan::failure());
    }
    let executable = Executable::open(&context.temporary.join(plan::PREFIX).join("bin/python"))?;
    let prefix = worker::prefix(context, &intent);
    let checked = executable.validate().map(|_| ());
    let closed = executable.close();
    prefix.and(checked).and(closed)?;
    fence(signals, deadline).await
}
async fn fence(signals: &mut Signals, deadline: Instant) -> Result<()> {
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(
            code,
            "Release controller collection interrupted.",
        ));
    }
    if Instant::now() >= deadline {
        return Err(Failure::new(
            124,
            "Release controller collection timed out.",
        ));
    }
    Ok(())
}
