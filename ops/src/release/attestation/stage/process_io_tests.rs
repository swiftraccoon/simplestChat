//! Actual inherited files and pipe-driven bulk output; no engine or network.

use super::*;
use crate::test_support::Fixture;
use std::{
    fs::OpenOptions,
    io::{Read, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
};

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
fn close(file: File) -> bool {
    nix::unistd::close(OwnedFd::from(file)).is_ok()
}
fn originals(data: Option<&[u8]>) -> (Fixture, Option<File>, File) {
    let fixture = Fixture::new();
    let input = data.map(|bytes| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(fixture.0.join("input"))
            .unwrap();
        let written = file.write_all(bytes);
        let closed = close(file);
        // The writer is already settled before either result is judged.
        written.unwrap();
        assert!(closed);
        File::open(fixture.0.join("input")).unwrap()
    });
    let output = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(fixture.0.join("output"));
    match output {
        Ok(output) => (fixture, input, output),
        Err(first) => {
            let input_closed = input.is_none_or(close);
            panic!("output admission: {first}; prior original closed: {input_closed}");
        }
    }
}
fn request(owner: &Owner, script: &str, fixture: &Fixture, body: Duration) -> Invocation {
    let body_end = owner.now() + body;
    Invocation {
        program: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            script.into(),
            "io-fixture".into(),
            fixture.0.join("gate").into_os_string(),
        ],
        environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        body_end,
        settlement_end: body_end + TERM + REAP,
        cleanup: false,
    }
}
struct Case {
    completion: Completion,
    cause: Option<Cause>,
    compact: bool,
    bytes: std::io::Result<Vec<u8>>,
    input_position: Option<std::io::Result<u64>>,
    originals_closed: bool,
}
fn run_case(
    script: &str,
    data: Option<&[u8]>,
    maximum: Option<u64>,
    allow_nonzero: bool,
    faults: Faults,
    body: Duration,
) -> Case {
    let mut owner = owner();
    let runtime = runtime();
    let (fixture, input, mut output) = originals(data);
    let request = request(&owner, script, &fixture, body);
    let (completion, compact) = runtime.block_on(async {
        let future = run_io_with(
            &mut owner,
            request,
            faults,
            OUT_LIMIT,
            Io {
                input: input.as_ref(),
                output: maximum.map(|maximum| Sink {
                    file: &output,
                    maximum,
                }),
                allow_nonzero,
            },
        );
        let compact = std::mem::size_of_val(&future) < 32768;
        (future.await, compact)
    });
    let bytes = (|| {
        output.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes)?;
        Ok(bytes)
    })();
    let input_position = input.as_ref().map(|mut file| file.stream_position());
    let input_closed = input.is_none_or(close);
    let output_closed = close(output);
    drop(runtime);
    let cause = owner.finish().err();
    drop(fixture);
    Case {
        completion,
        cause,
        compact,
        bytes,
        input_position,
        originals_closed: input_closed && output_closed,
    }
}
fn peers(case: &Case, inherited: bool) {
    let proof = &case.completion.observation;
    assert!(
        case.compact,
        "the unboxed production future stays below 32 KiB"
    );
    assert!(case.originals_closed);
    assert!(proof.pid.is_some() && proof.group_retired && proof.reaped);
    assert!(proof.stdout_close_attempted && proof.stderr_close_attempted);
    assert!(proof.stdout_closed && proof.stderr_closed);
    assert_eq!(proof.stdin_inherited, inherited);
    if inherited {
        assert!(proof.stdin_duplicate_sdk_owned);
        assert!(!proof.stdin_close_attempted && !proof.stdin_closed);
    } else {
        assert!(proof.stdin_close_attempted && proof.stdin_closed);
        assert!(!proof.stdin_duplicate_sdk_owned);
    }
}

#[test]
fn inherited_original_and_bulk_pipe_preserve_binary_bytes_without_capture() {
    let data: Vec<_> = (0..3 * 1024 * 1024).map(|at| (at % 251) as u8).collect();
    let case = run_case(
        "(/usr/bin/head -c 60000 /dev/zero >&2) & /bin/cat; wait",
        Some(&data),
        Some(data.len() as u64),
        false,
        Faults::default(),
        Duration::from_secs(5),
    );
    peers(&case, true);
    let proof = &case.completion.observation;
    assert_eq!(case.cause, None);
    assert!(case.completion.result.is_ok());
    assert_eq!(proof.exit_status, Some(0));
    assert!(proof.stdout_body_eof && proof.stderr_body_eof && proof.bulk_complete);
    assert!(proof.stdout_eof && proof.stderr_eof && case.completion.stdout.is_empty());
    assert_eq!(proof.bulk_bytes, data.len() as u64);
    assert_eq!(proof.stdout_bytes, data.len() as u64);
    assert_eq!(proof.bulk_sha256, hash(&Sha256::new_with_prefix(&data)));
    assert_eq!(proof.bulk_sha256, proof.stdout_sha256);
    assert_eq!(proof.stderr_bytes, 60000);
    assert_eq!(case.bytes.unwrap(), data);
    assert_eq!(case.input_position.unwrap().unwrap(), 3 * 1024 * 1024);
}

