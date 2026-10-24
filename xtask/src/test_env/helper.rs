//! Bounded setup/readiness CLIs share service-group ownership, including drops.

use super::{Stop, service::Service};
use crate::owned_process::{self, Signals};
use anyhow::Result;
use std::{fs::File, os::fd::OwnedFd};
use std::{
    process::{Command, Stdio},
    time::Duration,
};
use tokio::net::unix::pipe::Receiver;

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
    command: Command,
    timeout: Duration,
    capture: Option<u64>,
    signals: Option<&mut Signals>,
    dispatched: &mut bool,
) -> Result<Output> {
    run_owned(
        command,
        timeout,
        capture,
        signals,
        dispatched,
        Reader::close,
    )
    .await
}

async fn run_owned(
    mut command: Command,
    timeout: Duration,
    capture: Option<u64>,
    mut signals: Option<&mut Signals>,
    dispatched: &mut bool,
    close_reader: impl FnOnce(Reader) -> Result<()>,
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
    let mut reader = match capture {
        Some(maximum) => {
            let Some(stdout) = service.take_stdout() else {
                let _ = service.stop(GRACE).await;
                return Err(stopped(1, "Owned test output unavailable."));
            };
            match Reader::new(File::from(OwnedFd::from(stdout)), maximum) {
                Ok(reader) => Some(reader),
                Err(error) => {
                    let _ = service.stop(GRACE).await;
                    return Err(error);
                }
            }
        }
        None => None,
    };
    let completed = async {
        loop {
            if let Some(reader) = &mut reader {
                tokio::select! {
                    biased;
                    result = owned_process::exited(pid) => { result?; return Ok(()); },
                    result = reader.read_once() => {
                        if result? { owned_process::exited(pid).await?; return Ok(()); }
                    },
                }
            } else {
                owned_process::exited(pid).await?;
                return Ok::<_, anyhow::Error>(());
            }
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
    // Normal leader exit does not imply EOF: retire its group before draining
    // inherited pipes. On a known primary failure, only close the bounded pipe.
    let drained = if primary.is_ok() {
        match &mut reader {
            Some(reader) => tokio::time::timeout(GRACE, reader.finish())
                .await
                .map_err(|_| stopped(1, "Owned test output completion timed out."))
                .and_then(|result| result),
            None => Ok(()),
        }
    } else {
        Ok(())
    };
    let stdout = reader
        .as_mut()
        .map_or_else(Vec::new, |reader| std::mem::take(&mut reader.bytes));
    let closed = reader.map(close_reader).transpose();
    primary?;
    let status = ended
        .wait_status
        .ok_or_else(|| stopped(1, "Owned test setup exit unproven."))?;
    if !ended.group_cleanup_passed || drained.is_err() || closed.is_err() {
        if status != 0 {
            return Err(stopped(
                u8::try_from(status).unwrap_or(1),
                "Owned test setup retirement failed.",
            ));
        }
        if !ended.group_cleanup_passed {
            return Err(stopped(1, "Owned test setup group cleanup unproven."));
        }
    }
    drained?;
    closed?;
    if status == 0
        && let Some(signals) = &mut signals
        && let Some(code) = signals.pending().await
    {
        return Err(stopped(code, "Owned test setup interrupted."));
    }
    Ok(Output { status, stdout })
}

/// Inline readiness keeps no spawned task alive after an error return. Each
/// nonblocking read is cancellation safe and its bounded buffer survives polls.
struct Reader {
    file: Receiver,
    bytes: Vec<u8>,
    maximum: usize,
    complete: bool,
}
impl Reader {
    fn new(file: File, maximum: u64) -> Result<Self> {
        let admitted: Result<usize> = (|| {
            let maximum = usize::try_from(maximum)?;
            maximum
                .checked_add(1)
                .ok_or_else(|| stopped(1, "Owned test output bound invalid."))?;
            let flags = rustix::fs::fcntl_getfl(&file)?;
            rustix::fs::fcntl_setfl(&file, flags | rustix::fs::OFlags::NONBLOCK)?;
            Ok(maximum)
        })();
        let maximum = match admitted {
            Ok(maximum) => maximum,
            Err(error) => {
                let _ = nix::unistd::close(file);
                return Err(error);
            }
        };
        // Tokio owns the same pipe FD after this safe conversion. Its error
        // path drops that FD; the failure stays negative without a close claim.
        let file = match Receiver::from_file(file) {
            Ok(file) => file,
            Err(_) => {
                return Err(stopped(1, "Owned test output admission failed."));
            }
        };
        Ok(Self {
            file,
            bytes: Vec::with_capacity(maximum + 1),
            maximum,
            complete: false,
        })
    }
    async fn read_once(&mut self) -> Result<bool> {
        if self.complete {
            return Ok(true);
        }
        let mut buffer = [0; 8192];
        let remaining = (self.maximum + 1 - self.bytes.len()).min(buffer.len());
        loop {
            self.file.readable().await?;
            match self.file.try_read(&mut buffer[..remaining]) {
                Ok(0) => {
                    self.complete = true;
                    return Ok(true);
                }
                Ok(count) => {
                    self.bytes.extend_from_slice(&buffer[..count]);
                    if self.bytes.len() > self.maximum {
                        return Err(stopped(1, "Owned test output exceeded its bound."));
                    }
                    return Ok(false);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(stopped(1, "Owned test output failed.")),
            }
        }
    }
    async fn finish(&mut self) -> Result<()> {
        while !self.read_once().await? {}
        Ok(())
    }
    fn close(self) -> Result<()> {
        self.close_with(|file| nix::unistd::close(file).map_err(anyhow::Error::from))
    }
    fn close_with(self, close: impl FnOnce(File) -> Result<()>) -> Result<()> {
        // On failed deregistration Tokio consumes/drops its FD. Propagate that
        // uncertainty; only a returned owner reaches the explicit close below.
        close(File::from(self.file.into_nonblocking_fd()?))
    }
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
