//! Bound both subprocess streams and retain the owned group leader until cleanup.

use self::group::Group;
pub(crate) use self::io::Destination;
use self::io::read_limited;
#[cfg(feature = "full")]
pub(crate) mod child;
#[cfg(all(test, feature = "full"))]
mod child_tests;
mod cleanup;
mod group;
pub(crate) mod io;
#[cfg(test)]
pub(crate) use cleanup::zombie_rows;

use anyhow::Result;
use nix::unistd::Pid;
use rustix::process::{WaitId, WaitIdOptions, waitid};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Default)]
pub(crate) struct Cancellation(Arc<AtomicU8>);

impl Cancellation {
    pub(crate) fn cancel(&self, code: u8) {
        let _ = self
            .0
            .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
    }
    pub(crate) fn code(&self) -> u8 {
        self.0.load(Ordering::SeqCst)
    }
    pub(crate) async fn cancelled(&self) -> u8 {
        loop {
            let code = self.code();
            if code != 0 {
                return code;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[derive(Debug)]
pub(crate) struct Interrupted(pub(crate) u8);
impl std::fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("capacity interrupted")
    }
}
impl std::error::Error for Interrupted {}

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) timeout: Duration,
    pub(crate) stdout: u64,
    pub(crate) stderr: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            stdout: 2 * 1024 * 1024,
            stderr: 2 * 1024 * 1024,
        }
    }
}

pub(crate) struct Request {
    pub(crate) limits: Limits,
    pub(crate) input: Vec<u8>,
    pub(crate) stdout: Destination,
    pub(crate) stderr: Destination,
    /// Only verified owned-resource cleanup may continue after cancellation.
    pub(crate) cleanup: bool,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            input: Vec::new(),
            stdout: Destination::capture(),
            stderr: Destination::capture(),
            cleanup: false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) status: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct Runner {
    pub(crate) directory: PathBuf,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    pub(crate) cancellation: Cancellation,
}

impl Runner {
    pub(crate) async fn run(
        &self,
        executable: &Path,
        arguments: &[String],
        request: Request,
    ) -> Result<Output> {
        if !request.cleanup && self.cancellation.code() != 0 {
            return Err(Interrupted(self.cancellation.code()).into());
        }
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .current_dir(&self.directory)
            .env_clear()
            .envs(&self.environment)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn()?;
        let raw = Some(child.id())
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 1)
            .ok_or_else(|| anyhow::anyhow!("command_identity_unavailable"))?;
        let pid = Pid::from_raw(raw);
        let mut group = Group::new(child, pid);
        let stdin = group.child().stdin.take().expect("piped stdin");
        let stdout = group.child().stdout.take().expect("piped stdout");
        let stderr = group.child().stderr.take().expect("piped stderr");
        let work = async {
            let mut stdin = tokio::process::ChildStdin::from_std(stdin)?;
            let stdout = tokio::process::ChildStdout::from_std(stdout)?;
            let stderr = tokio::process::ChildStderr::from_std(stderr)?;
            let (_, output, errors, status) = tokio::try_join!(
                async {
                    stdin.write_all(&request.input).await?;
                    drop(stdin);
                    Ok::<_, anyhow::Error>(())
                },
                read_limited(stdout, request.stdout, request.limits.stdout, "stdout"),
                read_limited(stderr, request.stderr, request.limits.stderr, "stderr"),
                exited(pid),
            )?;
            Ok::<_, anyhow::Error>((output, errors, status))
        };
        let result = tokio::select! {
            biased;
            code = self.cancellation.cancelled(), if !request.cleanup => {
                Err(Interrupted(code).into())
            },
            result = tokio::time::timeout(request.limits.timeout, work) => match result {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("command_timed_out")),
            },
        };
        // Even a successful leader can leave descendants after closing its
        // pipes. Reclaim the exact reserved group before releasing that PID.
        let cleanup = self::cleanup::stop(&mut group).await;
        let (stdout, stderr, status) = match (result, cleanup) {
            (Ok(output), Ok(())) => output,
            (Err(error), Ok(())) => return Err(error),
            (result, Err(error)) => {
                return Err(error.context(result.err().map_or_else(
                    || "owned command cleanup failed".into(),
                    |original| format!("{original}; owned command cleanup failed"),
                )));
            }
        };
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

pub(crate) fn peek(pid: Pid) -> Result<Option<i32>> {
    let pid = rustix::process::Pid::from_raw(pid.as_raw())
        .ok_or_else(|| anyhow::anyhow!("command_identity_unavailable"))?;
    let value = waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )?;
    Ok(value.and_then(|status| {
        status
            .exit_status()
            .or_else(|| status.terminating_signal().map(|signal| 128 + signal))
    }))
}

pub(crate) async fn exited(pid: Pid) -> Result<i32> {
    loop {
        match peek(pid) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error)
                if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