#[test]
fn inherited_file_does_not_claim_that_the_child_read_its_input() {
    let case = run_case(
        "printf listed",
        Some(b"backup bytes intentionally unread"),
        None,
        false,
        Faults::default(),
        Duration::from_secs(3),
    );
    peers(&case, true);
    assert_eq!(case.cause, None);
    assert!(case.completion.result.is_ok());
    assert_eq!(case.completion.stdout, b"listed");
    assert_eq!(case.input_position.unwrap().unwrap(), 0);
    assert_eq!(case.bytes.unwrap(), b"");
    assert!(!case.completion.observation.bulk_complete);
}

#[test]
fn bulk_cap_and_actual_partial_write_refusals_keep_only_the_committed_prefix() {
    let data = vec![0xa5; 65537];
    let case = run_case(
        "/bin/cat",
        Some(&data),
        Some(65536),
        false,
        Faults::default(),
        Duration::from_secs(3),
    );
    peers(&case, true);
    let proof = &case.completion.observation;
    assert_eq!(case.cause, Some(Cause::Bounds));
    assert_eq!(case.completion.result, Err(Cause::Bounds));
    assert!(proof.stdout_limited && !proof.bulk_complete);
    assert_eq!(proof.stdout_bytes, 65537);
    assert_eq!(proof.bulk_bytes, 65536);
    assert_eq!(case.bytes.unwrap(), data[..65536]);
    assert_eq!(
        proof.bulk_sha256,
        hash(&Sha256::new_with_prefix(&data[..65536]))
    );
    assert!(case.completion.stdout.is_empty());

    let case = run_case(
        "printf abcdef",
        None,
        Some(65536),
        false,
        Faults {
            bulk_write_after: Some(3),
            ..Faults::default()
        },
        Duration::from_secs(3),
    );
    peers(&case, false);
    assert_eq!(case.cause, Some(Cause::Io));
    assert_eq!(case.completion.result, Err(Cause::Io));
    assert_eq!(case.completion.observation.bulk_bytes, 3);
    assert_eq!(case.bytes.unwrap(), b"abc");
    assert_eq!(
        case.completion.observation.bulk_sha256,
        hash(&Sha256::new_with_prefix(b"abc"))
    );
    assert!(!case.completion.observation.bulk_complete);
}

#[test]
fn cleanup_eof_cannot_complete_or_extend_a_bulk_file() {
    let case = run_case(
        concat!(
            "(trap '' TERM; : > \"$1\"; exec /bin/sleep 60) & ",
            "while [ ! -f \"$1\" ]; do /bin/sleep .01; done; printf live; exit 0"
        ),
        None,
        Some(65536),
        false,
        Faults::default(),
        Duration::from_millis(120),
    );
    peers(&case, false);
    let proof = &case.completion.observation;
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert_eq!(case.completion.result, Err(Cause::Deadline));
    assert_eq!(proof.exit_status, Some(0));
    assert!(!proof.stdout_body_eof && !proof.bulk_complete);
    assert!(proof.stdout_eof && proof.stderr_eof);
    assert_eq!(proof.bulk_bytes, 4);
    assert_eq!(case.bytes.unwrap(), b"live");
}

