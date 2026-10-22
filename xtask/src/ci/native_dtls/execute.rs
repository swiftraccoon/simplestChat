//! Fixed phases settle their groups/logs before assertions and namespace cleanup.

use super::{
    files::{Executable, Owned, Tools},
    marker::Marker,
    plan::{self, Context, failure},
    source,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::Result,
};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) trait Clock {
    fn now(&self) -> Instant;
}
struct RealClock;
impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub(super) async fn run(context: &Context, mut signals: Signals) -> Result<()> {
    let deadline = Instant::now() + plan::BUDGET;
    if let Some(code) = signals.pending().await {
        return Err(failure(code));
    }
    let tools = Tools::open(context)?;
    let owned = match Owned::new(context) {
        Ok(owned) => owned,
        Err(error) => {
            let _ = tools.close();
            return Err(error);
        }
    };
    let environment = context.clean_environment(&owned.path);
    let mut runner = Native::new(&context.cwd, &environment, signals);
    execute(context, owned, tools, &mut runner, deadline).await
}
fn remaining(deadline: Instant, clock: &impl Clock) -> Result<Duration> {
    deadline
        .checked_duration_since(clock.now())
        .and_then(|v| v.checked_sub(plan::RESERVE))
        .filter(|v| !v.is_zero())
        .ok_or_else(|| failure(124))
}
fn command(
    owned: &Owned,
    tools: &Tools,
    worker: Option<Arc<Executable>>,
    stage: usize,
    deadline: Instant,
    clock: &impl Clock,
) -> Result<Command> {
    let timeout = remaining(deadline, clock)?;
    let mut command = Command::capture(
        if stage == 2 {
            Program::DtlsWorker
        } else {
            Program::DtlsPython
        },
        &[],
    );
    command.capture = None;
    command.combined_log = Some(Arc::new(owned.log(plan::LOGS[stage])?));
    command.dtls = Some(worker.unwrap_or_else(|| tools.python.clone()));
    let path = |name: &str| owned.path.join(name).into_os_string();
    command.arguments = match stage {
        0 => vec![
            "-m".into(),
            "pip".into(),
            "install".into(),
            "--no-user".into(),
            "--target".into(),
            path("out/pip_invoke"),
            "--require-hashes".into(),
            "--only-binary=:all:".into(),
            "--requirement".into(),
            path("worker/python-invoke-requirements.txt"),
        ],
        1 => vec![
            "-m".into(),
            "invoke".into(),
            "--search-root".into(),
            path("worker"),
            "test".into(),
        ],
        _ => vec!["[dtls-close]".into()],
    };
    command.timeout = timeout;
    Ok(command)
}
pub(super) async fn execute(
    context: &Context,
    owned: Owned,
    tools: Tools,
    runner: &mut impl Executor,
    deadline: Instant,
) -> Result<()> {
    execute_with_clock(context, owned, tools, runner, deadline, &RealClock).await
}
pub(super) async fn execute_with_clock(
    context: &Context,
    owned: Owned,
    tools: Tools,
    runner: &mut impl Executor,
    deadline: Instant,
    clock: &impl Clock,
) -> Result<()> {
    let mut worker = None;
    let mut primary: Result<()> = async {
        runner.ready().await.map_err(|v| failure(v.code))?;
        remaining(deadline, clock)?;
        tools.validate()?;
        owned.prepare(context)?;
        for stage in 0..3 {
            runner.ready().await.map_err(|v| failure(v.code))?;
            remaining(deadline, clock)?;
            tools.validate()?;
            owned.check()?;
            if stage == 2 {
                worker = Some(Arc::new(Executable::worker(&owned)?));
            }
            runner
                .run(command(
                    &owned,
                    &tools,
                    worker.clone(),
                    stage,
                    deadline,
                    clock,
                )?)
                .await
                .map_err(|v| failure(v.code))?;
            tools.validate()?;
            owned.check()?;
        }
        source::assertions(
            &owned.read_log("dtls.log")?,
            &owned.read_log("orderly-close.log")?,
        )?;
        runner.ready().await.map_err(|v| failure(v.code))?;
        if clock.now() >= deadline {
            return Err(failure(124));
        }
        Ok(())
    }
    .await;
    let reads: Vec<_> = plan::LOGS.iter().map(|name| owned.read_log(name)).collect();
    if primary.is_ok() && reads.iter().any(Result::is_err) {
        primary = Err(failure(1));
    }
    // A prior failure may have prevented a later log from being created. A
    // would-be success has already refused every failed final bound read.
    let logs: Vec<_> = reads
        .into_iter()
        .map(|value| value.unwrap_or_default())
        .collect();
    let worker_closed = worker
        .map(|value| {
            Arc::try_unwrap(value)
                .map_err(|_| failure(1))
                .and_then(Executable::close)
        })
        .transpose();
    let tools_closed = tools.close();
    let mut result = primary.and(worker_closed.map(|_| ())).and(tools_closed);
    if result.is_ok() && !runner.cleanup_passed() {
        result = Err(failure(1));
    }
    let path = owned.path.clone();
    match output(&path, &logs, result.is_ok()) {
        Ok(bytes) => {
            let replay = runner
                .replay(&bytes, result.is_err(), deadline)
                .await
                .map_err(|v| failure(v.code));
            result = result.and(replay);
        }
        Err(error) => {
            result = result.and(Err(error));
        }
    }
    if result.is_ok() && !runner.cleanup_passed() {
        result = Err(failure(1));
    }
    let removing_prefix = result.is_ok();
    let (parent, cleanup) = owned.cleanup(removing_prefix);
    let binding = parent.binding();
    result = result.and(cleanup);
    if result.is_ok() {
        result = runner.ready().await.map_err(|v| failure(v.code));
    }
    if result.is_ok() && clock.now() >= deadline {
        result = Err(failure(124));
    }
    let mut marker = None;
    if result.is_ok()
        && let Some(sha) = &context.revision
    {
        let token = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("native-dtls");
        let (owned, published) = Marker::publish(&parent, &context.temporary, sha, token);
        marker = owned;
        result = result.and(published);
    }
    let closed = parent.finish().map_err(|_| failure(1));
    result = result.and(closed);
    if result.is_ok() {
        result = runner.ready().await.map_err(|v| failure(v.code));
    }
    if result.is_ok() && clock.now() >= deadline {
        result = Err(failure(124));
    }
    if result.is_err()
        && let Some(marker) = marker
    {
        let _ = marker.rollback();
    }
    if result.is_err() && removing_prefix {
        let _ = super::files::retain_after_failure(&binding, &context.temporary, &path, &logs);
    }
    result
}
fn output(path: &Path, logs: &[Vec<u8>], success: bool) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    if success {
        output.extend_from_slice(tail(&logs[1], 3));
        output.extend_from_slice(
            concat!(
                "Authenticated orderly DTLS close emitted no warning/error logs.\n",
                "RTP inactivity logs retain stream and media activity context.\n",
                "Bandwidth constraint updates preserve controller state and reach the pacer.\n"
            )
            .as_bytes(),
        );
    } else {
        output.extend_from_slice(
            format!(
                "Native DTLS test failed; logs retained in {}\n",
                path.display()
            )
            .as_bytes(),
        );
        for bytes in logs {
            let tail = tail(bytes, 60);
            if output.len().saturating_add(tail.len()) > plan::LOG_LIMIT as usize {
                return Err(failure(1));
            }
            output.extend_from_slice(tail);
        }
    }
    if output.len() as u64 > plan::LOG_LIMIT {
        return Err(failure(1));
    }
    Ok(output)
}

fn tail(bytes: &[u8], lines: usize) -> &[u8] {
    let mut count = 0;
    for (index, byte) in bytes.iter().enumerate().rev() {
        if *byte == b'\n' && index + 1 != bytes.len() {
            count += 1;
            if count == lines {
                return &bytes[index + 1..];
            }
        }
    }
    bytes
}
#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
