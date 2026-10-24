//! Actual inert children and explicit fault edges; no GH, network or host work.

use super::*;
use crate::test_support::Fixture;
use std::{future::Future, io::Write, task::Poll};

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
fn request(owner: &Owner, script: &str, body: Duration) -> Invocation {
    let body_end = owner.now() + body;
    Invocation {
        program: PathBuf::from("/bin/sh"),
        arguments: vec!["-c".into(), script.into()],
        environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        body_end,
        settlement_end: body_end + TERM + REAP,
    }
}
fn case(script: &str, faults: Faults, body: Duration) -> (Completion, Option<Cause>, bool) {
    let runtime = runtime();
    let observed = runtime.block_on(async {
        let mut owner = owner();
        let request = request(&owner, script, body);
        let pending = run_with(&mut owner, request, faults);
        let compact = std::mem::size_of_val(&pending) < 32768;
        let completion = pending.await;
        (completion, owner.error(), compact)
    });
    drop(runtime);
    observed
}
fn settled(observed: &Observation) {
    assert!(observed.group_retired && observed.reaped);
    assert!(
        observed.stdin_close_attempted
            && observed.stdout_close_attempted
            && observed.stderr_close_attempted
    );
}
fn closed(observed: &Observation) {
    settled(observed);
    assert!(observed.stdin_closed && observed.stdout_closed && observed.stderr_closed);
}

#[test]
fn original_empty_stdin_and_raw_stream_bytes_finish_on_the_default_stack() {
    let (completion, cause, compact) = case(
        "printf out; printf err >&2; if IFS= read -r line; then exit 9; fi; exit 0",
        Faults::default(),
        Duration::from_secs(3),
    );
    assert!(compact, "unboxed native future retains heap buffers");
    closed(&completion.observation);
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
    assert_eq!(completion.result.unwrap(), b"out");
    assert_eq!(cause, None);
}

#[test]
fn a_descendant_retaining_a_pipe_hits_body_deadline_before_cleanup_eof() {
    let (completion, cause, compact) = case(
        "(trap '' TERM; exec /bin/sleep 60) & exit 0",
        Faults::default(),
        Duration::from_millis(80),
    );
    assert!(compact);
    closed(&completion.observation);
    assert_eq!(completion.observation.exit_status, Some(0));
    assert_eq!(cause, Some(Cause::Deadline));
    assert!(matches!(completion.result, Err(Cause::Deadline)));
    assert!(completion.observation.timed_out);
    // Group retirement can produce EOF, but it cannot authorize command success.
    assert!(completion.observation.stdout_eof && completion.observation.stderr_eof);
}

#[test]
fn a_quiet_live_child_is_retired_without_mapping_its_signal_to_owner_interrupt() {
    let (completion, cause, _) = case(
        "exec /bin/sleep 60",
        Faults::default(),
        Duration::from_millis(80),
    );
    closed(&completion.observation);
    assert_eq!(cause, Some(Cause::Deadline));
    assert!(
        completion
            .observation
            .exit_status
            .is_some_and(|status| status < 0)
    );
    assert!(matches!(completion.result, Err(Cause::Deadline)));
}

#[test]
fn a_descendant_that_closed_its_pipes_is_still_retired_before_leader_reap() {
    let (completion, cause, _) = case(
        "(/bin/sleep 60 </dev/null >/dev/null 2>&1) & printf retained; exit 0",
        Faults::default(),
        Duration::from_secs(3),
    );
    closed(&completion.observation);
    assert!(completion.observation.stdout_eof && completion.observation.stderr_eof);
    assert_eq!(completion.observation.exit_status, Some(0));
    assert_eq!(completion.result.unwrap(), b"retained");
    assert_eq!(cause, None);
}

#[test]
fn checked_close_refusals_attempt_all_independent_peers_and_preserve_raw_status() {
    for faults in [
        Faults {
            output_close: true,
            ..Faults::default()
        },
        Faults {
            error_close: true,
            ..Faults::default()
        },
        Faults {
            input_close: true,
            output_close: true,
            error_close: true,
            ..Faults::default()
        },
    ] {
        let (completion, cause, _) =
            case("printf out; printf err >&2", faults, Duration::from_secs(3));
        settled(&completion.observation);
        assert_eq!(cause, Some(Cause::Close));
        assert!(completion.observation.exit_status.is_some());
        assert!(matches!(completion.result, Err(Cause::Close)));
        assert!(
            completion
                .observation
                .cleanup_errors
                .contains(&Cause::Close)
        );
    }
    let (completion, cause, _) = case(
        "exit 7",
        Faults {
            output_close: true,
            ..Faults::default()
        },
        Duration::from_secs(3),
    );
    settled(&completion.observation);
    assert_eq!(completion.observation.exit_status, Some(7));
    assert_eq!(cause, Some(Cause::Command));
    assert!(matches!(completion.result, Err(Cause::Command)));
    assert!(
        completion
            .observation
            .cleanup_errors
            .contains(&Cause::Close)
    );
}