#[test]
fn completed_nonzero_status_is_data_only_when_explicitly_allowed() {
    for code in [7, 124, 130, 143] {
        for allowed in [false, true] {
            let case = run_case(
                &format!("printf result; printf diagnostic >&2; exit {code}"),
                None,
                None,
                allowed,
                Faults::default(),
                Duration::from_secs(3),
            );
            peers(&case, false);
            let proof = &case.completion.observation;
            assert_eq!(proof.exit_status, Some(code));
            assert!(proof.stdout_body_eof && proof.stderr_body_eof);
            assert_eq!(case.completion.stdout, b"result");
            assert_eq!(case.completion.stderr, b"diagnostic");
            assert_eq!(case.cause, (!allowed).then_some(Cause::Command));
            assert_eq!(
                case.completion.result,
                if allowed { Ok(()) } else { Err(Cause::Command) }
            );
        }
    }
    let case = run_case(
        "printf result; exit 7",
        None,
        None,
        true,
        Faults {
            error_close: true,
            ..Faults::default()
        },
        Duration::from_secs(3),
    );
    let proof = &case.completion.observation;
    assert!(case.originals_closed && proof.group_retired && proof.reaped);
    assert!(proof.stdout_closed && proof.stderr_close_attempted && !proof.stderr_closed);
    assert_eq!(proof.exit_status, Some(7));
    assert!(proof.stdout_body_eof && proof.stderr_body_eof);
    assert_eq!(case.cause, Some(Cause::Close));
    assert_eq!(case.completion.result, Err(Cause::Close));
}

#[test]
fn invalid_original_modes_offsets_and_sink_bounds_refuse_before_spawn() {
    for kind in [
        "writable-input",
        "input-offset",
        "nonempty-sink",
        "zero",
        "too-large",
    ] {
        let mut owner = owner();
        let runtime = runtime();
        let (fixture, input, mut output) = originals(Some(b"original"));
        let mut input = input.unwrap();
        let mut setup = Ok(());
        if kind == "writable-input" {
            let closed = close(input);
            let replacement = OpenOptions::new()
                .read(true)
                .write(true)
                .open(fixture.0.join("input"));
            if !closed {
                owner.record(Cause::Close);
            }
            input = match replacement {
                Ok(file) => file,
                Err(first) => {
                    let closed = close(output);
                    drop(runtime);
                    let owner_result = owner.finish();
                    panic!("replacement: {first}; output closed: {closed}; {owner_result:?}");
                }
            };
        } else if kind == "input-offset" {
            setup = input.seek(SeekFrom::Start(1)).map(|_| ());
        } else if kind == "nonempty-sink" {
            setup = output.write_all(b"prior");
        }
        let maximum = match kind {
            "zero" => 0,
            "too-large" => BULK_LIMIT + 1,
            _ => 65536,
        };
        let request = request(&owner, "exit 0", &fixture, Duration::from_secs(3));
        let completed = runtime.block_on(run_io(
            &mut owner,
            request,
            Io {
                input: Some(&input),
                output: Some(Sink {
                    file: &output,
                    maximum,
                }),
                allow_nonzero: false,
            },
        ));
        let input_closed = close(input);
        let output_closed = close(output);
        drop(runtime);
        let cause = owner.finish().err();
        drop(fixture);
        setup.unwrap();
        assert!(input_closed && output_closed);
        assert_eq!(cause, Some(Cause::Input), "{kind}");
        assert_eq!(completed.result, Err(Cause::Input));
        assert!(completed.observation.pid.is_none());
    }
}

