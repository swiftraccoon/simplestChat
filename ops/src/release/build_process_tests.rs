//! Actual inert Unix children exercise the production release dispatch owner.

use super::*;
use std::{
    fs::{self, DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "release-native-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn log(&self) -> File {
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.0.join("command.log"))
            .unwrap()
    }
    fn request(&self, script: &str, capture: bool) -> Invocation {
        Invocation {
            role: Role::Git,
            program: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".into(), script.into()],
            cwd: self.0.clone(),
            environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            body_end: Instant::now() + Duration::from_secs(3),
            log: self.log(),
            archive: None,
            capture,
            allow_failure: false,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
fn owner() -> Owner {
    Owner::new(
        Instant::now(),
        crate::release::build::control::Mode::Export,
        60,
    )
    .unwrap()
}
fn settled(observation: &Observation) {
    assert!(observation.group_retired);
    assert!(observation.reaped);
    assert!(observation.stdout_closed);
    assert!(observation.stderr_closed);
    assert!(observation.log_closed);
    assert!(observation.archive_closed);
}
fn failed(completion: &Completion, failure: Failure) {
    assert!(matches!(&completion.result, Err(actual) if *actual == failure));
}

#[test]
fn original_pipe_bytes_and_checked_log_seal_are_retained() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut owner = owner();
        let mut request = fixture.request("printf out; printf err >&2", true);
        request.role = Role::Docker;
        let completion = run(&mut owner, request).await;
        settled(&completion.observation);
        assert!(completion.observation.stdout_eof && completion.observation.stderr_eof);
        assert_eq!(completion.observation.exit_status, Some(0));
        assert_eq!(completion.observation.stdout_bytes, 3);
        assert_eq!(completion.observation.stderr_bytes, 3);
        assert_eq!(
            completion.observation.stdout_sha256,
            hash(&Sha256::new_with_prefix(b"out"))
        );
        assert_eq!(
            completion.observation.stderr_sha256,
            hash(&Sha256::new_with_prefix(b"err"))
        );
        let report = completion.result.unwrap();
        assert_eq!(report.status, 0);
        let retained = fs::read(fixture.0.join("command.log")).unwrap();
        assert_eq!(report.inspection, retained);
        assert_eq!(report.log.bytes, 6);
        assert_eq!(report.log.metadata.len(), 6);
        assert_eq!(report.log.sha256, hash(&Sha256::new_with_prefix(&retained)));
        assert_eq!(owner.primary(), None);
    });
}

fn archive(fixture: &Fixture) -> File {
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(fixture.0.join("image.tar"))
        .unwrap()
}

#[test]
fn binary_save_stdout_exceeds_diagnostic_limit_and_preserves_original_inode() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let original = archive(&fixture);
        let admitted = original.metadata().unwrap();
        let mut request = fixture.request(
            "dd if=/dev/zero bs=65536 count=48 2>/dev/null; printf diagnostic >&2",
            false,
        );
        request.role = Role::Docker;
        request.archive = Some(original.try_clone().unwrap());
        let mut owner = owner();
        let completion = run(&mut owner, request).await;
        let original_metadata = original.metadata();
        let bytes = fs::read(fixture.0.join("image.tar"));
        let log_bytes = fs::read(fixture.0.join("command.log"));
        let original_closed = nix::unistd::close(original);
        original_closed.unwrap();
        settled(&completion.observation);
        assert!(completion.observation.stdout_eof && completion.observation.stderr_eof);
        assert_eq!(completion.observation.stdout_bytes, 48 * 65536);
        let retained = completion.observation.archive.as_ref().unwrap();
        assert_eq!(retained.bytes, 48 * 65536);
        assert_eq!(retained.metadata.ino(), admitted.ino());
        assert_eq!(retained.metadata.dev(), admitted.dev());
        assert_eq!(retained.metadata.ino(), original_metadata.unwrap().ino());
        let bytes = bytes.unwrap();
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(bytes.len() as u64, retained.bytes);
        assert_eq!(retained.sha256, hash(&Sha256::new_with_prefix(&bytes)));
        assert_eq!(retained.sha256, completion.observation.stdout_sha256);
        let report = completion.result.unwrap();
        assert_eq!(report.log.bytes, 10);
        assert!(report.inspection.is_empty());
        assert_eq!(log_bytes.unwrap(), b"diagnostic");
    });
}