#[test]
fn synthetic_read_and_conversion_refusals_close_the_other_real_pipe() {
    for faults in [
        Faults {
            output_read: true,
            ..Faults::default()
        },
        Faults {
            error_read: true,
            ..Faults::default()
        },
        Faults {
            output_conversion: true,
            ..Faults::default()
        },
    ] {
        let (completion, cause, _) =
            case("printf out; printf err >&2", faults, Duration::from_secs(3));
        closed(&completion.observation);
        assert_eq!(cause, Some(Cause::Io));
        assert!(matches!(completion.result, Err(Cause::Io)));
        assert!(completion.observation.exit_status.is_some());
    }
}

fn emit(stdout: bool, byte: u8, count: usize) -> std::io::Result<()> {
    if stdout {
        emit_to(std::io::stdout().lock(), byte, count)
    } else {
        emit_to(std::io::stderr().lock(), byte, count)
    }
}
fn emit_to(mut destination: impl Write, byte: u8, count: usize) -> std::io::Result<()> {
    let buffer = vec![byte; 65536];
    let mut left = count;
    while left != 0 {
        let amount = left.min(buffer.len());
        destination.write_all(&buffer[..amount])?;
        left -= amount;
    }
    destination.flush()
}

fn signal_worker(interrupt: bool) {
    let runtime = runtime();
    let observed = runtime.block_on(async {
        let mut owner = owner();
        let fixture = Fixture::new();
        let marker = fixture.0.join("ready");
        let mut request = request(
            &owner,
            ": > \"$1\"; exec /bin/sleep 60",
            Duration::from_secs(5),
        );
        request
            .arguments
            .extend(["fixture".into(), marker.clone().into_os_string()]);
        let end = request.body_end;
        let scheduler = tokio::spawn(async move {
            while Instant::now() < end {
                if marker.exists() {
                    let signal = if interrupt {
                        nix::sys::signal::Signal::SIGINT
                    } else {
                        nix::sys::signal::Signal::SIGTERM
                    };
                    return nix::sys::signal::kill(nix::unistd::Pid::this(), signal).is_ok();
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
    let (completion, cause, sent) = observed;
    closed(&completion.observation);
    assert!(sent.unwrap());
    let expected = if interrupt {
        Cause::Interrupt
    } else {
        Cause::Terminate
    };
    assert_eq!(cause, Some(expected));
    assert!(matches!(completion.result, Err(actual) if actual == expected));
    assert!(completion.observation.exit_status.is_some());
}
fn worker(role: &str) {
    match role {
        "streams" => {
            let out = std::thread::spawn(|| emit(true, 0xa5, 1024 * 1024));
            let err = std::thread::spawn(|| emit(false, 0x5a, 60000));
            let out = out.join();
            let err = err.join();
            out.unwrap().unwrap();
            err.unwrap().unwrap();
        }
        "stdout-limit" => {
            let _ = emit(true, 0xa5, OUT_LIMIT as usize + 1);
        }
        "stderr-limit" => {
            let _ = emit(false, 0x5a, ERR_LIMIT as usize + 1);
        }
        "exit124" => std::process::exit(124),
        "exit130" => std::process::exit(130),
        "exit143" => std::process::exit(143),
        "int" => signal_worker(true),
        "term" => signal_worker(false),
        _ => panic!("unknown closed native verifier role"),
    }
}

#[test]
fn isolated_workers_keep_capture_caps_raw_exits_and_owned_signals_distinct() {
    const ROLE: &str = "RELEASE_VERIFY_NATIVE_WORKER";
    if let Some(role) = std::env::var_os(ROLE) {
        return worker(role.to_str().expect("closed UTF8 role"));
    }
    let program = std::env::current_exe().unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!(
        "{module}::{}",
        "isolated_workers_keep_capture_caps_raw_exits_and_owned_signals_distinct"
    );
    for role in [
        "streams",
        "stdout-limit",
        "stderr-limit",
        "exit124",
        "exit130",
        "exit143",
        "int",
        "term",
    ] {
        let runtime = runtime();
        let (completion, cause, compact) = runtime.block_on(async {
            let mut owner = owner();
            let body_end = owner.now() + Duration::from_secs(20);
            let request = Invocation {
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
            };
            let pending = run(&mut owner, request);
            let compact = std::mem::size_of_val(&pending) < 32768;
            let completion = pending.await;
            (completion, owner.error(), compact)
        });
        drop(runtime);
        closed(&completion.observation);
        assert!(compact, "{role}: unboxed production future");
        match role {
            "stdout-limit" => {
                assert_eq!(cause, Some(Cause::Bounds));
                assert_eq!(completion.observation.stdout_bytes, OUT_LIMIT + 1);
                assert!(completion.observation.stdout_limited);
                assert!(!completion.observation.stdout_eof);
            }
            "stderr-limit" => {
                assert_eq!(cause, Some(Cause::Bounds));
                assert_eq!(completion.observation.stderr_bytes, ERR_LIMIT + 1);
                assert!(completion.observation.stderr_limited);
                assert!(!completion.observation.stderr_eof);
            }
            "exit124" | "exit130" | "exit143" => {
                let expected = role.strip_prefix("exit").unwrap().parse::<i32>().unwrap();
                assert_eq!(cause, Some(Cause::Command));
                assert_eq!(completion.observation.exit_status, Some(expected));
                assert!(matches!(completion.result, Err(Cause::Command)));
            }
            _ => {
                assert_eq!(cause, None, "{role}");
                assert_eq!(completion.observation.exit_status, Some(0));
                let output = completion.result.unwrap();
                assert_eq!(output.len() as u64, completion.observation.stdout_bytes);
                assert_eq!(
                    hash(&Sha256::new_with_prefix(&output)),
                    completion.observation.stdout_sha256
                );
                if role == "streams" {
                    assert_eq!(output.iter().filter(|b| **b == 0xa5).count(), 1024 * 1024);
                    assert_eq!(completion.observation.stderr_bytes, 60000);
                    assert_eq!(
                        completion.observation.stderr_sha256,
                        hash(&Sha256::new_with_prefix(vec![0x5a; 60000]))
                    );
                }
            }
        }
    }
}

#[test]
fn dropping_a_pending_read_keeps_original_buffers_and_resumes_actual_pipe_bytes() {
    let runtime = runtime();
    let observed = runtime.block_on(async {
        let mut owner = owner();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "IFS= read -r gate; printf after; printf peer >&2"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut group = Group::new(command.spawn().unwrap());
        drop(command);
        let mut input = group.child().stdin.take().expect("piped input");
        let stdout = group.child().stdout.take().expect("piped output");
        let stderr = group.child().stderr.take().expect("piped errors");
        let mut streams = Streams::new();
        let mut observed = Observation::new();
        match ChildStdout::from_std(stdout) {
            Ok(pipe) => streams.stdout = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        match ChildStderr::from_std(stderr) {
            Ok(pipe) => streams.stderr = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        let faults = Faults::default();
        let mut pending = false;
        let mut compact = false;
        let end = owner.now() + Duration::from_secs(3);
        while owner.now() < end && owner.error().is_none() {
            let mut future = std::pin::pin!(streams.read_one(&mut observed, &faults));
            compact = std::mem::size_of_val(future.as_ref().get_ref()) < 32768;
            let polled = std::future::poll_fn(|context| {
                Poll::Ready(match future.as_mut().poll(context) {
                    Poll::Pending => None,
                    Poll::Ready(value) => Some(value),
                })
            })
            .await;
            match polled {
                None => {
                    pending = true;
                    break;
                }
                Some(Err(cause)) => owner.record(cause),
                Some(Ok(())) => {}
            }
        }
        let premature_eof = observed.stdout_eof || observed.stderr_eof;
        if input.write_all(b"release\n").is_err() {
            owner.record(Cause::Io);
        }
        let input: OwnedFd = input.into();
        observed.stdin_close_attempted = true;
        observed.stdin_closed = nix::unistd::close(input).is_ok();
        if !observed.stdin_closed {
            owner.record(Cause::Close);
        }
        while owner.now() < end && owner.check(Window::Body).is_ok() {
            match owned_process::exited_now(group.pid()) {
                Ok(true) if streams.complete(&observed) => break,
                Ok(_) => {}
                Err(_) => {
                    owner.record(Cause::Io);
                    break;
                }
            }
            if let Err(cause) = streams.read_one(&mut observed, &faults).await {
                owner.record(cause);
                break;
            }
        }
        observed.group_retired = owned_process::finish_before(group.pid(), end).await;
        match group.reap().await {
            Ok(status) => {
                observed.reaped = true;
                observed.exit_status = status.code();
            }
            Err(_) => owner.record(Cause::Close),
        }
        streams.close(&mut owner, &mut observed, &faults);
        (
            pending,
            compact,
            premature_eof,
            streams.captured,
            observed,
            owner.error(),
        )
    });
    drop(runtime);
    let (pending, compact, premature_eof, output, observed, cause) = observed;
    closed(&observed);
    assert!(pending && compact && !premature_eof);
    assert!(observed.stdout_eof && observed.stderr_eof);
    assert_eq!(output, b"after");
    assert_eq!(observed.stderr_bytes, 4);
    assert_eq!(cause, None);
}