#[test]
fn real_suffix_drained_after_body_refusal_never_reaches_the_original_bulk_sink() {
    let mut owner = owner();
    let runtime = runtime();
    let (fixture, _, mut output) = originals(None);
    let (observed, live, release, natural_joined, remaining_before) = runtime.block_on(async {
        // One original role window precedes spawn; settlement uses the same
        // production TERM + REAP debt inside this owner's existing body.
        let body_end = owner.now() + Duration::from_secs(3);
        let end = (body_end + TERM + REAP).min(owner.body_end());
        let natural_end = end.checked_sub(REAP).unwrap_or(end);
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf live; IFS= read -r gate; printf cleanup; printf peer >&2",
            ])
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
        let mut streams = Streams::new(65536);
        streams.bulk = true;
        let mut observed = Observation::new();
        observed.pid = Some(group.child().id());
        match ChildStdout::from_std(stdout) {
            Ok(pipe) => streams.stdout = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        match ChildStderr::from_std(stderr) {
            Ok(pipe) => streams.stderr = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        let faults = Faults::default();
        while owner.now() < body_end && observed.bulk_bytes < 4 {
            if let Err(cause) = streams.read_one(&mut observed, &faults).await {
                owner.record(cause);
                break;
            }
            if owner.check(Window::Body).is_err() || owner.now() >= body_end {
                break;
            }
            if let Err(cause) = streams.commit(
                &Sink {
                    file: &output,
                    maximum: 65536,
                },
                &mut owner,
                &mut observed,
                body_end,
                &faults,
            ) {
                owner.record(cause);
                break;
            }
        }
        let live = observed.bulk_bytes;
        // A fixed body refusal occurs before releasing any suffix. The same
        // read/accept path may drain it for ownership; it cannot call commit.
        owner.record(Cause::Deadline);
        let release = input.write_all(b"release\n");
        let input: OwnedFd = input.into();
        observed.stdin_close_attempted = true;
        observed.stdin_closed = nix::unistd::close(input).is_ok();
        // EOF alone does not prove that the shell has become terminal. This
        // fixture expects natural exit0, so join its reserved PID without
        // consuming it before production settle can send TERM. A late/missing
        // join still runs every finalizer inside the same original end.
        let mut natural_terminal = false;
        while owner.now() < natural_end {
            let _ = owner.check(Window::Final);
            match owned_process::exited_now(group.pid()) {
                Ok(terminal) => natural_terminal = terminal,
                Err(_) => {
                    observed.secondary(&mut owner, Cause::Io);
                    break;
                }
            }
            if natural_terminal && streams.complete(&observed) {
                break;
            }
            if let Err(cause) = streams.read_one(&mut observed, &faults).await {
                observed.secondary(&mut owner, cause);
            }
        }
        let natural_joined =
            natural_terminal && streams.complete(&observed) && owner.now() < natural_end;
        if !natural_joined {
            observed.secondary(&mut owner, Cause::Deadline);
        }
        let remaining_before = end.saturating_duration_since(owner.now());
        settle(
            &mut owner,
            &mut group,
            &mut streams,
            &mut observed,
            &faults,
            end,
            false,
        )
        .await;
        streams.close(&mut owner, &mut observed, &faults);
        observed.bulk_sha256 = hash(&streams.bulk_hash);
        observed.stdout_sha256 = hash(&streams.out_hash);
        (observed, live, release, natural_joined, remaining_before)
    });
    let readback = (|| {
        output.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    })();
    let output_closed = close(output);
    drop(runtime);
    let cause = owner.finish().err();
    drop(fixture);
    let facts = (
        natural_joined,
        remaining_before,
        observed.group_retired,
        observed.reaped,
        observed.exit_status,
        observed.stdout_eof,
        observed.stderr_eof,
        &observed.cleanup_errors,
    );
    release.unwrap();
    assert!(output_closed);
    assert!(observed.stdin_closed && observed.stdout_closed && observed.stderr_closed);
    assert!(observed.group_retired && observed.reaped, "{facts:?}");
    assert!(natural_joined, "natural completion refused: {facts:?}");
    assert_eq!(observed.exit_status, Some(0));
    assert_eq!(cause, Some(Cause::Deadline));
    assert_eq!(live, 4);
    assert_eq!(observed.stdout_bytes, 11);
    assert_eq!(observed.bulk_bytes, 4);
    assert_eq!(
        observed.stdout_sha256,
        hash(&Sha256::new_with_prefix(b"livecleanup"))
    );
    assert_eq!(
        observed.bulk_sha256,
        hash(&Sha256::new_with_prefix(b"live"))
    );
    assert_eq!(readback.unwrap(), b"live");
    assert!(observed.stdout_eof && observed.stderr_eof && !observed.stdout_body_eof);
    assert!(!observed.bulk_complete);
}

#[test]
fn an_allowed_signed_child_exit_is_not_an_owner_signal() {
    let case = run_case(
        "kill -TERM $$",
        None,
        None,
        true,
        Faults::default(),
        Duration::from_secs(3),
    );
    peers(&case, false);
    assert_eq!(case.completion.observation.exit_status, Some(-15));
    assert!(case.completion.observation.stdout_body_eof);
    assert!(case.completion.observation.stderr_body_eof);
    assert_eq!(case.cause, None);
    assert_eq!(case.completion.result, Ok(()));
}

#[test]
fn an_actual_read_completing_at_the_original_role_end_commits_no_late_bytes() {
    let case = run_case(
        "printf late",
        None,
        Some(65536),
        false,
        Faults {
            late_bulk_read: true,
            ..Faults::default()
        },
        Duration::from_secs(3),
    );
    peers(&case, false);
    let proof = &case.completion.observation;
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert_eq!(case.completion.result, Err(Cause::Deadline));
    assert_eq!(proof.stdout_bytes, 4);
    assert_eq!(proof.bulk_bytes, 0);
    assert!(!proof.stdout_body_eof && !proof.bulk_complete);
    assert_eq!(proof.bulk_sha256, hash(&Sha256::new()));
    assert_eq!(case.bytes.unwrap(), b"");
}