#[test]
fn archive_limit_bounds_each_write_before_it_reaches_the_original() {
    let fixture = Fixture::new();
    let mut streams = Streams::new(false);
    streams.archive = Some(archive(&fixture));
    let mut observation = Observation::new();
    observation.stdout_bytes = crate::release::archive::MAX_ARCHIVE_BYTES - 2;
    let mut log = fixture.log();
    let overflow = streams.accept(true, b"abc", &mut log, &mut observation);
    let peer_write = streams.accept(false, b"diagnostic", &mut log, &mut observation);
    let archive_bytes = fs::read(fixture.0.join("image.tar"));
    let log_bytes = fs::read(fixture.0.join("command.log"));
    let archive_closed = nix::unistd::close(streams.archive.take().unwrap());
    let log_closed = nix::unistd::close(log);
    archive_closed.unwrap();
    log_closed.unwrap();
    assert_eq!(overflow, Err(Failure::Bounds));
    assert_eq!(
        observation.stdout_bytes,
        crate::release::archive::MAX_ARCHIVE_BYTES
    );
    assert!(observation.output_limited && streams.stdout_stopped);
    assert!(!streams.stderr_stopped);
    assert_eq!(archive_bytes.unwrap(), b"ab");
    peer_write.unwrap();
    assert_eq!(log_bytes.unwrap(), b"diagnostic");
    assert!(streams.inspection.is_empty());
}

#[test]
fn archive_metadata_refusal_precedes_close_failure_and_log_peer_still_finishes() {
    let runtime = runtime();
    let _entered = runtime.enter();
    let fixture = Fixture::new();
    let mut streams = Streams::new(false);
    let mut file = archive(&fixture);
    file.write_all(b"unobserved byte").unwrap();
    streams.archive = Some(file);
    let mut observation = Observation::new();
    observation.archive_closed = false;
    let mut owner = owner();
    let log = fixture.log();
    let faults = Faults {
        archive_close: true,
        ..Faults::default()
    };
    finish_archive(&mut owner, &mut streams, &mut observation, &faults);
    finish_log(&mut owner, log, &streams, &mut observation, &faults);
    assert_eq!(owner.primary(), Some(Failure::Io));
    assert_eq!(
        observation.cleanup_errors,
        vec![Failure::Io, Failure::Close]
    );
    assert!(!observation.archive_closed);
    assert!(observation.archive.is_none());
    assert!(observation.log_closed && observation.log.is_some());
}

#[test]
fn failed_save_retains_partial_archive_and_settles_every_admitted_peer() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut request = fixture.request("printf partial; printf failed >&2; exit 7", false);
        request.role = Role::Docker;
        request.archive = Some(archive(&fixture));
        let mut owner = owner();
        let completion = run(&mut owner, request).await;
        failed(&completion, Failure::Command);
        settled(&completion.observation);
        assert_eq!(completion.observation.exit_status, Some(7));
        assert_eq!(completion.observation.archive.as_ref().unwrap().bytes, 7);
        assert_eq!(fs::read(fixture.0.join("image.tar")).unwrap(), b"partial");
        assert_eq!(fs::read(fixture.0.join("command.log")).unwrap(), b"failed");
    });
}

#[test]
fn failed_archive_write_still_closes_log_pipes_and_child() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        nix::unistd::close(archive(&fixture)).unwrap();
        let mut request = fixture.request("printf partial; printf diagnostic >&2", false);
        request.role = Role::Docker;
        request.archive = Some(File::open(fixture.0.join("image.tar")).unwrap());
        let mut owner = owner();
        let completion = run(&mut owner, request).await;
        failed(&completion, Failure::Io);
        settled(&completion.observation);
        assert!(completion.observation.archive.is_none());
        assert!(completion.observation.exit_status.is_some());
        assert!(completion.observation.log.is_some());
    });
}

#[test]
fn archive_close_refusal_cannot_pass_or_replace_an_earlier_child_failure() {
    runtime().block_on(async {
        for (script, status, expected) in [
            ("printf archive", 0, Failure::Close),
            ("printf archive; exit 7", 7, Failure::Command),
        ] {
            let fixture = Fixture::new();
            let mut request = fixture.request(script, false);
            request.role = Role::Docker;
            request.archive = Some(archive(&fixture));
            let mut owner = owner();
            let completion = run_with(
                &mut owner,
                request,
                Faults {
                    archive_close: true,
                    ..Faults::default()
                },
            )
            .await;
            failed(&completion, expected);
            assert_eq!(completion.observation.exit_status, Some(status));
            assert!(completion.observation.group_retired && completion.observation.reaped);
            assert!(completion.observation.stdout_closed && completion.observation.stderr_closed);
            assert!(completion.observation.log_closed);
            assert!(!completion.observation.archive_closed);
            assert!(completion.observation.archive.is_some());
            assert!(
                completion
                    .observation
                    .cleanup_errors
                    .contains(&Failure::Close)
            );
        }
    });
}

