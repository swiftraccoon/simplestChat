//! Actual cached-verification CLI over owned fake verifier data, never signatures/network.
#![cfg(unix)]

#[path = "../src/release/attestation/verify/fixture.rs"]
mod fixture;
use fixture::Fixture;
use serde_json::Value;
use sha2::{Digest, Sha256};
use simplestchat_ops::owned_process::{self, Group};
use std::{
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt, process::CommandExt},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_OUTPUT: usize = 65536;
const USAGE: &str = concat!(
    "Usage: simplestchat-ops release attestation verify --directory ABS ",
    "--selection-json JSON --timeout-seconds N --finalization-seconds M\n",
    "Verify cached server bytes; 0 < M < N. Selection data does not prove fresh API identity.\n",
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

fn arguments(fixture: &Fixture, selected: &Value) -> Vec<OsString> {
    vec![
        "release".into(),
        "attestation".into(),
        "verify".into(),
        "--directory".into(),
        fixture.candidate.as_os_str().into(),
        "--selection-json".into(),
        serde_json::to_string(selected).unwrap().into(),
        "--timeout-seconds".into(),
        "20".into(),
        "--finalization-seconds".into(),
        "5".into(),
    ]
}
fn selected(fixture: &Fixture) -> Value {
    serde_json::to_value(&fixture.selection).unwrap()
}
fn environment(fixture: &Fixture) -> Vec<(OsString, OsString)> {
    vec![
        ("PATH".into(), fixture.bin.as_os_str().into()),
        ("HOME".into(), fixture.home.as_os_str().into()),
        ("LC_ALL".into(), "must-be-overridden".into()),
        ("UNRELATED".into(), "must-not-inherit".into()),
    ]
}
fn private_values_absent(fixture: &Fixture, result: &Output) {
    for bytes in [&result.stdout, &result.stderr] {
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(!text.contains(fixture.root.to_str().unwrap()));
        assert!(!text.contains("PRIVATE"));
    }
}
fn usage(fixture: &Fixture, result: &Output) {
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert_eq!(result.stderr, USAGE.as_bytes());
    private_values_absent(fixture, result);
}
fn no_dispatch(fixture: &Fixture) {
    assert!(
        fixture::read(&fixture.home.join("argv"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read_dir(&fixture.cwd).unwrap().count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn help_and_licenses_need_no_environment_and_readonly_stdout_cannot_pass() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    for flag in ["--help", "-h", "--licenses"] {
        let args: Vec<OsString> = ["release", "attestation", "verify", flag]
            .into_iter()
            .map(OsString::from)
            .collect();
        let output = cli(&fixture, &args, &[]).await;
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        if flag != "--licenses" {
            assert_eq!(output.stdout, USAGE.as_bytes());
        } else {
            let text = std::str::from_utf8(&output.stdout).unwrap();
            assert!(text.contains("rawzip 0.5.2") && text.contains("Permission is hereby granted"));
            assert!(text.contains("not a complete license inventory"));
        }
        private_values_absent(&fixture, &output);
        let original = fs::File::open("/dev/null").unwrap();
        let readonly = match original.try_clone() {
            Ok(cloned) => Stdio::from(cloned),
            Err(first) => {
                let closed = nix::unistd::close(original);
                panic!(
                    "read-only stdout clone failed: {first}; original close attempt: {closed:?}"
                );
            }
        };
        let refused = cli_with_stdout(&fixture, &args, &[], readonly).await;
        let closed = nix::unistd::close(original);
        assert!(closed.is_ok());
        let refused = refused.unwrap();
        assert_eq!(refused.status.code(), Some(1));
        assert!(refused.stdout.is_empty());
        private_values_absent(&fixture, &refused);
    }
    assert_eq!(before, fixture.snapshot().unwrap());
    no_dispatch(&fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_unknown_budget_path_and_non_utf8_selectors_are_usage_failures() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    let base = arguments(&fixture, &selected(&fixture));
    for suffix in [
        vec!["--directory=PRIVATE-duplicate"],
        vec!["--unknown", "PRIVATE-value"],
        vec!["--timeout-seconds=20"],
        vec!["--selection-json={}"],
        vec!["--help"],
    ] {
        let mut args = base.clone();
        args.extend(suffix.into_iter().map(OsString::from));
        usage(&fixture, &cli(&fixture, &args, &[]).await);
    }
    for (index, value) in [
        (8, "0"),
        (8, "-1"),
        (8, "+20"),
        (8, "20.0"),
        (10, "0"),
        (10, "20"),
        (10, "21"),
        (4, "relative"),
        (4, "/PRIVATE/../not-canonical"),
    ] {
        let mut args = base.clone();
        args[index] = value.into();
        usage(&fixture, &cli(&fixture, &args, &[]).await);
    }
    let mut args = base.clone();
    args[6] = OsString::from_vec(vec![0xff]);
    usage(&fixture, &cli(&fixture, &args, &[]).await);
    let mut missing = base;
    let _ = missing.pop();
    usage(&fixture, &cli(&fixture, &missing, &[]).await);
    assert_eq!(before, fixture.snapshot().unwrap());
    no_dispatch(&fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn passive_selection_requires_exact_nine_keys_and_fifty_three_bit_integer_fields() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    for (key, value) in [
        ("schemaVersion", Value::Bool(true)),
        ("artifactId", 0.into()),
        ("ciRunId", 790.into()),
        ("runAttempt", 0.into()),
        ("runAttempt", Value::Bool(true)),
        ("runAttempt", (1_u64 << 53).into()),
        ("artifactZipBytes", 0.into()),
        ("artifactZipBytes", (2_u64 * 1024 * 1024 * 1024 + 1).into()),
        ("repository", "PRIVATE-bad".into()),
        ("revision", "ABC".into()),
        ("zipSha256", "A".repeat(64).into()),
        ("attestation", serde_json::json!({})),
    ] {
        let mut metadata = selected(&fixture);
        metadata[key] = value;
        let args = arguments(&fixture, &metadata);
        usage(&fixture, &cli(&fixture, &args, &[]).await);
    }
    let mut metadata = selected(&fixture);
    let _ = metadata.as_object_mut().unwrap().remove("artifactId");
    usage(
        &fixture,
        &cli(&fixture, &arguments(&fixture, &metadata), &[]).await,
    );
    assert_eq!(before, fixture.snapshot().unwrap());
    no_dispatch(&fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn utf8_selection_limit_and_missing_local_candidate_refuse_before_any_verifier() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    let mut args = arguments(&fixture, &selected(&fixture));
    args[4] = fixture.root.join("absent").into_os_string();
    let json = serde_json::to_string(&selected(&fixture)).unwrap();
    let exact = format!("{json}{}", " ".repeat(16384 - json.len()));
    args[6] = exact.clone().into();
    let failed = cli(&fixture, &args, &environment(&fixture)).await;
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&failed.stdout).unwrap(),
        serde_json::json!({"passed":false,"failureClass":"release_attestation_failed"})
    );
    private_values_absent(&fixture, &failed);
    args[6] = format!("{exact} ").into();
    usage(&fixture, &cli(&fixture, &args, &[]).await);
    assert_eq!(before, fixture.snapshot().unwrap());
    no_dispatch(&fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn actual_native_cli_repeats_four_stub_calls_and_preserves_cached_originals() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    let args = arguments(&fixture, &selected(&fixture));
    for _ in 0..2 {
        let result = cli(&fixture, &args, &environment(&fixture)).await;
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert!(result.stderr.is_empty());
        private_values_absent(&fixture, &result);
        let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(envelope.as_object().unwrap().len(), 10);
        assert_eq!(envelope["attestation"]["predicate"], fixture.claim);
        let hash: String = Sha256::digest(&fixture.response)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let hashes = envelope["attestation"]["verificationSha256"]
            .as_object()
            .unwrap();
        assert_eq!(hashes.len(), 4);
        assert!(
            hashes
                .values()
                .all(|value| value == &Value::String(hash.clone()))
        );
    }
    let trace = String::from_utf8(fixture::read(&fixture.home.join("argv")).unwrap()).unwrap();
    let calls: Vec<_> = trace
        .split("--call--\n")
        .filter(|v| !v.is_empty())
        .collect();
    assert_eq!(calls.len(), 8);
    for (index, call) in calls.iter().enumerate() {
        let values: Vec<_> = call.lines().collect();
        let name = [
            "image.tar",
            "sbom.spdx.json",
            "runtime-proof.json",
            "vex.openvex.json",
        ][index % 4];
        assert_eq!(values.len(), 24);
        assert_eq!(&values[..2], ["attestation", "verify"]);
        assert_eq!(values[2], fixture.candidate.join(name).to_str().unwrap());
        assert_eq!(
            &values[3..6],
            [
                "--bundle",
                fixture
                    .candidate
                    .join("release-attestation.jsonl")
                    .to_str()
                    .unwrap(),
                "--repo"
            ]
        );
        assert!(values.contains(&"--deny-self-hosted-runners"));
        assert_eq!(&values[values.len() - 2..], ["--format", "json"]);
    }
    assert_eq!(before, fixture.snapshot().unwrap());
    assert_eq!(fs::read_dir(&fixture.cwd).unwrap().count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn attempts_above_prepare_domain_and_maximum_selected_ids_verify_without_truncation() {
    for maximum in [false, true] {
        let mut fixture = Fixture::new();
        if maximum {
            fixture.selection.artifact_id = (1_u64 << 53) - 1;
            fixture.selection.build_run_id = (1_u64 << 53) - 1;
            fixture.selection.ci_run_id = (1_u64 << 53) - 1;
        }
        fixture.selection.run_attempt = if maximum {
            (1_u64 << 53) - 1
        } else {
            1_u64 << 31
        };
        fixture.rebuild().unwrap();
        let before = fixture.snapshot().unwrap();
        let result = cli(
            &fixture,
            &arguments(&fixture, &selected(&fixture)),
            &environment(&fixture),
        )
        .await;
        assert_eq!(result.status.code(), Some(0));
        let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(envelope["runAttempt"], fixture.selection.run_attempt);
        assert_eq!(envelope["ciRunId"], fixture.selection.ci_run_id);
        assert_eq!(before, fixture.snapshot().unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn initial_and_late_operational_refusals_never_return_a_successful_envelope() {
    for late in [false, true] {
        let mut fixture = Fixture::new();
        let name = if late {
            "source.json"
        } else {
            "image-security.json"
        };
        let mut value: Value =
            serde_json::from_slice(&fixture::read(&fixture.candidate.join(name)).unwrap()).unwrap();
        if late {
            value["inputsSha256"] = serde_json::json!({});
        } else {
            value["passed"] = false.into();
        }
        fixture
            .rewrite(name, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        fixture.rebuild().unwrap();
        let before = fixture.snapshot().unwrap();
        let result = cli(
            &fixture,
            &arguments(&fixture, &selected(&fixture)),
            &environment(&fixture),
        )
        .await;
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap(),
            serde_json::json!({"passed":false,"failureClass":"release_attestation_failed"})
        );
        private_values_absent(&fixture, &result);
        let trace = String::from_utf8(fixture::read(&fixture.home.join("argv")).unwrap()).unwrap();
        assert_eq!(
            trace.lines().filter(|v| *v == "--call--").count(),
            if late { 4 } else { 0 }
        );
        assert_eq!(before, fixture.snapshot().unwrap());
    }
}
