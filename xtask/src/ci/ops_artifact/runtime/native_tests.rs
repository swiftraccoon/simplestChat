//! Actual inert Native processes exercise the new role admission and paired
//! stream owners. These helpers run no Git, Python, VM or candidate workload.

use super::super::namespace::Budget;
use super::{
    files::Output,
    fixture::{Clock, Fixture, expect_error},
    plan::Plan,
    tools::Tools,
    worker::Checkpoint,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
        release_checks::files::Executable,
    },
    owned_process::Signals,
};
use std::{
    fs,
    os::fd::AsRawFd,
    os::unix::fs::PermissionsExt,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[tokio::test]
async fn native_runtime_roles_require_the_exact_single_capsule_and_settle_both_pipes_on_refusal() {
    for changed in 0..=12 {
        let fixture = Fixture::new();
        let budget = Budget::default();
        let clock = Arc::new(Clock::new());
        let start = *clock.0.lock().unwrap();
        let plan = Plan::new(clock.clone());
        let checkpoint = Checkpoint {
            clock: clock.clone(),
            start,
            end: start + Duration::from_secs(150),
            abort: Arc::new(AtomicBool::new(false)),
        };
        let git = fixture.acquisition.root.join("git");
        let script = format!(
            concat!(
                "#!/bin/sh\n",
                "if [ \"${{GH_TOKEN+x}}\" = x ]; then exit 97; fi\n",
                "printf ran > inert-dispatched\n",
                "printf '%s\\n' '{}'\n"
            ),
            "a".repeat(40)
        );
        fs::write(&git, &script).unwrap();
        fs::write(fixture.acquisition.root.join("python3"), &script).unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        let mut output =
            Output::new(&fixture.output, &fixture.caller, &budget, &checkpoint).unwrap();
        let tools = Tools::open(
            &fixture.caller,
            &output,
            &fixture.acquisition.environment,
            &budget,
            &checkpoint,
        )
        .unwrap();
        let proof = tools
            .git_invocation(0, &plan, start + Duration::from_secs(150))
            .unwrap();
        let mut pending = output.capture(0).await.unwrap();
        let mut command = Command::capture(Program::OpsRuntimeGit, &[]);
        command.arguments = Tools::git_arguments(0).unwrap();
        command.capture = None;
        command.timeout = Duration::from_secs(5);
        command.grace = Duration::from_secs(5);
        command.separate_logs = pending.logs.take();
        command.ops_runtime = Some(proof.clone());
        let mut second = None;
        let mut input_descriptor = None;
        let second_permits = budget.reserve(2).unwrap();
        match changed {
            0 => {}
            1 => command.arguments.push("--PRIVATE-extra".into()),
            2 => command.program = Program::OpsRuntimePython,
            3 => command.timeout += Duration::from_secs(1),
            4 => command.grace += Duration::from_secs(1),
            5 => command.cleanup = true,
            6 => command.capture = Some(1),
            7 => command.ops_runtime = None,
            8 => clock.advance(Duration::from_secs(121)),
            9 => fs::write(
                fixture.caller.join("build/security_vm_ops.py"),
                b"changed original source",
            )
            .unwrap(),
            10 => {
                let selected = Arc::new(Executable::open(&git).unwrap());
                command.node = Some(selected.clone());
                second = Some(selected);
            }
            _ => {
                if changed == 12 {
                    command.program = Program::OpsRuntimePython;
                    command.ops_runtime = None;
                }
                let input = fs::File::open(&fixture.request).unwrap();
                assert!(fs::metadata(format!("/dev/fd/{}", input.as_raw_fd())).is_ok());
                input_descriptor = Some(input.as_raw_fd());
                command.stdin = Some(Arc::new(input));
            }
        }
        let mut native = Native::new(
            &fixture.caller,
            &fixture.acquisition.environment,
            Signals::new().unwrap(),
        );
        let result = native.run(command).await;
        if changed == 0 {
            result.unwrap();
            proof.settled().unwrap();
            let captures = proof.captures().unwrap();
            assert_eq!(
                output
                    .capture_read(pending, &captures, &checkpoint)
                    .unwrap(),
                format!("{}\n", "a".repeat(40)).as_bytes()
            );
        } else {
            expect_error(result, if changed == 8 { 124 } else { 1 });
            drop(pending);
        }
        assert_eq!(
            fixture.caller.join("inert-dispatched").exists(),
            changed == 0
        );
        if let Some(descriptor) = input_descriptor {
            assert!(fs::metadata(format!("/dev/fd/{descriptor}")).is_err());
        }
        if let Some(selected) = second {
            Arc::try_unwrap(selected).unwrap().close().unwrap();
        }
        drop(second_permits);
        drop(proof);
        tools.finish().unwrap();
        assert!(native.cleanup_passed());
        assert_eq!(budget.count(), 0);
        let (attempted, fallback) = budget.close_counts();
        assert!(attempted > 0);
        assert_eq!(fallback, 0);
    }
}
