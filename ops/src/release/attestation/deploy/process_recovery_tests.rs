//! Real inert native commands and current-observer faults; no Docker or host rollout.

use super::*;
use crate::test_support::Fixture;
use std::{
    fs::OpenOptions,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

struct Case {
    output: CommandOutput,
    primary: Option<Cause>,
    finished: std::result::Result<(), Cause>,
}
fn command(
    script: &str,
    prior: Option<Cause>,
    cleanup: bool,
    cap: Duration,
    input: Option<&std::fs::File>,
) -> Case {
    let mut owner = Owner::new(
        Instant::now(),
        Duration::from_secs(30),
        Duration::from_secs(5),
    )
    .unwrap();
    let original_end = owner.end();
    if let Some(cause) = prior {
        owner.record(cause);
    }
    if cleanup {
        owner.begin_deploy_recovery().unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let body = owner.now() + cap;
    let output = runtime.block_on(Native.run(
        &mut owner,
        super::super::Invocation {
            role: Role::Stop,
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), script.into()],
            body_end: body,
            settlement_end: body + Duration::from_secs(3),
            cleanup,
            input,
            output: None,
        },
    ));
    if cleanup {
        owner.end_deploy_recovery();
    }
    assert_eq!(owner.end(), original_end);
    drop(runtime);
    let primary = owner.error();
    let finished = owner.finish();
    Case {
        output,
        primary,
        finished,
    }
}

#[test]
fn real_native_cleanup_can_complete_without_repeating_the_original_failure() {
    for prior in [Cause::Policy, Cause::Deadline] {
        let case = command(
            "printf OUT; printf ERR >&2",
            Some(prior),
            true,
            Duration::from_secs(3),
            None,
        );
        assert_eq!(case.output.status, Some(0));
        assert_eq!(case.output.error, None);
        assert!(case.output.settled && case.output.stdout_complete);
        assert_eq!(case.output.stdout, b"OUT");
        assert_eq!(case.output.stderr, b"ERR");
        assert_eq!(case.primary, Some(prior));
        assert_eq!(case.finished, Err(prior));
    }
    let ordinary = command("printf OUT", None, false, Duration::from_secs(3), None);
    assert!(ordinary.output.settled);
    assert_eq!(ordinary.output.error, None);
    assert_eq!(ordinary.finished, Ok(()));
    let refused = command(
        "printf UNREACHED",
        Some(Cause::Policy),
        false,
        Duration::from_secs(3),
        None,
    );
    assert_eq!(refused.output.status, None);
    assert_eq!(refused.output.error, Some(Cause::Policy));
    assert!(refused.output.stdout.is_empty() && !refused.output.settled);
    assert_eq!(refused.finished, Err(Cause::Policy));
}

#[test]
fn real_current_command_failures_and_input_refusals_cannot_be_inherited_away() {
    for prior in [Cause::Policy, Cause::Command] {
        let case = command(
            "printf OUT; printf ERR >&2; exit 7",
            Some(prior),
            true,
            Duration::from_secs(3),
            None,
        );
        assert_eq!(case.output.status, Some(7));
        assert!(case.output.settled && case.output.stdout_complete);
        assert_eq!(case.output.error, Some(Cause::Command));
        assert_eq!(case.primary, Some(prior));
        assert_eq!(case.finished, Err(prior));
    }
    let timeout = command(
        "sleep 20",
        Some(Cause::Deadline),
        true,
        Duration::from_millis(40),
        None,
    );
    assert_eq!(timeout.output.error, Some(Cause::Deadline));
    assert!(!timeout.output.settled);
    assert_eq!(timeout.finished, Err(Cause::Deadline));
    let overflow = command(
        "head -c 2097153 /dev/zero",
        Some(Cause::Policy),
        true,
        Duration::from_secs(3),
        None,
    );
    assert_eq!(overflow.output.error, Some(Cause::Bounds));
    assert!(!overflow.output.settled);
    assert_eq!(overflow.finished, Err(Cause::Policy));
    let fixture = Fixture::new();
    let input = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.0.join("read-write-input"))
        .unwrap();
    // The production input adapter requires read-only; no child may start here.
    let refused = command(
        "printf UNREACHED",
        Some(Cause::Policy),
        true,
        Duration::from_secs(3),
        Some(&input),
    );
    assert!(refused.output.error.is_some() && !refused.output.settled);
    assert_eq!(refused.output.status, None);
    assert!(refused.output.stdout.is_empty());
    assert_eq!(refused.finished, Err(Cause::Policy));
    assert!(nix::unistd::close(input).is_ok());
}

