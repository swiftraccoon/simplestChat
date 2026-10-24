//! Actual fresh/cached local CLI routing over owned inert data, never signatures/network.
#![cfg(unix)]

#[path = "../src/release/attestation/verify/fixture.rs"]
mod cached;
#[path = "../src/release/attestation/acquire/fixture.rs"]
mod fixture;
use fixture::Fixture;
use serde_json::Value;
use simplestchat_ops::owned_process::{self, Group};
use std::{
    ffi::OsString,
    fs,
    os::unix::process::CommandExt,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_OUTPUT: usize = 65536;
const USAGE: &str = concat!(
    "Usage: simplestchat-ops release verify --repository REPO --revision SHA ",
    "--artifact-id N --ci-run N --output-parent ABS [--artifact-dir ABS] ",
    "--timeout-seconds N --finalization-seconds M\n",
    "Fresh API identity and signed local bytes; 0 < M < N. No SSH or deployment-host operation.\n"
);

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
                    owned_process::exited(pid)
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
        || !matches!(body, Some(Ok((Ok(()), Ok(()), Ok(())))))
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

fn arguments(fixture: &Fixture, cached: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["release".into(), "verify".into()];
    args.extend(fixture.arguments(cached));
    args
}
#[tokio::test(flavor = "current_thread")]
async fn help_notices_and_readonly_stdout_require_no_environment_or_tool() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    let mut observations = Vec::new();
    for flag in ["--help", "-h", "--licenses"] {
        let args: Vec<OsString> = ["release", "verify", flag]
            .into_iter()
            .map(Into::into)
            .collect();
        let normal = cli(&fixture, &args, &[]).await;
        let original = fs::File::open("/dev/null").unwrap();
        let cloned = original.try_clone();
        let refused = match cloned {
            Ok(cloned) => cli_with_stdout(&fixture, &args, &[], Stdio::from(cloned)).await,
            Err(error) => Err(error),
        };
        let closed = nix::unistd::close(original);
        observations.push((flag, normal, refused, closed));
    }
    let after = fixture.snapshot();
    let calls = fixture.read("argv");
    let empty = fs::read_dir(&fixture.cwd).map(|entries| entries.count());
    drop(fixture);
    assert_eq!(before.unwrap(), after.unwrap());
    assert!(calls.unwrap().is_empty());
    assert_eq!(empty.unwrap(), 0);
    for (flag, normal, refused, closed) in observations {
        closed.unwrap();
        assert_eq!(normal.status.code(), Some(0));
        assert!(normal.stderr.is_empty());
        if flag == "--licenses" {
            let text = std::str::from_utf8(&normal.stdout).unwrap();
            assert!(text.contains("httparse 1.10.1") && text.contains("getrandom 0.4.3"));
            assert!(
                text.contains("rawzip 0.5.2") && text.contains("not a complete license inventory")
            );
        } else {
            assert_eq!(normal.stdout, USAGE.as_bytes());
        }
        let refused = refused.unwrap();
        assert_eq!(refused.status.code(), Some(1));
        assert!(refused.stdout.is_empty());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn complete_cli_fresh_and_cached_results_have_exactly_four_public_keys() {
    let mut facts = Vec::new();
    for cached in [false, true] {
        let fixture = Fixture::new();
        if cached {
            fs::remove_file(fixture.bin.join("curl")).unwrap();
        }
        let before = fixture.snapshot();
        let env: Vec<_> = fixture.environment().into_iter().collect();
        let result = cli(&fixture, &arguments(&fixture, cached), &env).await;
        let after = fixture.snapshot();
        let expected = fixture.selection["revision"].clone();
        let calls = fixture.read("argv");
        let attempts = fixture.attempts();
        drop(fixture);
        facts.push((cached, before, after, result, expected, calls, attempts));
    }
    for (cached, before, after, result, expected, calls, attempts) in facts {
        assert_eq!(before.unwrap(), after.unwrap());
        assert_eq!(result.status.code(), Some(0));
        assert!(result.stderr.is_empty());
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert_eq!(value["revision"], expected);
        assert_eq!(
            String::from_utf8(calls.unwrap())
                .unwrap()
                .contains("auth\ntoken"),
            !cached
        );
        assert_eq!(attempts.unwrap().len(), usize::from(!cached));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn duplicate_unknown_invalid_numeric_and_budget_options_are_private_usage_failures() {
    let fixture = Fixture::new();
    let base = arguments(&fixture, true);
    let mut observations = Vec::new();
    for suffix in [
        vec!["--ci-run", "789"],
        vec!["--unknown", "PRIVATE"],
        vec!["--licenses"],
        vec!["--timeout-seconds=0"],
        vec!["--artifact-id=+123"],
    ] {
        let mut args = base.clone();
        args.extend(suffix.into_iter().map(OsString::from));
        observations.push(cli(&fixture, &args, &[]).await);
    }
    let calls = fixture.read("argv");
    let empty = fs::read_dir(&fixture.cwd).map(|entries| entries.count());
    drop(fixture);
    assert!(calls.unwrap().is_empty());
    assert_eq!(empty.unwrap(), 0);
    for output in observations {
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, USAGE.as_bytes());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn missing_tools_are_fixed_negative_and_equals_forms_reach_the_operation() {
    let fixture = Fixture::new();
    let original = arguments(&fixture, true);
    let mut args: Vec<OsString> = vec!["release".into(), "verify".into()];
    for pair in original[2..].as_chunks::<2>().0 {
        let mut value = pair[0].clone();
        value.push("=");
        value.push(&pair[1]);
        args.push(value);
    }
    let failed = cli(&fixture, &args, &[]).await;
    let calls = fixture.read("argv");
    drop(fixture);
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stderr.is_empty());
    assert_eq!(
        failed.stdout,
        b"{\"passed\":false,\"failureClass\":\"release_attestation_failed\"}\n"
    );
    assert!(calls.unwrap().is_empty());
}
