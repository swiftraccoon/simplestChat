//! Actual inert selected executables and reserved Native groups; no engine/TLS.

use super::{fixture::*, *};
use crate::{
    ci::process::{Executor, Native},
    owned_process::Signals,
};
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

fn script(f: &Fixture, body: &str) {
    fs::write(&f.tool, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&f.tool, fs::Permissions::from_mode(0o700)).unwrap();
}
fn signals() -> Signals {
    let (_, rx) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(rx)
}
#[tokio::test]
async fn native_keeps_actual_failure_status_and_both_finished_capture_seals() {
    for code in [0, 42, 124, 130, 143] {
        let f = Fixture::new();
        script(&f, &format!("printf '%s' 'fixed-output'; exit {code}"));
        let tools = tools::Tools::open(&f.tool).unwrap();
        let time = plan::Time::new(Clock::new());
        let scratch = files::Scratch::open(&f.root).unwrap();
        let mut capture = scratch.capture().await.unwrap();
        let (mut command, proof) = tools.command(&f.config.caddy, &time, 0).unwrap();
        capture.attach(&mut command).unwrap();
        let env = crate::tooling::Environment::new();
        let mut native = Native::new(&f.root, &env, signals());
        let result = native.run(command).await;
        let clean = native.cleanup_passed();
        drop(native);
        let bytes = capture.finish(proof.seals());
        drop(proof);
        let tool = tools.finish();
        let scratch = scratch.finish();
        assert_eq!(result.map_or_else(|e| e.code, |_| 0), code);
        assert!(clean);
        assert_eq!(bytes.unwrap(), b"fixed-output");
        tool.unwrap();
        scratch.unwrap();
    }
}
#[tokio::test]
async fn overflow_spawn_failure_and_wrong_recipe_retire_logs_without_a_positive_seal() {
    for kind in [
        "overflow", "spawn", "recipe", "host", "role", "timeout", "grace", "cleanup",
    ] {
        let f = Fixture::new();
        if kind == "spawn" {
            fs::write(&f.tool, b"#!/nonexistent-owned-interpreter\n").unwrap();
        } else if kind == "overflow" {
            script(
                &f,
                "trap '' PIPE\ni=0; while [ \"$i\" -lt 70000 ]; do printf x; i=$((i+1)); done",
            );
        } else {
            script(
                &f,
                &format!(
                    "printf attempted > '{}'; printf fixed-output",
                    f.root.join("dispatched").display()
                ),
            );
        }
        let tools = tools::Tools::open(&f.tool).unwrap();
        let time = plan::Time::new(Clock::new());
        let scratch = files::Scratch::open(&f.root).unwrap();
        let mut capture = scratch.capture().await.unwrap();
        let (mut command, proof) = tools.command(&f.config.caddy, &time, 0).unwrap();
        capture.attach(&mut command).unwrap();
        match kind {
            "recipe" => command.arguments.push("--force".into()),
            "host" => command.arguments[1] = "unix:///foreign.sock".into(),
            "role" => command.program = crate::ci::command::Program::Curl,
            "timeout" => command.timeout = Duration::from_secs(6),
            "grace" => command.grace = Duration::from_secs(2),
            "cleanup" => command.cleanup = true,
            _ => {}
        }
        let env = crate::tooling::Environment::new();
        let mut native = Native::new(&f.root, &env, signals());
        let result = native.run(command).await;
        let clean = native.cleanup_passed();
        drop(native);
        let sealed = capture.finish(proof.seals());
        drop(proof);
        let tool = tools.finish();
        let scratch = scratch.finish();
        assert!(result.is_err(), "{kind}");
        assert!(clean);
        tool.unwrap();
        scratch.unwrap();
        assert!(!f.root.join("dispatched").exists(), "{kind}");
        if kind != "spawn" {
            assert!(sealed.is_err(), "{kind}");
        } else {
            assert_eq!(sealed.unwrap(), b"");
        }
    }
}
#[tokio::test]
async fn signals_and_the_actual_five_second_timeout_keep_the_same_group_until_retired() {
    for code in [130, 143, 124] {
        let f = Fixture::new();
        script(
            &f,
            &format!(
                "printf '%s' \"$$\" > '{}'; exec /bin/sleep 30",
                f.root.join("ready").display()
            ),
        );
        let tools = tools::Tools::open(&f.tool).unwrap();
        let time = plan::Time::new(Arc::new(plan::RealClock));
        let scratch = files::Scratch::open(&f.root).unwrap();
        let mut capture = scratch.capture().await.unwrap();
        let (mut command, proof) = tools.command(&f.config.caddy, &time, 0).unwrap();
        capture.attach(&mut command).unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let env = crate::tooling::Environment::new();
        let mut native = Native::new(&f.root, &env, Signals::injected(rx));
        let cancel = async {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !f.root.join("ready").is_file() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            if code != 124 {
                tx.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(native.run(command), cancel);
        let clean = native.cleanup_passed();
        drop(native);
        let sealed = capture.finish(proof.seals());
        drop(proof);
        let tool = tools.finish();
        let scratch = scratch.finish();
        assert_eq!(result.unwrap_err().code, code);
        assert!(clean);
        sealed.unwrap();
        tool.unwrap();
        scratch.unwrap();
        let pid = fs::read_to_string(f.root.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}
