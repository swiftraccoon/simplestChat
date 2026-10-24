//! Actual release-fetch CLI over owned inert tools, never network or host proof.
#![cfg(unix)]

#[path = "../src/release/attestation/verify/fixture.rs"]
mod cached;
#[path = "../src/release/attestation/fetch/fixture.rs"]
mod fixture;
#[path = "../src/release/attestation/acquire/fixture.rs"]
mod fresh;
use fresh::Fixture;
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
    "Usage: simplestchat-ops release fetch --repository REPO --revision SHA ",
    "--artifact-id N --ci-run N --host HOST --user USER --identity ABS ",
    "[--port N] --output-parent ABS [--verified-directory ABS] ",
    "--timeout-seconds N --finalization-seconds M\n",
    "Verify explicit server bytes, then fetch through the prepared Python SSH receiver. ",
    "0 < M < N; no automatic retry or installed helper switch.\n"
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

fn arguments(fixture: &fixture::Fixture, cached: bool) -> Vec<OsString> {
    let mut args = vec!["release".into(), "fetch".into()];
    args.extend(fixture.arguments(cached));
    args
}
#[tokio::test(flavor = "current_thread")]
async fn help_notices_and_readonly_stdout_have_no_environment_or_tool_effects() {
    let fixture = fixture::Fixture::new();
    let before = fixture.inner.snapshot();
    let mut observations = Vec::new();
    for flag in ["--help", "-h", "--licenses"] {
        let args = ["release", "fetch", flag].map(OsString::from);
        let normal = cli(&fixture.inner, &args, &[]).await;
        let original = fs::File::open("/dev/null").unwrap();
        let refused = match original.try_clone() {
            Ok(clone) => cli_with_stdout(&fixture.inner, &args, &[], Stdio::from(clone)).await,
            Err(error) => Err(error),
        };
        let closed = nix::unistd::close(original);
        observations.push((flag, normal, refused, closed));
    }
    let after = fixture.inner.snapshot();
    let calls = fixture.inner.read("argv");
    let attempts = fixture.inner.attempts();
    drop(fixture);
    assert_eq!(before.unwrap(), after.unwrap());
    assert!(attempts.unwrap().is_empty() && calls.unwrap().is_empty());
    for (flag, normal, refused, closed) in observations {
        closed.unwrap();
        assert_eq!(normal.status.code(), Some(0));
        assert!(normal.stderr.is_empty());
        if flag == "--licenses" {
            let text = std::str::from_utf8(&normal.stdout).unwrap();
            assert!(text.contains("httparse 1.10.1") && text.contains("rawzip 0.5.2"));
        } else {
            assert_eq!(normal.stdout, USAGE.as_bytes());
        }
        let refused = refused.unwrap();
        assert_eq!(refused.status.code(), Some(1));
        assert!(refused.stdout.is_empty());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn complete_cached_and_downloaded_cli_bind_receipts_and_keep_input_inodes() {
    let mut facts = Vec::new();
    for cached in [false, true] {
        let fixture = fixture::Fixture::new();
        let before = fixture.inner.snapshot();
        let expected_receipt = fixture.receipt();
        let env: Vec<_> = fixture.inner.environment().into_iter().collect();
        let output = cli(&fixture.inner, &arguments(&fixture, cached), &env).await;
        let after = fixture.inner.snapshot();
        let attempts = fixture.inner.attempts();
        let outcome = attempts
            .as_ref()
            .ok()
            .and_then(|v| v.first())
            .map(|path| fs::read(path.join("outcome.json")));
        drop(fixture);
        facts.push((before, output, after, attempts, outcome, expected_receipt));
    }
    for (before, output, after, attempts, outcome, expected_receipt) in facts {
        assert_eq!(output.status.code(), Some(0), "{:?}", output);
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report.as_object().unwrap().len(), 10);
        assert_eq!(report["passed"], true);
        assert_eq!(report["remoteSettled"], true);
        assert_eq!(report["remote"], expected_receipt);
        assert_eq!(before.unwrap(), after.unwrap());
        assert_eq!(attempts.unwrap().len(), 1);
        let saved: Value = serde_json::from_slice(&outcome.unwrap().unwrap()).unwrap();
        assert_eq!(saved["remote"], report["remote"]);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn syntax_and_parsed_selector_statuses_preserve_the_reference_split() {
    let fixture = fixture::Fixture::new();
    let valid = arguments(&fixture, true);
    let mut variants = Vec::new();
    for (name, value) in [
        ("--artifact-id", "+123"),
        ("--ci-run", "07"),
        ("--port", "65536"),
        ("--user", "bad user"),
        ("--host", "::1"),
        ("--verified-directory", ""),
        ("--finalization-seconds", "60"),
    ] {
        let mut args = valid.clone();
        if name == "--port" {
            args.extend([name.into(), value.into()]);
        } else {
            let at = args.iter().position(|v| v == name).unwrap() + 1;
            args[at] = value.into();
        }
        variants.push((args, name == "--finalization-seconds"));
    }
    let mut duplicate = valid.clone();
    duplicate.extend(["--host".into(), "again.example".into()]);
    variants.push((duplicate, true));
    let mut unknown = valid;
    unknown.extend(["--other".into(), "x".into()]);
    variants.push((unknown, true));
    let mut outputs = Vec::new();
    for (args, usage) in variants {
        outputs.push((cli(&fixture.inner, &args, &[]).await, usage));
    }
    let calls = fixture.inner.read("argv");
    let attempts = fixture.inner.attempts();
    drop(fixture);
    assert!(calls.unwrap().is_empty() && attempts.unwrap().is_empty());
    for (output, usage) in outputs {
        if usage {
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr, USAGE.as_bytes());
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stderr.is_empty());
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                report,
                serde_json::json!({"passed":false,"phase":"options",
                "failureClass":"invalid_options"})
            );
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn equals_flags_reach_fixed_operational_failure_without_leaking_context() {
    let fixture = fixture::Fixture::new();
    let values = arguments(&fixture, true);
    let mut args = values[..2].to_vec();
    for pair in values[2..].as_chunks::<2>().0 {
        let mut combined = pair[0].clone();
        combined.push("=");
        combined.push(&pair[1]);
        args.push(combined);
    }
    let output = cli(&fixture.inner, &args, &[]).await;
    let attempts = fixture.inner.attempts();
    drop(fixture);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["phase"], "github_metadata");
    assert_eq!(report["failureClass"], "controller_operation_failed");
    assert_eq!(attempts.unwrap().len(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn original_int_and_term_cancel_waiting_receipts_and_settle_all_local_peers() {
    let mut facts = Vec::new();
    for signal in [rustix::process::Signal::INT, rustix::process::Signal::TERM] {
        let fixture = fixture::Fixture::new();
        let ready = fixture.inner.home.join("signal-ready");
        fixture
            .script(concat!(
                "IFS= read -r envelope || exit 92\n",
                "printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"ready\"}'\n",
                "IFS= read -r grant || exit 93\n",
                "printf ready > \"$HOME/signal-ready\" || exit 94\n",
                "exec /bin/sleep 20\n"
            ))
            .unwrap();
        let environment: Vec<_> = fixture.inner.environment().into_iter().collect();
        let output = cli_with_control(
            &fixture.inner,
            &arguments(&fixture, true),
            &environment,
            Stdio::piped(),
            Some((&ready, signal)),
        )
        .await;
        let ready_observed = ready.exists();
        let attempts = fixture.inner.attempts();
        drop(fixture);
        facts.push((output, ready_observed, attempts));
    }
    for (output, ready, attempts) in facts {
        let output = output.unwrap();
        assert!(ready);
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["failureClass"], "interrupted");
        assert_eq!(report["passed"], false);
        assert_eq!(report["remoteSettled"], false);
        assert_eq!(attempts.unwrap().len(), 1);
    }
}
