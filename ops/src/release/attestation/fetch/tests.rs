//! Real owned inert children/files: no GH, HTTP, SSH service or target effects.
use super::super::acquire::tests::fixture as fresh;
use super::*;
#[path = "fixture.rs"]
mod fixture;
use std::{fs, os::unix::fs::PermissionsExt};
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
fn options(fixture: &fixture::Fixture, cached: bool) -> Options {
    let mut options = Options::parse(&fixture.arguments(cached)).unwrap().unwrap();
    options.environment = Some(fixture.inner.environment());
    options
}
fn observed(
    fixture: &fixture::Fixture,
    options: &Options,
) -> (serde_json::Value, Option<Cause>, (usize, usize)) {
    let runtime = runtime();
    let (bytes, cause, closes) = runtime.block_on(async {
        let start = Instant::now();
        let mut owner = Owner::new(start, options.local.whole, options.local.reserve).unwrap();
        files::start_probe(None);
        let bytes = execute(options, start, &mut owner).await;
        (bytes, owner.error(), files::take_probe())
    });
    drop(runtime);
    let _ = fixture;
    (serde_json::from_slice(&bytes).unwrap(), cause, closes)
}
#[test]
fn whole_cached_and_downloaded_paths_bind_new_grant_only_after_actual_ready() {
    for cached in [false, true] {
        let fixture = fixture::Fixture::new();
        let options = options(&fixture, cached);
        let before = fixture.inner.snapshot();
        let (report, cause, closes) = observed(&fixture, &options);
        let after = fixture.inner.snapshot();
        let argv = fixture.inner.read("argv");
        let grant = fixture.inner.read("grant-observed");
        let attempts = fixture.inner.attempts();
        let attempt_facts = attempts.as_ref().ok().and_then(|v| v.first()).map(|path| {
            (
                fs::read(path.join("artifact.json")),
                fs::read(path.join("outcome.json")),
                path.join("verified").is_dir(),
            )
        });
        drop(fixture);
        assert_eq!(cause, None, "report {report}");
        assert!(report["passed"].as_bool().unwrap());
        assert_eq!(report["phase"], "complete");
        assert_eq!(report["remoteSettled"], true);
        assert!(closes.0 > 0 && closes.1 == 0);
        assert_eq!(before.unwrap(), after.unwrap());
        assert_eq!(attempts.unwrap().len(), 1);
        let facts = attempt_facts.unwrap();
        assert!(facts.0.is_ok() && facts.1.is_ok());
        assert_eq!(facts.2, !cached);
        assert_eq!(
            String::from_utf8(argv.unwrap())
                .unwrap()
                .matches("auth\ntoken")
                .count(),
            if cached { 1 } else { 2 }
        );
        assert!(String::from_utf8(grant.unwrap()).unwrap().contains("INERT"));
    }
}
#[test]
fn preflight_failure_retains_receipt_without_requesting_any_remote_grant() {
    let fixture = fixture::Fixture::new();
    let mut receipt = fixture.receipt();
    for (key, value) in [
        ("passed", false.into()),
        ("settled", false.into()),
        ("finalized", false.into()),
        ("evidence", serde_json::Value::Null),
        ("archiveSha256", serde_json::Value::Null),
        ("manifestSha256", serde_json::Value::Null),
        ("downloadedBytes", 0.into()),
        ("status", "failed".into()),
        ("phase", "preflight".into()),
        ("failureClass", "preflight_refused".into()),
    ] {
        receipt[key] = value;
    }
    fresh::write(
        &fixture.inner.home.join("receipt"),
        &serde_json::to_vec(&receipt).unwrap(),
        0o600,
    )
    .unwrap();
    fixture
        .script("IFS= read -r envelope || exit 92\n/bin/cat \"$HOME/receipt\"\nprintf '\\n'\n")
        .unwrap();
    let options = options(&fixture, true);
    let (report, cause, closes) = observed(&fixture, &options);
    let argv = fixture.inner.read("argv");
    let grant_exists = fixture.inner.home.join("grant-observed").exists();
    drop(fixture);
    assert_eq!(cause, Some(Cause::Command));
    assert_eq!(report["remote"], receipt);
    assert_eq!(report["passed"], false);
    assert_eq!(report["remoteSettled"], false);
    assert!(!grant_exists);
    assert!(
        !String::from_utf8(argv.unwrap())
            .unwrap()
            .contains("auth\ntoken")
    );
    assert!(closes.0 > 0 && closes.1 == 0);
}
#[test]
fn current_exit_and_natural_trailing_eof_are_independent_of_valid_receipt() {
    for (tail, status, valid) in [
        (" \\t\\r\\n\\013\\014", 0, true),
        ("extra", 0, false),
        ("", 7, false),
    ] {
        let fixture = fixture::Fixture::new();
        fixture
            .script(&format!(
                concat!(
                    "IFS= read -r envelope || exit 92\n",
                    "printf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"ready\"}}'\n",
                    "IFS= read -r grant || exit 93\n/bin/cat \"$HOME/receipt\"\n",
                    "printf '\\n{tail}'\nexit {status}\n"
                ),
                tail = tail,
                status = status
            ))
            .unwrap();
        let options = options(&fixture, true);
        let (report, cause, closes) = observed(&fixture, &options);
        let journal_exists = fixture.inner.attempts().unwrap()[0]
            .join("outcome.json")
            .exists();
        drop(fixture);
        assert_eq!(report["passed"], valid);
        assert_eq!(cause.is_none(), valid, "{report}");
        assert!(journal_exists && closes.0 > 0 && closes.1 == 0);
        if status == 7 {
            assert_eq!(report["remoteSettled"], true);
        }
    }
}
#[test]
fn key_mode_and_present_empty_cache_refuse_before_effects() {
    let fixture = fixture::Fixture::new();
    let mut args = fixture.arguments(true);
    let index = args
        .iter()
        .position(|v| v == "--verified-directory")
        .unwrap()
        + 1;
    args[index] = "".into();
    let empty = Options::parse(&args).is_err();
    fs::set_permissions(
        fixture.inner.root.join("identity"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let options = options(&fixture, true);
    let (report, cause, closes) = observed(&fixture, &options);
    let attempts = fixture.inner.attempts();
    let argv = fixture.inner.read("argv");
    drop(fixture);
    assert!(empty);
    assert_eq!(cause, Some(Cause::Input));
    assert_eq!(report["phase"], "options");
    assert!(attempts.unwrap().is_empty() && argv.unwrap().is_empty());
    assert!(closes.0 > 0 && closes.1 == 0);
}
#[test]
fn exclusive_final_outcome_survives_primary_and_all_close_attempts() {
    let fixture = fixture::Fixture::new();
    let runtime = runtime();
    let facts = runtime.block_on(async {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .unwrap();
        files::start_probe(None);
        let directory = Dir::open(&fixture.inner.cwd, &mut owner).unwrap();
        owner.record(Cause::Policy);
        let first = directory.final_journal(b"{\"passed\":false}\n", &mut owner);
        let second = directory.final_journal(b"{}\n", &mut owner);
        let primary = owner.error();
        directory.finish(&mut owner);
        (first, second, primary, files::take_probe())
    });
    drop(runtime);
    let bytes = fs::read(fixture.inner.cwd.join("outcome.json"));
    drop(fixture);
    assert!(facts.0.is_ok() && facts.1.is_err());
    assert_eq!(facts.2, Some(Cause::Policy));
    assert!(facts.3.0 > 0 && facts.3.1 == 0);
    assert_eq!(bytes.unwrap(), b"{\"passed\":false}\n");
}
#[test]
fn frames_escape_unicode_without_rounding_private_numeric_lexemes() {
    let bytes = process::frame("{\"v\":\"😀\",\"n\":999999999999999999999999}".as_bytes()).unwrap();
    assert_eq!(
        bytes,
        b"{\"v\":\"\\ud83d\\ude00\",\"n\":999999999999999999999999}\n"
    );
    assert!(process::frame(&vec![b'x'; 16384]).is_err());
}
#[test]
fn receipt_receive_and_finish_suffix_keep_separate_reference_counters() {
    let fixture = fixture::Fixture::new();
    // Leave a full receive read of headroom for the reference's raw
    // 4096-byte read-ahead, which still counts toward its receive cap.
    let mut response = vec![b' '; 59000];
    response.extend(serde_json::to_vec(&fixture.receipt()).unwrap());
    response.push(b'\n');
    response.extend(vec![b' '; 64000]);
    fresh::write(&fixture.inner.home.join("wire"), &response, 0o600).unwrap();
    fixture
        .script(concat!(
            "IFS= read -r envelope || exit 92\n",
            "printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"ready\"}'\n",
            "IFS= read -r grant || exit 93\nexec /bin/cat \"$HOME/wire\"\n"
        ))
        .unwrap();
    let options = options(&fixture, true);
    let (report, cause, closes) = observed(&fixture, &options);
    drop(fixture);
    assert_eq!(cause, None, "{report}");
    assert_eq!(report["passed"], true);
    assert!(closes.0 > 0 && closes.1 == 0);
}
#[test]
fn report_failure_keeps_first_cause_when_final_journal_close_also_fails() {
    let fixture = fixture::Fixture::new();
    let runtime = runtime();
    let facts = runtime.block_on(async {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .unwrap();
        let directory = Dir::open(&fixture.inner.cwd, &mut owner).unwrap();
        files::start_probe(Some(1));
        owner.record(Cause::Policy);
        let result = directory.final_journal(b"{\"passed\":false}\n", &mut owner);
        directory.finish(&mut owner);
        (result, owner.error(), files::take_probe())
    });
    drop(runtime);
    drop(fixture);
    assert!(facts.0.is_err());
    assert_eq!(facts.1, Some(Cause::Policy));
    assert!(facts.2.0 > 1 && facts.2.1 == 0);
}
#[test]
fn actual_outer_runtime_and_umask_restore_precede_the_original_end_fence() {
    const MARKER: &str = "SIMPLESTCHAT_FETCH_LIFECYCLE_WORKER";
    if std::env::var_os(MARKER).is_some() {
        let start = Instant::now();
        let end = start + Duration::from_secs(30);
        let old = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
        let closed = std::cell::Cell::new(false);
        COMPLETED.set(Some(end));
        let code = lifecycle(start, Duration::from_secs(30), || {
            let owner = Owner::new(start, Duration::from_secs(30), Duration::from_secs(5)).unwrap();
            let runtime = runtime();
            runtime.block_on(async {});
            drop(runtime);
            closed.set(true);
            (true, Some(owner))
        });
        let restored = nix::sys::stat::umask(old);
        assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
        assert_eq!(code, ExitCode::FAILURE);
        assert!(closed.get());
        return;
    }
    let runtime = runtime();
    let completion = runtime.block_on(async {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .unwrap();
        let body = Instant::now() + Duration::from_secs(10);
        verify::process::run_small(
            &mut owner,
            verify::process::Invocation {
                program: std::env::current_exe().unwrap(),
                arguments: vec![
                    "--exact".into(),
                    concat!(
                        "release::attestation::fetch::tests::",
                        "actual_outer_runtime_and_umask_restore_precede_the_original_end_fence"
                    )
                    .into(),
                    "--nocapture".into(),
                ],
                environment: Environment::from([(MARKER.into(), "1".into())]),
                body_end: body,
                settlement_end: body + Duration::from_secs(10),
            },
        )
        .await
    });
    drop(runtime);
    assert!(completion.observation.group_retired && completion.observation.reaped);
    assert!(
        completion.observation.stdin_closed
            && completion.observation.stdout_closed
            && completion.observation.stderr_closed
    );
    assert!(completion.result.is_ok());
}
#[test]
fn admitted_ssh_deadline_refuses_receipt_and_still_closes_both_peers_and_group() {
    let fixture = fixture::Fixture::new();
    fixture.script("exec /bin/sleep 20\n").unwrap();
    let runtime = runtime();
    let facts = runtime.block_on(async {
        let start = Instant::now();
        let mut owner =
            Owner::new(start, Duration::from_secs(90), Duration::from_secs(10)).unwrap();
        let protocol_end = owner.body_end() - process::TAIL;
        let mut receiver = process::Receiver::start(
            process::Invocation {
                program: fixture.inner.bin.join("ssh"),
                arguments: Vec::new(),
                environment: ssh_environment(&fixture.inner.environment()),
                protocol_end,
            },
            &mut owner,
        )
        .await
        .unwrap();
        owner.observe_at(protocol_end);
        let result = receiver.receive(45, &mut owner).await;
        receiver.finish(&mut owner).await;
        (result, owner.error(), receiver.observation)
    });
    drop(runtime);
    drop(fixture);
    assert!(facts.0.is_err());
    assert_eq!(facts.1, Some(Cause::Deadline));
    assert!(
        facts.2.input_closed && facts.2.output_closed && facts.2.group_retired && facts.2.reaped
    );
    assert!(!facts.2.natural_eof);
}
#[test]
fn final_outcome_original_end_refusal_never_clears_primary_or_skips_other_closes() {
    let fixture = fixture::Fixture::new();
    let runtime = runtime();
    let facts = runtime.block_on(async {
        let start = Instant::now();
        let mut owner = Owner::new(start, Duration::from_secs(30), Duration::from_secs(5)).unwrap();
        files::start_probe(None);
        let directory = Dir::open(&fixture.inner.cwd, &mut owner).unwrap();
        owner.record(Cause::Policy);
        owner.observe_at(owner.end());
        let result = directory.final_journal(b"{}\n", &mut owner);
        directory.finish(&mut owner);
        (result, owner.error(), files::take_probe())
    });
    drop(runtime);
    let created = fixture.inner.cwd.join("outcome.json").exists();
    drop(fixture);
    assert_eq!(facts.0, Err(Cause::Deadline));
    assert_eq!(facts.1, Some(Cause::Policy));
    assert!(!created && facts.2.0 > 0 && facts.2.1 == 0);
}
#[test]
fn nonzero_leader_and_descendant_retirement_cannot_supply_missing_natural_eof() {
    let fixture = fixture::Fixture::new();
    fixture
        .script(concat!(
            "IFS= read -r envelope || exit 92\n",
            "printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"ready\"}'\n",
            "IFS= read -r grant || exit 93\n/bin/cat \"$HOME/receipt\"\nprintf '\\n'\n",
            "/bin/sleep 20 &\nexit 7\n"
        ))
        .unwrap();
    let options = options(&fixture, true);
    let (report, cause, closes) = observed(&fixture, &options);
    drop(fixture);
    assert_eq!(cause, Some(Cause::Deadline));
    assert_eq!(report["passed"], false);
    assert_eq!(report["remoteSettled"], false);
    assert!(report["remote"].is_object());
    assert!(closes.0 > 0 && closes.1 == 0);
}
#[test]
fn consumed_wait_failure_is_negative_and_never_retries_a_disarmed_group() {
    let fixture = fixture::Fixture::new();
    fixture.script("printf '{}\\n'\n").unwrap();
    let runtime = runtime();
    let facts = runtime.block_on(async {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(90),
            Duration::from_secs(10),
        )
        .unwrap();
        let protocol_end = owner.body_end() - process::TAIL;
        let mut receiver = process::Receiver::start(
            process::Invocation {
                program: fixture.inner.bin.join("ssh"),
                arguments: Vec::new(),
                environment: ssh_environment(&fixture.inner.environment()),
                protocol_end,
            },
            &mut owner,
        )
        .await
        .unwrap();
        let line = receiver.receive(45, &mut owner).await;
        receiver.faults.reap_result = true;
        let first = receiver.finish(&mut owner).await;
        let second = receiver.finish(&mut owner).await;
        (line, first, second, owner.error(), receiver.observation)
    });
    drop(runtime);
    drop(fixture);
    assert!(facts.0.is_ok());
    assert_eq!(facts.1, None);
    assert_eq!(facts.2, None);
    assert_eq!(facts.3, Some(Cause::Io));
    assert!(facts.4.finish_attempted && facts.4.reap_attempted && !facts.4.reaped);
    assert!(facts.4.group_retired && facts.4.input_closed && facts.4.output_closed);
}
#[test]
fn post_admission_witness_reads_cannot_spend_the_original_ssh_tail() {
    for point in [HashFault::AfterAdmission, HashFault::AfterReady] {
        let fixture = fixture::Fixture::new();
        let options = options(&fixture, true);
        let runtime = runtime();
        let facts = runtime.block_on(async {
            let start = Instant::now();
            let mut owner = Owner::new(start, options.local.whole, options.local.reserve).unwrap();
            files::start_probe(None);
            PROTOCOL_HASH_FAULT.set(Some(point));
            LAST_RECEIVER.with(|last| *last.borrow_mut() = None);
            let bytes = execute(&options, start, &mut owner).await;
            let remaining = owner.body_end().duration_since(owner.now());
            let observed = LAST_RECEIVER.with(|last| last.borrow_mut().take());
            (
                bytes,
                owner.error(),
                remaining,
                observed,
                files::take_probe(),
            )
        });
        drop(runtime);
        let calls = fixture.inner.read("argv");
        let grant = fixture.inner.home.join("grant-observed").exists();
        drop(fixture);
        assert_eq!(facts.1, Some(Cause::Deadline));
        assert_eq!(facts.2, process::TAIL);
        let observed = facts.3.unwrap();
        assert!(observed.finish_attempted && observed.reap_attempted && observed.reaped);
        assert!(observed.input_closed && observed.output_closed && observed.group_retired);
        assert!(facts.4.0 > 0 && facts.4.1 == 0);
        let report: serde_json::Value = serde_json::from_slice(&facts.0).unwrap();
        assert_eq!(report["passed"], false);
        assert_eq!(report["remote"], serde_json::Value::Null);
        assert_eq!(report["remoteSettled"], false);
        assert_eq!(report["failureClass"], "deadline_exceeded");
        assert!(!grant);
        assert!(
            !String::from_utf8(calls.unwrap())
                .unwrap()
                .contains("auth\ntoken")
        );
    }
}

#[test]
fn late_real_signals_after_runtime_restore_refuse_fetch_success() {
    use super::super::control::signal_tests::{self, Entry};
    let Some(expected) = signal_tests::worker(Entry::Fetch) else {
        return signal_tests::supervise(Entry::Fetch);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let fixture = fixture::Fixture::new();
    let options = options(&fixture, true);
    let started = Instant::now();
    let mut facts = None;
    let code = lifecycle(started, options.local.whole, || {
        let mut owner = Owner::new(started, options.local.whole, options.local.reserve).unwrap();
        let runtime = runtime();
        let result = runtime.block_on(async {
            files::start_probe(None);
            let bytes = execute(&options, started, &mut owner).await;
            if let Err(cause) = verify::stdout(&mut owner, &bytes) {
                owner.record(cause);
            }
            (
                owner.check(Window::Final).is_ok(),
                bytes,
                files::take_probe(),
            )
        });
        drop(runtime);
        let passed = result.0;
        facts = Some(result);
        (passed, Some(owner))
    });
    let receiver = LAST_RECEIVER.with(|r| r.borrow_mut().take());
    drop(fixture);
    let restored = nix::sys::stat::umask(previous);
    signal_tests::assert_finished(
        code,
        expected,
        nix::sys::stat::Mode::from_bits_truncate(0o027),
    );
    let (passed, emitted, probes) = facts.unwrap();
    assert!(passed);
    let report: serde_json::Value = serde_json::from_slice(&emitted).unwrap();
    assert_eq!(report["passed"], true);
    let receiver = receiver.unwrap();
    assert_eq!(receiver.exit, Some(0));
    assert!(
        receiver.group_retired
            && receiver.reaped
            && receiver.input_closed
            && receiver.output_closed
            && receiver.natural_eof
    );
    assert!(probes.0 > 0 && probes.1 == 0);
    assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
}
