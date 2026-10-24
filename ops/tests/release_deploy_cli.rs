//! Actual fixed-route CLI: no network, stage layout, root or runtime test override.
#![cfg(unix)]
use simplestchat_ops::owned_process::{self, Group};
use std::{
    ffi::OsString,
    fs,
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};
const MAX_OUTPUT: usize = 65536;
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    cwd: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let cwd = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "release-deploy-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&cwd).unwrap();
        Self { cwd }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir(&self.cwd).unwrap();
    }
}
async fn read(reader: &mut (impl AsyncRead + Unpin), bytes: &mut Vec<u8>) -> std::io::Result<()> {
    let mut chunk = vec![0; 4096];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            return Ok(());
        }
        if bytes.len() + count > MAX_OUTPUT {
            return Err(std::io::Error::other("CLI fixture output bound"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn cli(
    fixture: &Fixture,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
) -> Output {
    cli_with_stdout(fixture, arguments, environment, Stdio::piped())
        .await
        .unwrap()
}

async fn cli_with_stdout(
    fixture: &Fixture,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    stdout: Stdio,
) -> std::io::Result<Output> {
    cli_with_control(fixture, arguments, environment, stdout, None).await
}
async fn cli_with_control(
    fixture: &Fixture,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    stdout: Stdio,
    signal: Option<(&std::path::Path, rustix::process::Signal)>,
) -> std::io::Result<Output> {
    let end = Instant::now() + Duration::from_secs(30);
    let body_end = end - Duration::from_secs(5);
    let child = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .env_clear()
        .envs(environment.iter().cloned())
        .current_dir(&fixture.cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let mut group = Group::new(child);
    let pid = group.pid();
    let stdout = group
        .child()
        .stdout
        .take()
        .map(tokio::process::ChildStdout::from_std)
        .transpose();
    let stdout_converted = stdout.is_ok();
    let mut stdout = stdout.ok().flatten();
    let stderr = group
        .child()
        .stderr
        .take()
        .map(tokio::process::ChildStderr::from_std)
        .transpose();
    let converted = stdout_converted && stderr.as_ref().is_ok_and(Option::is_some);
    let mut stderr = stderr.ok().flatten();
    let mut out = Vec::new();
    let mut err = Vec::new();
    // A refused consuming conversion is negative. Keep every other known
    // reader and group owner until all independent finalizers have run.
    let body = if converted {
        Some(
            tokio::time::timeout_at(body_end.into(), async {
                tokio::join!(
                    async {
                        if let Some(stdout) = &mut stdout {
                            read(stdout, &mut out).await
                        } else {
                            Ok(())
                        }
                    },
                    async {
                        if let Some(stderr) = &mut stderr {
                            read(stderr, &mut err).await
                        } else {
                            Err(std::io::Error::other("missing stderr reader"))
                        }
                    },
                    owned_process::exited(pid),
                    async {
                        if let Some((ready, signal)) = signal {
                            while !ready.exists() {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            rustix::process::kill_process(pid, signal)
                                .map_err(std::io::Error::from)?;
                        }
                        Ok::<(), std::io::Error>(())
                    }
                )
            })
            .await,
        )
    } else {
        None
    };
    let retired = owned_process::finish_before(pid, end).await;
    let terminal = tokio::time::timeout_at(end.into(), owned_process::exited(pid)).await;
    // Consume and join the reserved child after group retirement. A timed-out
    // consuming reap would detach its blocking waiter instead of settling it.
    let status = group.reap().await;
    let stdout_closed = stdout.is_none_or(|stdout| {
        stdout
            .into_owned_fd()
            .and_then(|descriptor| nix::unistd::close(descriptor).map_err(std::io::Error::from))
            .is_ok()
    });
    let stderr_closed = stderr.is_some_and(|stderr| {
        stderr
            .into_owned_fd()
            .and_then(|descriptor| nix::unistd::close(descriptor).map_err(std::io::Error::from))
            .is_ok()
    });
    if !converted
        || !matches!(body, Some(Ok((Ok(()), Ok(()), Ok(()), Ok(())))))
        || !retired
        || !matches!(terminal, Ok(Ok(())))
        || !stdout_closed
        || !stderr_closed
        || Instant::now() >= end
    {
        return Err(std::io::Error::other("bounded CLI owner did not settle"));
    }
    Ok(Output {
        status: status.map_err(std::io::Error::other)?,
        stdout: out,
        stderr: err,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn help_and_notices_are_early_and_readonly_stdout_is_checked() {
    let fixture = Fixture::new();
    let mut observations = Vec::new();
    for flag in ["--help", "-h", "--licenses"] {
        let args = ["release", "deploy", flag].map(OsString::from);
        let normal = cli(&fixture, &args, &[]).await;
        let original = fs::File::open("/dev/null").unwrap();
        let refused = match original.try_clone() {
            Ok(clone) => cli_with_stdout(&fixture, &args, &[], Stdio::from(clone)).await,
            Err(error) => Err(error),
        };
        let closed = nix::unistd::close(original);
        observations.push((flag, normal, refused, closed));
    }
    let empty = fs::read_dir(&fixture.cwd).unwrap().next().is_none();
    drop(fixture);
    for (flag, normal, refused, closed) in observations {
        assert!(closed.is_ok());
        assert!(normal.status.success());
        assert!(normal.stderr.is_empty());
        if flag == "--licenses" {
            assert!(String::from_utf8_lossy(&normal.stdout).contains("signal-hook"));
        } else {
            assert!(
                normal
                    .stdout
                    .starts_with(b"Usage: simplestchat-ops release deploy ")
            );
            assert!(String::from_utf8_lossy(&normal.stdout).contains("0<M<N<=1800"));
        }
        assert_eq!(refused.unwrap().status.code(), Some(1));
    }
    assert!(empty);
}
#[tokio::test(flavor = "current_thread")]
async fn missing_duplicate_and_unknown_selectors_are_usage_two_without_effects() {
    let fixture = Fixture::new();
    let revision = "a".repeat(40);
    let mut results = Vec::new();
    for tail in [
        vec![],
        vec!["--revision", &revision],
        vec!["--revision", &revision, "--revision", &revision],
        vec!["--root", "/PRIVATE/not-a-selector"],
        vec!["--help", "--licenses"],
        vec![
            "--revision",
            &revision,
            "--timeout-seconds",
            "30",
            "--finalization-seconds",
        ],
    ] {
        let args = ["release", "deploy"]
            .into_iter()
            .chain(tail)
            .map(OsString::from)
            .collect::<Vec<_>>();
        results.push(cli(&fixture, &args, &[]).await);
    }
    let empty = fs::read_dir(&fixture.cwd).unwrap().next().is_none();
    drop(fixture);
    for result in results {
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
    }
    assert!(empty);
}
#[tokio::test(flavor = "current_thread")]
async fn budget_and_revision_domains_do_not_admit_a_host_operation() {
    let fixture = Fixture::new();
    let mut results = Vec::new();
    for (revision, n, m) in [
        ("a".repeat(40), "1801", "1"),
        ("a".repeat(40), "1800", "1800"),
        ("a".repeat(40), "600", "0"),
        ("a".repeat(40), "+600", "60"),
        ("a".repeat(40), "6_00", "60"),
        ("A".repeat(40), "600", "60"),
        ("a".repeat(39), "600", "60"),
        ("a".repeat(40), "18446744073709551616", "60"),
    ] {
        let args = [
            "release".into(),
            "deploy".into(),
            format!("--revision={revision}").into(),
            format!("--timeout-seconds={n}").into(),
            format!("--finalization-seconds={m}").into(),
        ];
        results.push(cli(&fixture, &args, &[]).await);
    }
    let empty = fs::read_dir(&fixture.cwd).unwrap().next().is_none();
    drop(fixture);
    for result in results {
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
    }
    assert!(empty);
}
