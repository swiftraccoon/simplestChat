//! Actual private pipe fixtures, no TLS, credentials, API or network claims.
use super::*;
use crate::test_support::Fixture;
use std::{fs, future::Future, io::Write, task::Poll};
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
        Duration::from_secs(45),
        Duration::from_secs(5),
    )
    .unwrap()
}
fn request(owner: &Owner, script: &str, role: Role, body: Duration) -> Invocation {
    let body_end = owner.now() + body;
    Invocation {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
        environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        input: b"INERT CONFIG\n".to_vec(),
        role,
        body_end,
        settlement_end: body_end + TERM + REAP,
        expire_after: None,
    }
}
fn api(script: &str, faults: Faults, body: Duration) -> (Completion, Option<Cause>, bool) {
    let runtime = runtime();
    let result = runtime.block_on(async {
        let mut owner = owner();
        let request = request(&owner, script, Role::Api, body);
        let future = run_with(&mut owner, request, faults);
        let compact = std::mem::size_of_val(&future) < 32768;
        let completion = future.await;
        (completion, owner.error(), compact)
    });
    drop(runtime);
    result
}
fn peers(observed: &Observation) {
    assert!(observed.input_complete && observed.input_closed);
    assert!(observed.output_closed && observed.error_closed);
    assert!(observed.group_retired && observed.reaped);
}
#[test]
fn actual_valid_header_permission_retires_an_unread_live_body_without_zero_inference() {
    let (completion, cause, compact) = api(
        concat!(
            "/bin/cat >/dev/null; printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://a.blob.core.windows.net/p?q=v\\r\\n",
            "Content-Length: 999999\\r\\n\\r\\n'; exec /bin/sleep 60"
        ),
        Faults::default(),
        Duration::from_secs(2),
    );
    peers(&completion.observation);
    assert!(compact);
    assert_eq!(cause, None);
    assert!(completion.observation.planned_header_stop);
    assert!(!completion.observation.output_eof);
    assert!(
        completion
            .observation
            .exit_status
            .is_some_and(|status| status < 0)
    );
    assert_eq!(
        completion.result.unwrap().unwrap(),
        "https://a.blob.core.windows.net/p?q=v"
    );
}
#[test]
fn invalid_grant_is_primary_before_a_later_output_close_fault() {
    let (completion, cause, _) = api(
        concat!(
            "/bin/cat >/dev/null; printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://evil.example/p?q=v\\r\\n\\r\\n'; exec /bin/sleep 60"
        ),
        Faults {
            output_close: true,
            ..Faults::default()
        },
        Duration::from_secs(2),
    );
    assert_eq!(cause, Some(Cause::Policy));
    assert!(completion.result.is_err());
    assert!(!completion.observation.planned_header_stop);
    assert!(completion.observation.group_retired && completion.observation.reaped);
    assert!(completion.observation.input_closed && completion.observation.error_closed);
    assert!(!completion.observation.output_closed);
}
#[test]
fn cleanup_generated_eof_does_not_accept_a_missing_or_incomplete_api_header() {
    for script in [
        "/bin/cat >/dev/null; exec /bin/sleep 60",
        "/bin/cat >/dev/null; printf 'HTTP/1.1 302 Found\\r\\n'; exec /bin/sleep 60",
    ] {
        let (completion, cause, _) = api(script, Faults::default(), Duration::from_millis(80));
        peers(&completion.observation);
        assert_eq!(cause, Some(Cause::Deadline));
        assert!(completion.result.is_err());
        assert!(!completion.observation.planned_header_stop);
    }
}
fn blob_case(
    extra: &[u8],
    child_status: i32,
    expected: u64,
    faults: Faults,
    write_fault: bool,
) -> (Completion, Option<Cause>, Vec<u8>, (usize, usize)) {
    blob_case_framed(extra, child_status, expected, faults, write_fault, None)
}
fn blob_case_framed(
    extra: &[u8],
    child_status: i32,
    expected: u64,
    faults: Faults,
    write_fault: bool,
    length: Option<u64>,
) -> (Completion, Option<Cause>, Vec<u8>, (usize, usize)) {
    let fixture = Fixture::new();
    let mut wire = b"HTTP/1.1 200 OK\r\n".to_vec();
    if let Some(length) = length {
        wire.extend(format!("Content-Length: {length}\r\n").as_bytes());
    }
    wire.extend(b"\r\n");
    wire.extend(extra);
    let mut file = fs::File::create(fixture.0.join("wire")).unwrap();
    let written = file.write_all(&wire);
    let closed = nix::unistd::close(file);
    written.unwrap();
    closed.unwrap();
    let runtime = runtime();
    let result = runtime.block_on(async {
        let mut owner = if faults.late_output {
            Owner::new(
                Instant::now(),
                Duration::from_secs(300),
                Duration::from_secs(30),
            )
            .unwrap()
        } else {
            owner()
        };
        super::super::super::files::start_probe(None);
        let directory = Dir::open(&fixture.0, &mut owner).unwrap();
        if write_fault {
            super::super::super::files::copy_fault("artifact.zip".into(), false, 0);
        }
        let role = Role::Blob {
            directory: directory.clone(),
            expected,
            digest: super::super::super::files::hex(&Sha256::digest(extra)),
        };
        let body = if faults.late_output {
            Duration::from_secs(240)
        } else {
            Duration::from_secs(2)
        };
        let mut request = request(
            &owner,
            "/bin/cat >/dev/null; /bin/cat \"$1\"; exit \"$2\"",
            role,
            body,
        );
        request.arguments.extend([
            "fixture".into(),
            fixture.0.join("wire").into_os_string(),
            child_status.to_string().into(),
        ]);
        let mut completion = run_with(&mut owner, request, faults).await;
        if let Some(file) = completion.file.take() {
            file.finish(&mut owner);
        }
        directory.finish(&mut owner);
        let bytes = fs::read(fixture.0.join("artifact.zip")).unwrap_or_default();
        let closes = super::super::super::files::take_probe();
        (completion, owner.error(), bytes, closes)
    });
    drop(runtime);
    drop(fixture);
    result
}
#[test]
fn blob_size_digest_child_status_write_and_close_results_remain_independent() {
    for (payload, status, expected, fault, write_fault, primary) in [
        (b"payload".as_slice(), 0, 7, false, false, None),
        (
            b"payload".as_slice(),
            7,
            7,
            false,
            false,
            Some(Cause::Command),
        ),
        (
            b"payload".as_slice(),
            0,
            6,
            false,
            false,
            Some(Cause::Bounds),
        ),
        (b"payload".as_slice(), 0, 7, false, true, Some(Cause::Io)),
        (b"payload".as_slice(), 0, 7, true, false, Some(Cause::Close)),
        (
            b"payload".as_slice(),
            7,
            7,
            true,
            false,
            Some(Cause::Command),
        ),
    ] {
        let (completion, cause, bytes, closes) = blob_case(
            payload,
            status,
            expected,
            Faults {
                output_close: fault,
                ..Faults::default()
            },
            write_fault,
        );
        assert_eq!(cause, primary);
        assert_eq!(completion.result.is_ok(), primary.is_none());
        assert!(completion.observation.group_retired && completion.observation.reaped);
        assert!(completion.observation.input_closed && completion.observation.error_closed);
        assert!(closes.0 > 0 && closes.1 == 0);
        if primary.is_none() {
            assert_eq!(bytes, payload);
        }
    }
}
#[test]
fn large_blob_streaming_is_heap_bounded_and_uses_the_original_file() {
    let payload = vec![b'x'; 3 * 1024 * 1024];
    let (completion, cause, bytes, closes) =
        blob_case(&payload, 0, payload.len() as u64, Faults::default(), false);
    peers(&completion.observation);
    assert_eq!(cause, None);
    assert!(completion.result.is_ok());
    assert_eq!(bytes, payload);
    assert!(closes.0 > 0 && closes.1 == 0);
}
#[test]
fn cancelling_one_pending_step_keeps_both_buffers_and_resumes_the_same_pipes() {
    let runtime = runtime();
    let facts =
        runtime.block_on(async {
            let mut owner = owner();
            let request = request(&owner, "unused", Role::Api, Duration::from_secs(2));
            let mut command = Command::new("/bin/sh");
            command.args(["-c",concat!("IFS= read -r gate; printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://a.blob.core.windows.net/p?q=v\\r\\n\\r\\n'; printf peer >&2")])
            .process_group(0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut group = Group::new(command.spawn().unwrap());
            drop(command);
            let mut input = group.child().stdin.take().unwrap();
            let stdout = group.child().stdout.take().unwrap();
            let stderr = group.child().stderr.take().unwrap();
            let mut streams = Streams::new();
            let mut observed = Observation::default();
            match ChildStdout::from_std(stdout) {
                Ok(pipe) => streams.output = Some(pipe),
                Err(_) => owner.record(Cause::Io),
            }
            match ChildStderr::from_std(stderr) {
                Ok(pipe) => streams.error = Some(pipe),
                Err(_) => owner.record(Cause::Io),
            }
            let buffers = (
                streams.output_buffer.as_ptr(),
                streams.error_buffer.as_ptr(),
            );
            let (pending, compact) = {
                let mut future =
                    std::pin::pin!(streams.step(&request, &mut owner, &mut observed, false));
                let compact = std::mem::size_of_val(future.as_ref().get_ref()) < 32768;
                let pending =
                    std::future::poll_fn(|context| Poll::Ready(future.as_mut().poll(context)))
                        .await
                        .is_pending();
                (pending, compact)
            };
            let buffers_same = buffers
                == (
                    streams.output_buffer.as_ptr(),
                    streams.error_buffer.as_ptr(),
                );
            if input.write_all(b"go\n").is_err() {
                owner.record(Cause::Io);
            }
            let closed = nix::unistd::close(std::os::fd::OwnedFd::from(input)).is_ok();
            let end = owner.now() + Duration::from_secs(2);
            while owner.now() < end && !(observed.output_eof && observed.error_eof) {
                let result = streams
                    .step(&request, &mut owner, &mut observed, false)
                    .await;
                error(&mut owner, result);
            }
            let retired = owned_process::finish_before(group.pid(), end).await;
            let reaped = group.reap().await.is_ok();
            streams.close(&mut owner, &mut observed, &Faults::default());
            (
                pending,
                compact,
                buffers_same,
                closed,
                retired,
                reaped,
                streams.location,
                owner.error(),
            )
        });
    drop(runtime);
    assert!(facts.0 && facts.1 && facts.2 && facts.3 && facts.4 && facts.5);
    assert_eq!(
        facts.6.as_deref(),
        Some("https://a.blob.core.windows.net/p?q=v")
    );
    assert_eq!(facts.7, None);
}

fn signal_worker(interrupt: bool) {
    let fixture = Fixture::new();
    let marker = fixture.0.join("ready");
    let runtime = runtime();
    let (completion, cause, sent) = runtime.block_on(async {
        let mut owner = owner();
        let mut request = request(
            &owner,
            "/bin/cat >/dev/null; : > \"$1\"; exec /bin/sleep 60",
            Role::Api,
            Duration::from_secs(5),
        );
        request
            .arguments
            .extend(["fixture".into(), marker.clone().into_os_string()]);
        let end = request.body_end;
        let scheduler = tokio::spawn(async move {
            while Instant::now() < end {
                if marker.exists() {
                    return nix::sys::signal::kill(
                        nix::unistd::Pid::this(),
                        if interrupt {
                            nix::sys::signal::Signal::SIGINT
                        } else {
                            nix::sys::signal::Signal::SIGTERM
                        },
                    )
                    .is_ok();
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            false
        });
        let completion = run(&mut owner, request).await;
        let sent = scheduler.await;
        (completion, owner.error(), sent)
    });
    drop(runtime);
    drop(fixture);
    peers(&completion.observation);
    assert!(sent.unwrap());
    let expected = if interrupt {
        Cause::Interrupt
    } else {
        Cause::Terminate
    };
    assert_eq!(cause, Some(expected));
    assert!(matches!(completion.result,Err(cause) if cause==expected));
}
#[test]
fn actual_owned_interrupt_and_termination_settle_all_transport_peers() {
    const ROLE: &str = "SC_FRESH_TRANSPORT_SIGNAL";
    if let Some(role) = std::env::var_os(ROLE) {
        return signal_worker(role == "INT");
    }
    let program = std::env::current_exe().unwrap();
    let test = format!(
        "{}::actual_owned_interrupt_and_termination_settle_all_transport_peers",
        module_path!().split_once("::").unwrap().1
    );
    let mut facts = Vec::new();
    for role in ["INT", "TERM"] {
        let runtime = runtime();
        let result = runtime.block_on(async {
            let mut owner = owner();
            let body_end = owner.now() + Duration::from_secs(15);
            super::super::super::verify::process::run(
                &mut owner,
                super::super::super::verify::process::Invocation {
                    program: program.clone(),
                    arguments: vec![
                        "--exact".into(),
                        test.clone().into(),
                        "--nocapture".into(),
                        "--test-threads=1".into(),
                    ],
                    environment: BTreeMap::from([
                        ("PATH".into(), "/usr/bin:/bin".into()),
                        (ROLE.into(), role.into()),
                    ]),
                    body_end,
                    settlement_end: body_end + TERM + REAP,
                },
            )
            .await
        });
        drop(runtime);
        facts.push(result);
    }
    for completion in facts {
        assert!(completion.observation.group_retired && completion.observation.reaped);
        assert!(
            completion.observation.stdin_closed
                && completion.observation.stdout_closed
                && completion.observation.stderr_closed
        );
        assert!(completion.result.is_ok());
        assert_eq!(completion.observation.exit_status, Some(0));
    }
}
#[test]
fn stdin_backpressure_and_both_large_streams_are_drained_concurrently() {
    let fixture = Fixture::new();
    let runtime = runtime();
    let (mut completion, cause, bytes) = runtime.block_on(async {
        let mut owner = owner();
        let directory = Dir::open(&fixture.0, &mut owner).unwrap();
        let role = Role::Blob {
            directory: directory.clone(),
            expected: 300000,
            digest: super::super::super::files::hex(&Sha256::digest(vec![0; 300000])),
        };
        let mut request = request(
            &owner,
            concat!(
                "(/usr/bin/head -c 60000 /dev/zero >&2) & ",
                "printf 'HTTP/1.1 200 OK\\r\\n\\r\\n'; ",
                "/usr/bin/head -c 300000 /dev/zero; /bin/cat >/dev/null; wait"
            ),
            role,
            Duration::from_secs(3),
        );
        request.input = vec![b'x'; 32768];
        let mut completion = run(&mut owner, request).await;
        if let Some(file) = completion.file.take() {
            file.finish(&mut owner);
        }
        directory.finish(&mut owner);
        let bytes = fs::read(fixture.0.join("artifact.zip"));
        (completion, owner.error(), bytes)
    });
    drop(runtime);
    drop(fixture);
    peers(&completion.observation);
    assert_eq!(cause, None);
    assert!(completion.result.is_ok());
    assert_eq!(completion.observation.stderr_bytes, 60000);
    assert_eq!(bytes.unwrap().len(), 300000);
    assert!(completion.file.take().is_none());
}

#[test]
fn recoverable_read_and_write_interruptions_resume_the_actual_original_pipes() {
    let (completion, cause, compact) = api(
        concat!(
            "/bin/cat >/dev/null; ",
            "printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://a.blob.core.windows.net/p?q=v\\r\\n\\r\\n'"
        ),
        Faults {
            interruptions: true,
            ..Faults::default()
        },
        Duration::from_secs(2),
    );
    peers(&completion.observation);
    assert_eq!(cause, None, "safe pipe facts {:?}", completion.observation);
    assert!(compact);
    assert!(completion.result.is_ok());
}

#[test]
fn planned_api_stop_accepts_a_fast_natural_zero_and_still_retires_the_group() {
    let (completion, cause, compact) = api(
        concat!(
            "/bin/cat >/dev/null; ",
            "printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://a.blob.core.windows.net/p?q=v\\r\\n\\r\\n'; exit 0"
        ),
        Faults::default(),
        Duration::from_secs(2),
    );
    peers(&completion.observation);
    assert_eq!(cause, None, "{:?}", completion.observation);
    assert!(compact && completion.observation.planned_header_stop);
    assert!(completion.result.is_ok());
    assert!(matches!(
        completion.observation.exit_status,
        Some(0) | Some(-15)
    ));
}
#[test]
fn terminal_api_leader_does_not_leave_a_live_pipe_holding_descendant() {
    let (completion, cause, _) = api(
        concat!(
            "/bin/cat >/dev/null; ",
            "(/bin/sleep 60) & printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://a.blob.core.windows.net/p?q=v\\r\\n\\r\\n'; exit 0"
        ),
        Faults::default(),
        Duration::from_secs(2),
    );
    peers(&completion.observation);
    assert_eq!(cause, None, "{:?}", completion.observation);
    assert!(completion.result.is_ok());
    assert!(completion.observation.planned_header_stop);
}

#[test]
fn api_header_authority_survives_a_proven_natural_ignored_body_error() {
    let (completion, cause, _) = api(
        concat!(
            "/bin/cat >/dev/null; ",
            "printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://a.blob.core.windows.net/p?q=v\\r\\n",
            "Content-Length: 999999\\r\\n\\r\\nshort'; exit 18"
        ),
        Faults {
            terminal_before_stop: true,
            ..Faults::default()
        },
        Duration::from_secs(2),
    );
    peers(&completion.observation);
    assert_eq!(cause, None, "{:?}", completion.observation);
    assert!(completion.result.is_ok());
    assert!(completion.observation.planned_header_stop);
    assert_eq!(completion.observation.exit_status, Some(18));
}
#[test]
fn a_natural_nonzero_without_valid_api_headers_remains_negative_after_all_closes() {
    let (completion, cause, _) = api(
        "/bin/cat >/dev/null; exit 18",
        Faults::default(),
        Duration::from_secs(2),
    );
    peers(&completion.observation);
    assert_eq!(cause, Some(Cause::Command));
    assert!(completion.result.is_err());
    assert!(!completion.observation.planned_header_stop);
    assert_eq!(completion.observation.exit_status, Some(18));
}

#[test]
fn only_the_final_exact_group_proof_can_discharge_a_pending_permission_candidate() {
    for refused in [false, true] {
        let (completion, cause, _) = api(
            concat!(
                "/bin/cat >/dev/null; ",
                "printf 'HTTP/1.1 302 Found\\r\\n",
                "Location: https://a.blob.core.windows.net/p?q=v\\r\\n\\r\\n'; exit 0"
            ),
            Faults {
                permission_candidate: true,
                retirement_refusal: refused,
                ..Faults::default()
            },
            Duration::from_secs(2),
        );
        assert!(
            completion.observation.input_closed
                && completion.observation.output_closed
                && completion.observation.error_closed
                && completion.observation.reaped
        );
        assert!(completion.observation.permission_candidate);
        assert_eq!(completion.observation.group_retired, !refused);
        assert_eq!(completion.result.is_ok(), !refused);
        assert_eq!(cause, refused.then_some(Cause::Io));
        if refused {
            assert_eq!(
                completion.observation.first_io_stage,
                Some("term-permission-final-proof")
            );
        }
    }
}

#[test]
fn natural_truncated_content_length_eof_retains_the_short_tail_before_exit18() {
    for (expected, available) in [(3, 2), (65540, 65539)] {
        let payload = vec![b'x'; available];
        let (completion, cause, bytes, closes) = blob_case_framed(
            &payload,
            18,
            expected,
            Faults::default(),
            false,
            Some(expected),
        );
        peers(&completion.observation);
        assert!(completion.result.is_err());
        assert_eq!(cause, Some(Cause::Command));
        assert_eq!(completion.observation.exit_status, Some(18));
        assert!(completion.observation.natural_output_eof);
        assert_eq!(bytes, payload);
        assert!(closes.0 > 0 && closes.1 == 0);
    }
}
#[test]
fn a_postawait_body_completion_at240_001_cannot_write_inside_a_larger_whole_budget() {
    let payload = vec![b'x'; 65536];
    let (completion, cause, bytes, closes) = blob_case_framed(
        &payload,
        0,
        65536,
        Faults {
            late_output: true,
            ..Faults::default()
        },
        false,
        Some(65536),
    );
    assert!(
        completion.observation.input_closed
            && completion.observation.output_closed
            && completion.observation.error_closed
            && completion.observation.group_retired
            && completion.observation.reaped
    );
    assert!(completion.result.is_err());
    assert_eq!(cause, Some(Cause::Deadline));
    assert!(bytes.is_empty());
    assert!(!completion.observation.natural_output_eof);
    assert!(closes.0 > 0 && closes.1 == 0);
}

#[test]
fn refused_stdout_and_natural_stderr_eof_close_promptly_without_inventing_eof() {
    let started = Instant::now();
    let (completion, cause, _) = api(
        concat!(
            "/bin/cat >/dev/null; ",
            "printf 'HTTP/1.1 302 Found\\r\\n",
            "Location: https://evil.example/p?q=v\\r\\n\\r\\n'; exit 0"
        ),
        Faults::default(),
        Duration::from_secs(1),
    );
    let elapsed = started.elapsed();
    peers(&completion.observation);
    assert_eq!(cause, Some(Cause::Policy));
    assert!(completion.result.is_err());
    assert!(!completion.observation.output_eof);
    assert!(completion.observation.error_eof);
    assert!(
        elapsed < Duration::from_secs(6),
        "original1+10 tail was needlessly consumed: {elapsed:?}"
    );
}
#[test]
fn refused_stderr_and_natural_stdout_eof_close_promptly_without_inventing_eof() {
    let started = Instant::now();
    let (completion, cause, _) = api(
        concat!(
            "/bin/cat >/dev/null; exec 1>&-; ",
            "/usr/bin/head -c 65537 /dev/zero >&2; exit 0"
        ),
        Faults::default(),
        Duration::from_secs(1),
    );
    let elapsed = started.elapsed();
    peers(&completion.observation);
    assert_eq!(cause, Some(Cause::Bounds));
    assert!(completion.result.is_err());
    assert!(completion.observation.output_eof);
    assert!(!completion.observation.error_eof);
    assert_eq!(completion.observation.stderr_bytes, 65537);
    assert!(
        elapsed < Duration::from_secs(6),
        "original1+10 tail was needlessly consumed: {elapsed:?}"
    );
}
