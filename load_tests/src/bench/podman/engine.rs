//! Bounded structured engine calls, backed by the shared owned process host.

use crate::bench::{
    clock::Clock,
    process::{Child, Command, Exit, Host, Sink},
};
use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub(super) const MIB: u64 = 1024 * 1024;
#[derive(Clone)]
pub(super) struct Engine {
    pub(super) host: Arc<dyn Host>,
    pub(super) clock: Arc<dyn Clock>,
    executable: PathBuf,
    directory: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}
pub(super) struct Output {
    pub(super) exit: Exit,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

impl Engine {
    /// Resolve only the explicitly selected Podman executable; never invoke a
    /// daemon, shell, VM or command while parsing or discovering it.
    pub(super) fn new(
        host: Arc<dyn Host>,
        clock: Arc<dyn Clock>,
        directory: PathBuf,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let path = environment
            .get(OsStr::new("PATH"))
            .cloned()
            .unwrap_or_else(|| "/bin:/usr/bin".into());
        let executable = std::env::split_paths(&path)
            .map(|path| path.join("podman"))
            .find(|path| {
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| anyhow::anyhow!("Podman is not on PATH"))?;
        Ok(Self {
            host,
            clock,
            executable,
            directory,
            environment,
        })
    }
    pub(super) fn spawn(
        &self,
        args: &[String],
        timeout: Duration,
        stdout_limit: u64,
        stdout: Sink,
        stderr: Sink,
        cleanup: bool,
    ) -> Result<Box<dyn Child>> {
        self.host.spawn(Command {
            executable: self.executable.clone(),
            arguments: args.to_vec(),
            directory: self.directory.clone(),
            environment: self.environment.clone(),
            timeout,
            stdout_limit,
            stderr_limit: 2 * MIB,
            input: Vec::new(),
            stdout,
            stderr,
            cleanup,
        })
    }
    pub(super) async fn call(
        &self,
        args: &[String],
        timeout: Duration,
        limit: u64,
        stdout: Sink,
        stderr: Sink,
        cleanup: bool,
    ) -> Result<Output> {
        let mut child = self.spawn(args, timeout, limit, stdout, stderr, cleanup)?;
        let deadline = self.clock.elapsed_ms() + timeout.as_secs_f64() * 1000.0;
        let observed: Result<Exit> = async {
            loop {
                if let Some(exit) = child.poll().await? {
                    return Ok(exit);
                }
                ensure!(
                    self.clock.elapsed_ms() <= deadline,
                    "Owned Podman command timed out"
                );
                if cleanup {
                    self.clock.sleep(Duration::from_millis(20)).await;
                } else {
                    tokio::select! {
                        _=self.host.cancelled()=>bail!("Benchmark interrupted"),
                        _=self.clock.sleep(Duration::from_millis(20))=>{},
                    }
                }
            }
        }
        .await;
        // Poll never reaps the leader. Even a successful command must reclaim
        // any descendants and close both bounded sinks before exposing output.
        let stopped = child.stop(Duration::ZERO).await;
        let exit = match (observed, stopped) {
            (Ok(exit), Ok(_)) => exit,
            (Err(error), Ok(_)) => return Err(error),
            (result, Err(error)) => {
                return Err(error.context(result.err().map_or_else(
                    || "Owned Podman command cleanup failed".into(),
                    |prior| format!("{prior}; owned Podman command cleanup failed"),
                )));
            }
        };
        Ok(Output {
            exit,
            stdout: child.output().to_vec(),
            stderr: child.errors().to_vec(),
        })
    }
    pub(super) async fn run(&self, args: &[String], timeout: u64, cleanup: bool) -> Result<String> {
        let output = self
            .call(
                args,
                Duration::from_secs(timeout),
                64 * MIB,
                Sink::Capture,
                Sink::Capture,
                cleanup,
            )
            .await?;
        ensure!(
            output.exit.code == Some(0) && output.exit.signal.is_none(),
            "{}",
            failure(args, &output)
        );
        Ok(String::from_utf8(output.stdout)?)
    }
    pub(super) async fn exec(&self, id: &str, script: &str) -> Result<String> {
        self.run(
            &[
                "exec".into(),
                id.into(),
                "sh".into(),
                "-c".into(),
                script.into(),
            ],
            10,
            false,
        )
        .await
    }
    pub(super) async fn state(&self, id: &str, cleanup: bool) -> Result<Option<State>> {
        let output = self
            .call(
                &[
                    "inspect".into(),
                    "--format".into(),
                    "{{.State.Status}} {{.State.ExitCode}}".into(),
                    id.into(),
                ],
                Duration::from_secs(20),
                MIB,
                Sink::Capture,
                Sink::Capture,
                cleanup,
            )
            .await?;
        if output.exit.code != Some(0) || output.exit.signal.is_some() {
            return Ok(None);
        }
        let value = String::from_utf8(output.stdout)?;
        let mut fields = value.split_whitespace();
        let status = fields.next().unwrap_or("").to_owned();
        let code = fields.next().and_then(|value| value.parse().ok());
        Ok(Some(State { status, code }))
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct State {
    pub(super) status: String,
    pub(super) code: Option<i32>,
}
impl State {
    /// Only terminal engine states with an actual recorded exit code admit
    /// immutable artifact collection. Paused/restarting/stopping remain live.
    pub(super) fn terminal(&self) -> bool {
        matches!(self.status.as_str(), "exited" | "stopped" | "dead") && self.code.is_some()
    }
}

pub(super) fn failure(args: &[String], output: &Output) -> String {
    let bytes = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let text = String::from_utf8_lossy(bytes);
    let mut detail = text.trim().lines().last().unwrap_or("no output").to_owned();
    for argument in args {
        if let Some(token) = argument.strip_prefix("METRICS_TOKEN=")
            && !token.is_empty()
        {
            detail = detail.replace(token, "[redacted]");
        }
    }
    format!(
        "podman {} failed: {detail}",
        args.first().map_or("", String::as_str)
    )
}

pub(super) fn require_image(image: &str) -> Result<()> {
    ensure!(
        !image.is_empty()
            && image.len() <= 4096
            && !image.contains('\0')
            && !image.starts_with('-')
            && !image.bytes().any(|b| b.is_ascii_whitespace()),
        "Image reference must be one non-option argument"
    );
    Ok(())
}

pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Podman image IDs have one SHA256 body, with an optional explicit algorithm prefix.
pub(super) fn valid_image_id(id: &str) -> bool {
    valid_id(id.strip_prefix("sha256:").unwrap_or(id))
}
