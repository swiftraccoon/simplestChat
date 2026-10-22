//! Bounded setup/readiness CLIs share service-group ownership, including drops.

use super::{Stop, service::Service};
use crate::owned_process::{self, Signals};
use anyhow::Result;
use std::{
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::AsyncReadExt;

const GRACE: Duration = Duration::from_secs(2);

pub(super) struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
}

fn stopped(code: u8, message: &str) -> anyhow::Error {
    Stop {
        code,
        message: message.into(),
    }
    .into()
}

/// Observe harness cancellation without acquiring another process.
pub(super) async fn check(signals: &mut Signals) -> Result<()> {
    if let Some(code) = signals.pending().await {
        return Err(stopped(code, "Owned test setup interrupted."));
    }
    Ok(())
}

/// Cancel only an in-memory wait/socket future. Helper subprocesses use run so
/// dropping a readiness operation cannot abandon a child outside service groups.
pub(super) async fn wait<T>(
    signals: &mut Signals,
    future: impl std::future::Future<Output = T>,
) -> Result<T> {
    tokio::select! {
        biased;
        code = signals.next() => Err(stopped(code, "Owned test setup interrupted.")),
        value = future => Ok(value),
    }
}

/// Preserve configured log/null streams when capture is absent. A captured
/// stdout has its own byte ceiling; caller-configured stderr stays out of RAM.
/// Cleanup calls pass no signals so a repeated interruption cannot skip stops.
pub(super) async fn run(
    command: Command,
    timeout: Duration,
    capture: Option<u64>,
    signals: Option<&mut Signals>,
) -> Result<Output> {
    run_dispatched(command, timeout, capture, signals, &mut false).await
}

/// Record acquisition only after spawn succeeds. A pre-spawn interruption or
/// failed executable admission cannot imply a detached daemon was dispatched.
pub(super) async fn run_dispatched(
    mut command: Command,
    timeout: Duration,
    capture: Option<u64>,
    mut signals: Option<&mut Signals>,
    dispatched: &mut bool,
) -> Result<Output> {
    if let Some(signals) = &mut signals
        && let Some(code) = signals.pending().await
    {
        return Err(stopped(code, "Owned test setup interrupted."));
    }
    command.stdin(Stdio::null());
    if capture.is_some() {
        command.stdout(Stdio::piped());
    }
    let mut service = Service::spawn(command)?;
    *dispatched = true;
    let pid = rustix::process::Pid::from_raw(i32::try_from(service.pid())?)
        .expect("owned service has a positive process identity");
    let mut reader = Reader(if let Some(maximum) = capture {
        let stdout = service
            .take_stdout()
            .ok_or_else(|| stopped(1, "Owned test output unavailable."))?;
        let stdout = tokio::process::ChildStdout::from_std(stdout)?;
        Some(tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout
                .take(
                    maximum
                        .checked_add(1)
                        .ok_or_else(|| stopped(1, "Owned test output bound invalid."))?,
                )
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() as u64 > maximum {
                return Err(stopped(1, "Owned test output exceeded its bound."));
            }
            Ok(bytes)
        }))
    } else {
        None
    });
    let completed = async {
        if let Some(reader) = &mut reader.0 {
            tokio::select! {
                biased;
                bytes = reader => {
                    let bytes = bytes.map_err(|_| stopped(1, "Owned test output failed."))??;
                    owned_process::exited(pid).await?;
                    Ok::<_, anyhow::Error>(Some(bytes))
                },
                result = owned_process::exited(pid) => { result?; Ok(None) },
            }
        } else {
            owned_process::exited(pid).await?;
            Ok(None)
        }
    };
    let interruption = async {
        match &mut signals {
            Some(signals) => signals.next().await,
            None => std::future::pending().await,
        }
    };
    let primary = tokio::select! {
        biased;
        code = interruption => Err(stopped(code, "Owned test setup interrupted.")),
        result = tokio::time::timeout(timeout, completed) => match result {
            Ok(value) => value,
            Err(_) => Err(stopped(124, "Owned test setup timed out.")),
        },
    };
    let ended = service.stop(GRACE).await;
    // Retiring the group releases pipes inherited by an otherwise exited
    // leader's descendants. Never wait for their EOF before this cleanup.
    let stdout = match primary? {
        Some(bytes) => bytes,
        None => match &mut reader.0 {
            Some(reader) => tokio::time::timeout(GRACE, reader)
                .await
                .map_err(|_| stopped(1, "Owned test output completion timed out."))?
                .map_err(|_| stopped(1, "Owned test output failed."))??,
            None => Vec::new(),
        },
    };
    let status = ended
        .wait_status
        .ok_or_else(|| stopped(1, "Owned test setup exit unproven."))?;
    if !ended.group_cleanup_passed {
        return Err(stopped(
            u8::try_from(status)
                .ok()
                .filter(|status| *status != 0)
                .unwrap_or(1),
            "Owned test setup group cleanup unproven.",
        ));
    }
    if status == 0
        && let Some(signals) = &mut signals
        && let Some(code) = signals.pending().await
    {
        return Err(stopped(code, "Owned test setup interrupted."));
    }
    Ok(Output { status, stdout })
}

struct Reader(Option<tokio::task::JoinHandle<Result<Vec<u8>>>>);
impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(reader) = &self.0 {
            reader.abort();
        }
    }
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
