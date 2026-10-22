//! Structured engine argv; isolated loopback control requests never use host ports.

use super::process::{Limits, Output, Request, Runner};
use anyhow::{Result, bail, ensure};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[async_trait::async_trait]
pub(super) trait Engine: Send + Sync {
    fn executable(&self) -> &Path;
    async fn call(&self, arguments: &[String], request: Request) -> Result<Output>;

    async fn run(&self, arguments: &[String], timeout: u64, check: bool) -> Result<String> {
        let output = self
            .call(
                arguments,
                Request {
                    limits: Limits {
                        timeout: Duration::from_secs(timeout),
                        ..Limits::default()
                    },
                    ..Request::default()
                },
            )
            .await?;
        if check && output.status != 0 {
            let errors = if output.stderr.is_empty() {
                &output.stdout
            } else {
                &output.stderr
            };
            let text = String::from_utf8_lossy(errors);
            let mut detail = text.trim().lines().last().unwrap_or("no output").to_owned();
            for argument in arguments {
                if let Some(token) = argument.strip_prefix("METRICS_TOKEN=")
                    && !token.is_empty()
                {
                    detail = detail.replace(token, "[redacted]");
                }
            }
            bail!(
                "{} {} failed: {detail}",
                self.executable()
                    .file_name()
                    .unwrap_or(OsStr::new("engine"))
                    .to_string_lossy(),
                arguments[0]
            );
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    async fn read(&self, container: &str, path: &str) -> Result<String> {
        self.run(
            &["exec".into(), container.into(), "cat".into(), path.into()],
            20,
            true,
        )
        .await
    }

    async fn running(&self, container: &str) -> Result<bool> {
        Ok(self
            .run(
                &[
                    "inspect".into(),
                    "--format".into(),
                    "{{.State.Running}}".into(),
                    container.into(),
                ],
                20,
                false,
            )
            .await?
            .trim()
            == "true")
    }

    async fn oom_killed(&self, container: &str) -> Result<bool> {
        let output = self
            .call(
                &[
                    "inspect".into(),
                    "--format".into(),
                    "{{.State.OOMKilled}}".into(),
                    container.into(),
                ],
                Request {
                    cleanup: true,
                    limits: Limits {
                        timeout: Duration::from_secs(20),
                        ..Limits::default()
                    },
                    ..Request::default()
                },
            )
            .await?;
        Ok(String::from_utf8(output.stdout)?.trim() == "true")
    }

    async fn logs(&self, container: &str) -> Result<String> {
        let output = self
            .call(
                &[
                    "logs".into(),
                    "--tail".into(),
                    "1000".into(),
                    container.into(),
                ],
                Request {
                    limits: Limits {
                        timeout: Duration::from_secs(60),
                        ..Limits::default()
                    },
                    cleanup: true,
                    ..Request::default()
                },
            )
            .await?;
        let mut bytes = output.stdout;
        bytes.extend(output.stderr);
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn control(&self, container: &str, path: &str, token: &str) -> Result<String> {
        ensure!(
            matches!(path, "/health" | "/metrics")
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "invalid capacity control request"
        );
        let authorization = if token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {token}\r\n")
        };
        let input = format!(
            concat!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\n",
                "Connection: close\r\n{authorization}\r\n"
            ),
            path = path,
            authorization = authorization
        )
        .into_bytes();
        let output = self
            .call(
                &[
                    "exec".into(),
                    "--interactive".into(),
                    container.into(),
                    "/bin/bash".into(),
                    "-c".into(),
                    "exec 3<>/dev/tcp/127.0.0.1/3000; cat >&3; cat <&3".into(),
                ],
                Request {
                    limits: Limits {
                        timeout: Duration::from_secs(5),
                        stdout: 512 * 1024,
                        stderr: 65536,
                    },
                    input,
                    ..Request::default()
                },
            )
            .await?;
        parse_control(output)
    }
}

pub(super) struct NativeEngine {
    executable: PathBuf,
    runner: Runner,
}

impl NativeEngine {
    pub(super) fn discover(preferred: Option<&str>, runner: Runner) -> Result<Self> {
        let path = runner
            .environment
            .get(OsStr::new("PATH"))
            .cloned()
            .unwrap_or_else(|| "/bin:/usr/bin".into());
        let candidates = preferred.map_or_else(|| vec!["docker", "podman"], |value| vec![value]);
        for candidate in candidates {
            for directory in std::env::split_paths(&path) {
                let executable = directory.join(candidate);
                use std::os::unix::fs::PermissionsExt;
                if executable.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                }) {
                    return Ok(Self { executable, runner });
                }
            }
        }
        bail!("Neither docker nor podman is on PATH")
    }
}

#[async_trait::async_trait]
impl Engine for NativeEngine {
    fn executable(&self) -> &Path {
        &self.executable
    }
    async fn call(&self, arguments: &[String], request: Request) -> Result<Output> {
        self.runner.run(&self.executable, arguments, request).await
    }
}

fn split<'a>(mut bytes: &'a [u8], separator: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    while let Some(index) = bytes
        .windows(separator.len())
        .position(|part| part == separator)
    {
        parts.push(&bytes[..index]);
        bytes = &bytes[index + separator.len()..];
    }
    parts.push(bytes);
    parts
}

pub(super) fn parse_control(output: Output) -> Result<String> {
    let raw = output.stdout;
    let boundary = raw.windows(4).position(|value| value == b"\r\n\r\n");
    let Some(boundary) = boundary else {
        bail!("capacity control response is incomplete or unsuccessful");
    };
    let body = &raw[boundary + 4..];
    let lines = split(&raw[..boundary], b"\r\n");
    let mut headers = std::collections::BTreeMap::new();
    for line in &lines[1..] {
        let (name, value) = line
            .iter()
            .position(|byte| *byte == b':')
            .map_or((*line, &[][..]), |index| {
                (&line[..index], &line[index + 1..])
            });
        headers.insert(name.to_ascii_lowercase(), value.trim_ascii());
    }
    ensure!(
        output.status == 0
            && lines[0] == b"HTTP/1.1 200 OK"
            && headers.get(b"content-length".as_slice()).copied()
                == Some(body.len().to_string().as_bytes()),
        "capacity control response is incomplete or unsuccessful"
    );
    Ok(String::from_utf8(body.to_vec())?)
}

pub(super) fn sandbox(engine: &dyn Engine) -> Vec<String> {
    let mut arguments = [
        "--pull",
        "never",
        "--read-only",
        "--user",
        "10001:10001",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--ulimit",
        "core=0:0",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=64m",
    ]
    .map(str::to_owned)
    .to_vec();
    let logs = if engine.executable().file_name() == Some(OsStr::new("podman")) {
        vec!["--log-driver", "k8s-file", "--log-opt", "max-size=10mb"]
    } else {
        vec![
            "--log-driver",
            "local",
            "--log-opt",
            "max-size=10m",
            "--log-opt",
            "max-file=3",
        ]
    };
    arguments.extend(logs.into_iter().map(str::to_owned));
    arguments
}