#[test]
fn pending_read_drop_resumes_original_pipe_bytes_without_a_false_eof() {
    use std::{future::Future, os::fd::OwnedFd, task::Poll};

    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut owner = owner();
        let mut log = fixture.log();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                concat!(
                    "IFS= read -r gate || exit 9; ",
                    "printf after-cancel; printf peer-after-cancel >&2",
                ),
            ])
            .current_dir(&fixture.0)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut group = Group::new(command.spawn().unwrap());
        drop(command);
        let pid = group.pid();
        let mut input = group.child().stdin.take().unwrap();
        let mut streams = Streams::new(true);
        streams.stdout = Some(ChildStdout::from_std(group.child().stdout.take().unwrap()).unwrap());
        streams.stderr = Some(ChildStderr::from_std(group.child().stderr.take().unwrap()).unwrap());
        let mut observation = Observation::new();
        let mut pending_observed = false;
        let mut read_failure = None;
        let pending_end = Instant::now() + Duration::from_secs(1);
        while Instant::now() < pending_end {
            let polled = {
                let mut future = Box::pin(streams.read_one(&mut log, &mut observation));
                let polled = std::future::poll_fn(|context| {
                    Poll::Ready(match future.as_mut().poll(context) {
                        Poll::Pending => None,
                        Poll::Ready(value) => Some(value),
                    })
                })
                .await;
                // This is the cancellation under test. The gated child has not
                // emitted either stream, and the same Streams is reused below.
                drop(future);
                polled
            };
            match polled {
                None => {
                    pending_observed = true;
                    break;
                }
                Some(Err(error)) => {
                    read_failure = Some(error);
                    break;
                }
                Some(Ok(())) => {} // A scheduler-delayed timer tick may finish first.
            }
        }
        let eof_before_release = observation.stdout_eof || observation.stderr_eof;
        let released = input.write_all(b"go\n");
        let input: OwnedFd = input.into();
        let input_closed = nix::unistd::close(input);
        let end = Instant::now() + Duration::from_secs(5);
        while !streams.complete(&observation) && Instant::now() < end {
            if let Err(error) = streams.read_one(&mut log, &mut observation).await {
                read_failure.get_or_insert(error);
                break;
            }
        }
        let leader = tokio::time::timeout_at(
            tokio::time::Instant::from_std(end),
            owned_process::exited(pid),
        )
        .await;
        observation.group_retired = owned_process::finish_before(pid, end).await;
        let reaped = group.reap().await;
        observation.reaped = reaped.is_ok();
        observation.exit_status = reaped
            .as_ref()
            .ok()
            .and_then(|status| status.code().or_else(|| status.signal().map(|n| -n)));
        close_pipes(
            &mut owner,
            &mut streams,
            &mut observation,
            &Faults::default(),
        );
        finish_log(
            &mut owner,
            log,
            &streams,
            &mut observation,
            &Faults::default(),
        );
        // Assert only after the admitted input, group, readers and log finalize.
        assert!(pending_observed);
        assert!(!eof_before_release);
        assert!(released.is_ok() && input_closed.is_ok());
        assert!(read_failure.is_none());
        assert!(matches!(leader, Ok(Ok(()))));
        settled(&observation);
        assert_eq!(observation.exit_status, Some(0));
        assert!(observation.stdout_eof && observation.stderr_eof);
        assert_eq!(observation.stdout_bytes, b"after-cancel".len() as u64);
        assert_eq!(observation.stderr_bytes, b"peer-after-cancel".len() as u64);
        assert_eq!(
            hash(&streams.out_hash),
            hash(&Sha256::new_with_prefix(b"after-cancel"))
        );
        assert_eq!(
            hash(&streams.err_hash),
            hash(&Sha256::new_with_prefix(b"peer-after-cancel"))
        );
        let seal = observation.log.unwrap();
        assert_eq!(
            seal.bytes,
            observation.stdout_bytes + observation.stderr_bytes
        );
        assert_eq!(
            seal.sha256,
            hash(&Sha256::new_with_prefix(&streams.inspection))
        );
        assert_eq!(streams.out_buffer.len(), 65536);
        assert_eq!(streams.err_buffer.len(), 65536);
    });
}

