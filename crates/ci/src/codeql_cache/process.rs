//! Private bounded analyzer/package commands; only argv is accepted, never shell text.

use super::require;
use anyhow::Result;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(super) fn capture(
    root: &Path,
    environment: &BTreeMap<String, String>,
    program: &str,
    arguments: &[String],
    timeout: u64,
    stdout_limit: u64,
    stderr_limit: u64,
) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(arguments)
            .current_dir(root)
            .env_clear()
            .envs(environment)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 1)
            .and_then(Pid::from_raw)
            .ok_or_else(|| anyhow::anyhow!("codeql_cache_command_identity"))?;
        let output = child.stdout.take().expect("piped stdout");
        let errors = child.stderr.take().expect("piped stderr");
        let work = async {
            let (status, output, _) = tokio::try_join!(
                exited(pid),
                read(output, stdout_limit),
                read(errors, stderr_limit)
            )?;
            require(status.exit_status() == Some(0), "codeql_cache_environment")?;
            Ok(output)
        };
        let result = tokio::select! {
            biased;
            _ = interrupt.recv() => Err(anyhow::anyhow!("codeql_cache_interrupted")),
            _ = terminate.recv() => Err(anyhow::anyhow!("codeql_cache_interrupted")),
            result = tokio::time::timeout(Duration::from_secs(timeout),work) => match result {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("codeql_cache_command_timeout")),
            },
        };
        if result.is_err() {
            // NOWAIT reserves the leader's PID throughout pipe drain and cleanup.
            let _ = rustix::process::kill_process_group(pid, Signal::KILL);
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        } else {
            child.wait().await?;
        }
        result
    })
}

async fn read(reader: impl tokio::io::AsyncRead + Unpin, maximum: u64) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.take(maximum + 1).read_to_end(&mut output).await?;
    require(
        output.len() as u64 <= maximum,
        "codeql_cache_command_output",
    )?;
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
            Err(_) => anyhow::bail!("codeql_cache_command_wait"),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
