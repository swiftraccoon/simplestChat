//! Real dual evidence pipes with bounded duplicate capture; no engine or network.
use super::*;
use crate::test_support::Fixture;
use std::{
    fs::OpenOptions,
    io::{Read, SeekFrom},
    os::unix::fs::OpenOptionsExt,
};
fn close(file: File) -> bool {
    nix::unistd::close(file).is_ok()
}
struct Case {
    completed: Completion,
    cause: Option<Cause>,
    stdout: std::io::Result<Vec<u8>>,
    stderr: std::io::Result<Vec<u8>>,
    originals_closed: bool,
    compact: bool,
    descendant: Option<i32>,
}
fn run(script: &str, faults: Faults, alias: bool, maximum: u64) -> Case {
    run_kind(
        script,
        faults,
        alias,
        maximum,
        false,
        None,
        Duration::from_secs(3),
    )
}
fn run_kind(
    script: &str,
    faults: Faults,
    alias: bool,
    maximum: u64,
    cleanup: bool,
    prior: Option<Cause>,
    body_duration: Duration,
) -> Case {
    let fixture = Fixture::new();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut owner = Owner::new(
        Instant::now(),
        Duration::from_secs(45),
        Duration::from_secs(5),
    )
    .unwrap();
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.0.join("stdout"))
        .unwrap();
    let error = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.0.join("stderr"));
    let error = match error {
        Ok(file) => file,
        Err(first) => {
            let closed = close(output);
            drop(runtime);
            let finished = owner.finish();
            panic!("second original admission: {first}; prior closed={closed}; {finished:?}");
        }
    };
    if let Some(cause) = prior {
        owner.record(cause);
    }
    let body = owner.now() + body_duration;
    let request = Invocation {
        program: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            script.into(),
            "restore-io".into(),
            fixture.0.join("descendant").into_os_string(),
        ],
        environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        body_end: body,
        settlement_end: body + TERM + REAP,
        cleanup,
    };
    let (completed, compact) = runtime.block_on(async {
        let future = run_io_with(
            &mut owner,
            request,
            faults,
            OUT_LIMIT,
            Io {
                input: None,
                output: Some(Sink {
                    file: &output,
                    maximum,
                }),
                stderr: Some(Sink {
                    file: if alias { &output } else { &error },
                    maximum: ERR_LIMIT,
                }),
                capture_output: true,
                allow_nonzero: true,
            },
        );
        let compact = std::mem::size_of_val(&future) < 32768;
        (future.await, compact)
    });
    let read = |mut file: &File| -> std::io::Result<Vec<u8>> {
        file.seek(SeekFrom::Start(0))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        Ok(data)
    };
    let stdout = read(&output);
    let stderr = read(&error);
    let output_closed = close(output);
    let error_closed = close(error);
    drop(runtime);
    let cause = owner.finish().err();
    let descendant = std::fs::read_to_string(fixture.0.join("descendant"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    drop(fixture);
    Case {
        completed,
        cause,
        stdout,
        stderr,
        originals_closed: output_closed && error_closed,
        compact,
        descendant,
    }
}
fn peers(case: &Case) {
    let p = &case.completed.observation;
    assert!(case.originals_closed && case.compact);
    assert!(p.pid.is_some() && p.group_retired && p.reaped);
    assert!(p.stdin_close_attempted && p.stdin_closed);
    assert!(p.stdout_close_attempted && p.stderr_close_attempted);
    assert!(p.stdout_closed && p.stderr_closed);
}
#[test]
fn both_live_evidence_originals_match_the_observed_bounded_captures() {
    let case = run(
        "printf OUT; printf ERR >&2",
        Faults::default(),
        false,
        OUT_LIMIT,
    );
    peers(&case);
    let p = &case.completed.observation;
    assert_eq!(case.cause, None);
    assert_eq!(case.stdout.unwrap(), b"OUT");
    assert_eq!(case.stderr.unwrap(), b"ERR");
    assert_eq!(case.completed.stdout, b"OUT");
    assert_eq!(case.completed.stderr, b"ERR");
    assert!(p.bulk_complete && p.stderr_sink_complete);
    assert_eq!(p.stderr_sink_sha256, p.stderr_sha256);
}
#[test]
fn each_sink_write_failure_preserves_first_cause_and_closes_every_known_peer() {
    for stdout in [true, false] {
        let faults = if stdout {
            Faults {
                bulk_write_after: Some(2),
                ..Faults::default()
            }
        } else {
            Faults {
                stderr_write_after: Some(2),
                ..Faults::default()
            }
        };
        let case = run(
            "printf OUT; printf ERR >&2; sleep 20",
            faults,
            false,
            OUT_LIMIT,
        );
        peers(&case);
        assert_eq!(case.cause, Some(Cause::Io));
        let written = if stdout {
            case.stdout.unwrap()
        } else {
            case.stderr.unwrap()
        };
        assert_eq!(
            written,
            if stdout {
                b"OU".as_slice()
            } else {
                b"ER".as_slice()
            }
        );
    }
}
#[test]
fn a_naturally_complete_create_id_survives_later_stderr_pipe_close_refusal() {
    let id = "a".repeat(64);
    let case = run(
        &format!("printf '{id}'; printf note >&2"),
        Faults {
            error_close: true,
            ..Faults::default()
        },
        false,
        OUT_LIMIT,
    );
    // The simulated checked-close refusal follows the actual close and reap.
    assert!(case.originals_closed && case.compact);
    let p = &case.completed.observation;
    assert!(p.group_retired && p.reaped && p.stdout_closed && p.stderr_close_attempted);
    assert_eq!(case.cause, Some(Cause::Close));
    assert_eq!(p.exit_status, Some(0));
    assert!(p.stdout_body_eof && !p.stdout_limited);
    assert_eq!(case.completed.stdout, id.as_bytes());
    assert_eq!(case.stdout.unwrap(), id.as_bytes());
}
#[test]
fn cleanup_drained_capture_and_eof_cannot_complete_a_create_identity() {
    let id = "a".repeat(64);
    let case = run(
        &format!("printf '{id}'"),
        Faults {
            terminal_before_read: true,
            late_bulk_read: true,
            ..Faults::default()
        },
        false,
        OUT_LIMIT,
    );
    peers(&case);
    let p = &case.completed.observation;
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert_eq!(p.exit_status, Some(0));
    assert!(p.stdout_eof && !p.stdout_body_eof && !p.bulk_complete);
    assert_eq!(case.completed.stdout, id.as_bytes());
    assert!(case.stdout.unwrap().is_empty());
}
#[test]
fn a_postawait_stderr_deadline_never_commits_its_late_observed_prefix() {
    let case = run(
        "printf ERR >&2",
        Faults {
            terminal_before_read: true,
            late_stderr_read: true,
            ..Faults::default()
        },
        false,
        OUT_LIMIT,
    );
    peers(&case);
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert!(case.stderr.unwrap().is_empty());
    assert!(!case.completed.observation.stderr_sink_complete);
}
#[test]
fn aliased_sinks_and_large_duplicate_capture_refuse_before_child_admission() {
    for (alias, maximum) in [(true, OUT_LIMIT), (false, OUT_LIMIT + 1)] {
        let case = run("exit 0", Faults::default(), alias, maximum);
        assert!(case.originals_closed && case.compact);
        assert_eq!(case.cause, Some(Cause::Input));
        assert!(case.completed.observation.pid.is_none());
        assert!(case.stdout.unwrap().is_empty() && case.stderr.unwrap().is_empty());
    }
}

#[test]
fn a_prior_deadline_does_not_fabricate_a_current_cleanup_role_timeout() {
    let case = run_kind(
        "printf OUT; printf ERR >&2",
        Faults::default(),
        false,
        OUT_LIMIT,
        true,
        Some(Cause::Deadline),
        Duration::from_secs(3),
    );
    peers(&case);
    let proof = &case.completed.observation;
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert_eq!(proof.exit_status, Some(0));
    assert!(proof.timed_out && !proof.role_timed_out);
    assert!(
        proof.stdout_body_eof,
        "current cleanup stdout must reach natural body EOF"
    );
    assert!(
        proof.stderr_body_eof,
        "current cleanup stderr must reach natural body EOF"
    );
    assert!(
        proof.cleanup_errors.is_empty(),
        "current cleanup peer errors: {:?}",
        proof.cleanup_errors
    );
    assert!(proof.bulk_complete && proof.stderr_sink_complete);
    assert_eq!(case.stdout.unwrap(), b"OUT");
    assert_eq!(case.stderr.unwrap(), b"ERR");
}
#[test]
fn a_fresh_current_role_timeout_is_not_discharged_by_an_inherited_deadline() {
    let case = run_kind(
        "sleep 20",
        Faults::default(),
        false,
        OUT_LIMIT,
        true,
        Some(Cause::Deadline),
        Duration::from_millis(40),
    );
    peers(&case);
    let proof = &case.completed.observation;
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert!(proof.role_timed_out && !proof.stdout_body_eof && !proof.stderr_body_eof);
    assert!(!proof.bulk_complete && !proof.stderr_sink_complete);
}
#[test]
fn proved_terminal_leader_still_retires_its_exact_live_descendant_group() {
    let script = concat!(
        "sleep 20 </dev/null >/dev/null 2>&1 & child=$!; ",
        "printf '%s' \"$child\" > \"$1\"; printf OUT; printf ERR >&2",
    );
    let case = run_kind(
        script,
        Faults {
            terminal_before_read: true,
            ..Faults::default()
        },
        false,
        OUT_LIMIT,
        true,
        Some(Cause::Deadline),
        Duration::from_secs(3),
    );
    peers(&case);
    let proof = &case.completed.observation;
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert_eq!(proof.exit_status, Some(0));
    assert!(
        case.descendant
            .is_some_and(|pid| pid > 0 && Some(pid as u32) != proof.pid)
    );
    assert!(proof.group_retired && proof.reaped && !proof.role_timed_out);
    assert!(proof.cleanup_errors.is_empty());
}