#[test]
fn independent_stream_limits_allow_four_mib_uncaptured_log_with_backpressure() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let script = concat!(
            "dd if=/dev/zero bs=65536 count=32 2>/dev/null & ",
            "dd if=/dev/zero bs=65536 count=32 1>&2 2>/dev/null; wait",
        );
        let mut owner = owner();
        let completion = run(&mut owner, fixture.request(script, false)).await;
        settled(&completion.observation);
        assert_eq!(completion.observation.stdout_bytes, STREAM_LIMIT);
        assert_eq!(completion.observation.stderr_bytes, STREAM_LIMIT);
        let report = completion.result.unwrap();
        assert!(report.inspection.is_empty());
        assert_eq!(report.log.bytes, 2 * STREAM_LIMIT);
        assert_eq!(report.log.metadata.len(), 2 * STREAM_LIMIT);
    });
}

#[test]
fn captured_combined_limit_does_not_replace_either_live_stream_limit() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let script = concat!(
            "dd if=/dev/zero bs=65536 count=20 2>/dev/null & ",
            "dd if=/dev/zero bs=65536 count=20 1>&2 2>/dev/null; wait",
        );
        let mut owner = owner();
        let completion = run(&mut owner, fixture.request(script, true)).await;
        failed(&completion, Failure::Bounds);
        settled(&completion.observation);
        assert!(completion.observation.inspection_limited);
        assert!(!completion.observation.output_limited);
        assert_eq!(completion.observation.stdout_bytes, 20 * 65536);
        assert_eq!(completion.observation.stderr_bytes, 20 * 65536);
        assert_eq!(completion.observation.log.unwrap().bytes, 40 * 65536);
    });
}

#[test]
fn one_stream_overflow_is_bounded_before_log_and_retirement_still_closes_its_peer() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut owner = owner();
        let completion = run(
            &mut owner,
            fixture.request("dd if=/dev/zero bs=65536 count=33 2>/dev/null", false),
        )
        .await;
        failed(&completion, Failure::Bounds);
        settled(&completion.observation);
        assert!(completion.observation.output_limited);
        assert_eq!(completion.observation.stdout_bytes, STREAM_LIMIT);
        assert_eq!(completion.observation.log.unwrap().bytes, STREAM_LIMIT);
    });
}

#[test]
fn failed_private_log_write_still_attempts_both_pipes_and_reap() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut request = fixture.request("printf out; printf err >&2", true);
        nix::unistd::close(request.log).unwrap();
        request.log = File::open(fixture.0.join("command.log")).unwrap();
        let mut owner = owner();
        let completion = run(&mut owner, request).await;
        failed(&completion, Failure::Io);
        settled(&completion.observation);
        assert!(completion.observation.log.is_none());
        assert!(completion.observation.exit_status.is_some());
    });
}

#[test]
fn a_real_nonzero_reap_precedes_a_later_pipe_close_failure() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut owner = owner();
        let completion = run_with(
            &mut owner,
            fixture.request("exit 7", true),
            Faults {
                stdout_close: true,
                ..Faults::default()
            },
        )
        .await;
        failed(&completion, Failure::Command);
        assert_eq!(owner.primary(), Some(Failure::Command));
        assert_eq!(completion.observation.exit_status, Some(7));
        assert!(completion.observation.reaped && completion.observation.group_retired);
        assert!(!completion.observation.stdout_closed);
        assert!(completion.observation.stderr_closed && completion.observation.log_closed);
        assert!(
            completion
                .observation
                .cleanup_errors
                .contains(&Failure::Close)
        );
    });
}

#[test]
fn a_zero_status_and_seal_cannot_override_failed_log_close() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut owner = owner();
        let completion = run_with(
            &mut owner,
            fixture.request("printf retained", true),
            Faults {
                log_close: true,
                ..Faults::default()
            },
        )
        .await;
        failed(&completion, Failure::Close);
        assert_eq!(completion.observation.exit_status, Some(0));
        assert!(completion.observation.log.is_some());
        assert!(!completion.observation.log_closed);
        assert!(completion.observation.stdout_closed && completion.observation.stderr_closed);
    });
}

