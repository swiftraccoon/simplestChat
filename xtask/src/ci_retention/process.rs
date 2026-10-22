//! Bounded private CLI responses and cancellation of exactly the owned group.
//!
//! Keep the leader unreaped until pipe draining or cleanup finishes; its PID
//! reserves the process-group identity while descendants are terminated.

use crate::tooling::{self, Environment};
use anyhow::{Result, bail};
use rustix::process::{Pid, Signal as ProcessSignal, WaitId, WaitIdOptions};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::signal::unix::{Signal, SignalKind};

#[derive(Debug)]
pub(super) struct Interrupted(pub u8);
impl std::fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache_retention_interrupted")
    }
}
impl std::error::Error for Interrupted {}

pub(super) struct Signals {
    interrupt: Option<Signal>,
    terminate: Option<Signal>,
    injected: Option<tokio::sync::mpsc::UnboundedReceiver<u8>>,
}

impl Signals {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            interrupt: Some(tokio::signal::unix::signal(SignalKind::interrupt())?),
            terminate: Some(tokio::signal::unix::signal(SignalKind::terminate())?),
            injected: None,
        })
    }

    #[cfg(test)]
    pub(super) fn injected(receiver: tokio::sync::mpsc::UnboundedReceiver<u8>) -> Self {
        Self {
            interrupt: None,
            terminate: None,
            injected: Some(receiver),
        }
    }

    async fn next(&mut self) -> u8 {
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
}

pub(super) async fn capture(
    root: &Path,
    environment: &Environment,
    arguments: &[String],
    limit: Duration,
    signals: &mut Signals,
) -> Result<Vec<u8>> {
    let mut command = Command::from(tooling::command(root, environment, "gh", arguments));
    command
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("cache_retention_api"))?;
    let raw = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
        .ok_or_else(|| anyhow::anyhow!("cache_retention_command_identity"))?;
    let pid = Pid::from_raw(raw).expect("positive child pid");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let operation = async {
        tokio::try_join!(
            read_limited(stdout, 4 * 1024 * 1024),
            read_limited(stderr, 65536),
            exited(pid)
        )
    };
    let result = tokio::select! {
        biased;
        code = signals.next() => Err(anyhow::Error::new(Interrupted(code))),
        output = tokio::time::timeout(limit, operation) => match output {
            Ok(output) => output,
            Err(_) => Err(anyhow::anyhow!("cache_retention_deadline")),
        },
    };
    match result {
        Ok((output, _, status)) if status.exit_status() == Some(0) => {
            let status = child.wait().await?;
            if !status.success() {
                bail!("cache_retention_api");
            }
            Ok(output)
        }
        Ok(_) => {
            stop(&mut child, pid).await;
            bail!("cache_retention_api")
        }
        Err(error) => {
            stop(&mut child, pid).await;
            Err(error)
        }
    }
}

async fn read_limited(reader: impl AsyncRead + Unpin, limit: u64) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.take(limit + 1).read_to_end(&mut output).await?;
    if output.len() as u64 > limit {
        bail!("cache_retention_output_limit");
    }
    Ok(output)
}

async fn exited(pid: Pid) -> Result<rustix::process::WaitIdStatus> {
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => bail!("cache_retention_command_wait"),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn stop(child: &mut Child, pid: Pid) {
    let _ = rustix::process::kill_process_group(pid, ProcessSignal::TERM);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(
            rustix::process::waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
            ),
            Ok(Some(_))
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The leader remains unreaped, so this group ID cannot name another run.
    let _ = rustix::process::kill_process_group(pid, ProcessSignal::KILL);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}