#[test]
fn actual_close_observer_fault_and_same_cause_current_observations_stay_negative() {
    let mut owner = Owner::new(
        Instant::now(),
        Duration::from_secs(30),
        Duration::from_secs(5),
    )
    .unwrap();
    owner.record(Cause::Policy);
    owner.begin_deploy_recovery().unwrap();
    let original_end = owner.end();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let body = owner.now() + Duration::from_secs(3);
    // The existing primitive performs the real close, then refuses its observer.
    let mut completed =
        runtime.block_on(super::super::super::stage::process::run_stderr_close_fault(
            &mut owner,
            Invocation {
                program: "/bin/sh".into(),
                arguments: vec!["-c".into(), "printf OUT; printf ERR >&2".into()],
                environment: BTreeMap::new(),
                body_end: body,
                settlement_end: body + Duration::from_secs(3),
                cleanup: true,
            },
        ));
    let proof = &completed.observation;
    assert_eq!(proof.exit_status, Some(0));
    assert!(proof.group_retired && proof.reaped && proof.stderr_close_attempted);
    assert!(!proof.stderr_closed && proof.cleanup_errors.contains(&Cause::Close));
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            completed.result.err(),
            proof,
            false,
            owner.final_work(),
            false
        ),
        Some(Cause::Close)
    );
    // These receipt mutations are classifier neighbors: known role/body facts
    // must not be replaced by the subsequent close observer's refusal.
    completed.observation.stdout_limited = true;
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            Some(Cause::Policy),
            &completed.observation,
            false,
            Ok(()),
            false
        ),
        Some(Cause::Bounds)
    );
    completed.observation.stdout_limited = false;
    completed.observation.stderr_limited = true;
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            Some(Cause::Policy),
            &completed.observation,
            false,
            Ok(()),
            false
        ),
        Some(Cause::Bounds)
    );
    completed.observation.stderr_limited = false;
    completed.observation.role_timed_out = true;
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            Some(Cause::Policy),
            &completed.observation,
            false,
            Ok(()),
            false
        ),
        Some(Cause::Deadline)
    );
    // A separately reported current error keeps precedence over this projection.
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            Some(Cause::Io),
            &completed.observation,
            false,
            Ok(()),
            false
        ),
        Some(Cause::Io)
    );
    completed.observation.role_timed_out = false;
    // Synthetic current-policy observation tests the classifier's same-cause
    // rule; the actual shared syscall adapter does not itself generate Policy.
    completed.observation.cleanup_errors = vec![Cause::Policy];
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            Some(Cause::Policy),
            &completed.observation,
            false,
            Ok(()),
            false
        ),
        Some(Cause::Policy)
    );
    assert_eq!(
        command_error(
            Some(Cause::Policy),
            true,
            Some(Cause::Policy),
            &completed.observation,
            true,
            Err(Cause::Terminate),
            false
        ),
        Some(Cause::Policy)
    );
    owner.end_deploy_recovery();
    assert_eq!(owner.end(), original_end);
    drop(runtime);
    assert_eq!(owner.finish(), Err(Cause::Policy));
}

const SIGNAL_WORKER: &str = "SC_DEPLOY_RECOVERY_SIGNAL_WORKER";
#[test]
fn a_consumed_real_signal_stays_visible_without_replacing_the_primary_or_deadline() {
    if std::env::var(SIGNAL_WORKER).as_deref() == Ok("1") {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .unwrap();
        owner.record(Cause::Policy);
        owner.begin_deploy_recovery().unwrap();
        let original_end = owner.end();
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        assert_eq!(owner.final_work(), Err(Cause::Terminate));
        assert_eq!(owner.final_work(), Ok(())); // The pending observation was consumed.
        assert_eq!(owner.signal_cause(), Some(Cause::Terminate));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let body = owner.now() + Duration::from_secs(3);
        let output = runtime.block_on(Native.run(
            &mut owner,
            super::super::Invocation {
                role: Role::Stop,
                program: "/bin/sh".into(),
                arguments: vec!["-c".into(), "printf OUT".into()],
                body_end: body,
                settlement_end: body + Duration::from_secs(3),
                cleanup: true,
                input: None,
                output: None,
            },
        ));
        assert_eq!(output.status, Some(0));
        assert_eq!(output.error, Some(Cause::Terminate));
        assert!(!output.settled);
        assert_eq!(owner.error(), Some(Cause::Policy));
        owner.end_deploy_recovery();
        assert_eq!(owner.end(), original_end);
        drop(runtime);
        assert_eq!(owner.finish(), Err(Cause::Policy));
        return;
    }
    // The real signal is confined to this owned inert worker process.
    let mut owner = Owner::new(
        Instant::now(),
        Duration::from_secs(30),
        Duration::from_secs(5),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let name = concat!(
        module_path!(),
        "::a_consumed_real_signal_stays_visible_without_replacing_the_primary_or_deadline"
    );
    let name = name.split_once("::").unwrap().1;
    let body = owner.now() + Duration::from_secs(10);
    let completed = runtime.block_on(super::super::super::stage::process::run(
        &mut owner,
        Invocation {
            program: std::env::current_exe().unwrap(),
            arguments: vec!["--exact".into(), name.into(), "--test-threads=1".into()],
            environment: BTreeMap::from([(SIGNAL_WORKER.into(), "1".into())]),
            body_end: body,
            settlement_end: body + Duration::from_secs(6),
            cleanup: false,
        },
    ));
    assert_eq!(
        completed.result,
        Ok(()),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    assert_eq!(completed.observation.exit_status, Some(0));
    assert!(
        completed.observation.group_retired
            && completed.observation.reaped
            && completed.observation.stdout_closed
            && completed.observation.stderr_closed
    );
    drop(runtime);
    assert_eq!(owner.finish(), Ok(()));
}