#[test]
fn subordinate_signal_and_owner_style_exit_codes_remain_child_failures() {
    runtime().block_on(async {
        for (script, status, allow_failure) in [
            ("kill -TERM $$", -15, false),
            ("exit 124", 124, false),
            ("exit 130", 130, false),
            ("exit 143", 143, true),
        ] {
            let fixture = Fixture::new();
            let mut request = fixture.request(script, true);
            request.allow_failure = allow_failure;
            let mut owner = owner();
            let completion = run(&mut owner, request).await;
            settled(&completion.observation);
            assert_eq!(completion.observation.exit_status, Some(status));
            if allow_failure {
                assert_eq!(completion.result.unwrap().status, status);
                assert_eq!(owner.primary(), None);
            } else {
                failed(&completion, Failure::Command);
                assert_eq!(owner.primary(), Some(Failure::Command));
            }
        }
    });
}

#[test]
fn body_deadline_retires_quiet_child_without_replacing_the_original_cause() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut request = fixture.request("sleep 10", true);
        request.body_end = Instant::now() + Duration::from_millis(50);
        let mut owner = owner();
        let completion = run(&mut owner, request).await;
        failed(&completion, Failure::Deadline);
        settled(&completion.observation);
        assert!(completion.observation.timed_out);
        assert_eq!(completion.observation.exit_status, Some(-15));
        assert!(completion.observation.elapsed < Duration::from_secs(5));
    });
}

#[test]
fn exited_leader_is_reserved_until_its_pipe_holding_descendant_is_retired() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut request = fixture.request("sleep 20 & exit 0", true);
        request.body_end = Instant::now() + Duration::from_millis(50);
        let mut owner = owner();
        let completion = run(&mut owner, request).await;
        failed(&completion, Failure::Deadline);
        settled(&completion.observation);
        assert_eq!(completion.observation.exit_status, Some(0));
        assert!(completion.observation.stdout_eof && completion.observation.stderr_eof);
        assert!(completion.observation.elapsed < Duration::from_secs(5));
    });
}

#[test]
fn refusal_and_spawn_failure_close_admitted_log_without_a_child_claim() {
    runtime().block_on(async {
        for case in ["primary", "late", "missing", "mode"] {
            let fixture = Fixture::new();
            let mut request = fixture.request("printf forbidden > effect", true);
            let mut owner = owner();
            let expected = match case {
                "primary" => {
                    owner.record(Failure::Guard);
                    Failure::Guard
                }
                "late" => {
                    request.body_end = owner.end();
                    Failure::Deadline
                }
                "missing" => {
                    request.program = fixture.0.join("missing-executable");
                    Failure::Io
                }
                "mode" => {
                    fs::set_permissions(
                        fixture.0.join("command.log"),
                        fs::Permissions::from_mode(0o644),
                    )
                    .unwrap();
                    Failure::Input
                }
                _ => unreachable!(),
            };
            let completion = run(&mut owner, request).await;
            failed(&completion, expected);
            assert!(completion.observation.pid.is_none());
            assert!(completion.observation.exit_status.is_none());
            assert!(completion.observation.log_closed);
            assert!(!fixture.0.join("effect").exists());
        }
    });
}

#[test]
fn inspection_preserves_original_non_utf8_bytes_for_operation_validation() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut owner = owner();
        let completion = run(&mut owner, fixture.request("printf '\\377'", true)).await;
        settled(&completion.observation);
        assert_eq!(completion.result.unwrap().inspection, [255]);
    });
}

async fn native_signal_case(signal: &str) {
    let fixture = Fixture::new();
    let mut request = fixture.request("unused", true);
    request.program = std::env::current_exe().unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    request.arguments = vec![
        "--ignored".into(),
        "--exact".into(),
        format!("{module}::signal_worker").into(),
        "--nocapture".into(),
    ];
    request
        .environment
        .insert("RELEASE_NATIVE_SIGNAL_CASE".into(), signal.into());
    request.body_end = Instant::now() + Duration::from_secs(10);
    let mut owner = owner();
    let completion = run(&mut owner, request).await;
    settled(&completion.observation);
    let retained = fs::read(fixture.0.join("command.log")).unwrap();
    assert!(
        completion.result.is_ok(),
        "{}",
        String::from_utf8_lossy(&retained)
    );
    assert_eq!(completion.observation.exit_status, Some(0));
}
#[test]
fn original_native_interrupt_is_observed_in_an_isolated_test_process() {
    runtime().block_on(native_signal_case("INT"));
}
#[test]
fn original_native_terminate_is_observed_in_an_isolated_test_process() {
    runtime().block_on(native_signal_case("TERM"));
}
#[test]
fn native_deadline_and_retirement_complete_on_the_default_worker_stack() {
    runtime().block_on(native_signal_case("DEFAULT_STACK"));
}
#[test]
fn original_native_interrupt_is_observed_during_retained_archive_hashing() {
    runtime().block_on(native_signal_case("HASH_INT"));
}
#[test]
fn original_native_terminate_is_observed_during_retained_archive_hashing() {
    runtime().block_on(native_signal_case("HASH_TERM"));
}

fn retained_hash_signal_worker(signal: nix::sys::signal::Signal, expected: Failure) {
    use std::io::Seek;

    const BYTES: u64 = 64 * 1024 * 1024;
    let fixture = Fixture::new();
    let mut owner = owner();
    let end = owner.end();
    let revision = "a".repeat(40);
    let manifest = crate::release::manifest::Manifest::decode(
        &serde_json::to_vec(&serde_json::json!({
            "schemaVersion":1,
            "revision":revision,
            "platform":"linux/amd64",
            "archiveSha256":"0".repeat(64),
            "imageTag":format!("simplestchat-release/production:{revision}"),
            "migrations":{"1":"b".repeat(96)},
            "createdAt":"2026-10-24T00:00:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
    let mut original = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(fixture.0.join("retained-hash.tar"))
        .unwrap();
    let before = match original.set_len(BYTES).and_then(|()| original.metadata()) {
        Ok(before) => before,
        Err(error) => {
            let closed = nix::unistd::close(original);
            assert!(closed.is_ok(), "failed setup still closes original file");
            panic!("retained hash fixture setup failed: {error}");
        }
    };
    let mut checkpoints = 0_u64;
    let mut signal_result = None;
    let verified =
        crate::release::archive::verify_retained_archive(&mut original, &manifest, || {
            checkpoints += 1;
            if checkpoints == 50 {
                signal_result = Some(nix::sys::signal::kill(nix::unistd::Pid::this(), signal));
                if !matches!(signal_result.as_ref(), Some(Ok(()))) {
                    return false;
                }
            }
            owner.before(end).is_ok()
        });
    let after = original.metadata();
    let consumed = original.stream_position();
    let closed = nix::unistd::close(original);
    // Every assertion follows the original descriptor's explicit close attempt.
    assert!(closed.is_ok());
    assert!(matches!(signal_result, Some(Ok(()))));
    assert!(verified.is_err());
    assert_eq!(owner.primary(), Some(expected));
    assert!(checkpoints >= 50);
    let consumed = consumed.unwrap();
    assert!(
        consumed > 0 && consumed < BYTES,
        "interrupted inside the hash pass"
    );
    assert!(super::super::files::same(&before, &after.unwrap()));
    assert_eq!(before.len(), BYTES);
}

#[test]
#[ignore = "subprocess fixture invoked by the owning native and retained-hash signal tests"]
fn signal_worker() {
    let signal =
        std::env::var("RELEASE_NATIVE_SIGNAL_CASE").expect("owning subprocess test selector");
    let expected = match signal.as_str() {
        "INT" | "HASH_INT" => Failure::Interrupt,
        "TERM" | "HASH_TERM" => Failure::Terminate,
        "DEFAULT_STACK" => Failure::Deadline,
        _ => panic!("closed native signal selector"),
    };
    runtime().block_on(async {
        if signal == "DEFAULT_STACK" {
            // The owning Native subprocess clears its environment, so the
            // libtest worker has its ordinary stack rather than an override.
            assert!(std::env::var_os("RUST_MIN_STACK").is_none());
            let fixture = Fixture::new();
            let mut owner = owner();
            let mut request = fixture.request("sleep 10", true);
            request.body_end = Instant::now() + Duration::from_millis(100);
            let completion = run(&mut owner, request).await;
            failed(&completion, Failure::Deadline);
            settled(&completion.observation);
            assert_eq!(completion.observation.exit_status, Some(-15));
            assert!(completion.observation.timed_out);
            return;
        }
        if matches!(signal.as_str(), "HASH_INT" | "HASH_TERM") {
            let signal = if signal == "HASH_INT" {
                nix::sys::signal::Signal::SIGINT
            } else {
                nix::sys::signal::Signal::SIGTERM
            };
            retained_hash_signal_worker(signal, expected);
            return;
        }
        let fixture = Fixture::new();
        let mut owner = owner();
        let completion = run(
            &mut owner,
            fixture.request(&format!("kill -{signal} $PPID; sleep 10"), true),
        )
        .await;
        failed(&completion, expected);
        assert_eq!(owner.primary(), Some(expected));
        settled(&completion.observation);
        assert_eq!(completion.observation.exit_status, Some(-15));
        assert!(!completion.observation.timed_out);
    });
}
