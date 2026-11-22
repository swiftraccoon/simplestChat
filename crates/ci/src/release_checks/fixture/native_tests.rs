//! The native runner with inert shell children: statuses, private streams and
//! records, bounds, the stop sequence, signals in both windows, input and an
//! inherited descriptor. No engine, browser or network is used.

use super::super::{
    host::{Commands, Fault, Paths, Request, Window},
    native::{self, Native, Platform},
};
use super::Scratch;
use simplestchat_toolkit::{executable::Executable, owned_process::Signals};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    time::{Duration, Instant},
};

struct Owned {
    scratch: Scratch,
    commands: Commands<Native>,
    signal: tokio::sync::mpsc::UnboundedSender<u8>,
}

fn start(work: Duration) -> Owned {
    let scratch = Scratch::new("native");
    let private = scratch.0.join("private");
    fs::create_dir(&private).unwrap();
    let policy = scratch.0.join("policy");
    fs::write(&policy, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&policy, fs::Permissions::from_mode(0o700)).unwrap();
    let (signal, receiver) = tokio::sync::mpsc::unbounded_channel();
    let native = Native::new(
        Signals::injected(receiver),
        &scratch.0,
        Executable::open(&policy).unwrap(),
    );
    let commands = Commands::new(native, &private, Instant::now() + work);
    Owned {
        scratch,
        commands,
        signal,
    }
}

fn shell(script: &str) -> Request {
    Request::new("sh", vec!["/bin/sh".into(), "-c".into(), script.into()])
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn gone(pid_file: &Path) -> bool {
    let pid: i32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
}

#[test]
fn a_child_keeps_its_status_private_streams_and_record_and_a_fixed_environment() {
    let mut owned = start(Duration::from_secs(60));
    runtime().block_on(async {
        let mut request = shell("printf out; printf err >&2; env; exit 3");
        request.check = false;
        request.environment = vec![("ADDED".into(), "value".into())];
        let outcome = owned.commands.run(request).await.unwrap();
        assert_eq!(outcome.status, Some(3));
        let stdout = fs::read_to_string(&outcome.stdout).unwrap();
        assert!(
            stdout.starts_with("out")
                && stdout.contains("LC_ALL=C\n")
                && stdout.contains("ADDED=value")
        );
        assert!(
            stdout.contains("PATH=/usr/sbin:/usr/bin:/sbin:/bin\n") && !stdout.contains("HOME=")
        );
        assert_eq!(fs::read_to_string(&outcome.stderr).unwrap(), "err");
        let private = owned.scratch.0.join("private");
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(private.join("command-001.json")).unwrap()).unwrap();
        assert_eq!(record["exitStatus"], 3);
        assert_eq!(record["timeoutSeconds"], 30.0);
        for name in [
            "command-001.json",
            "command-001.stdout",
            "command-001.stderr",
        ] {
            let mode = fs::metadata(private.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name}");
        }
        let failed = owned.commands.last_failure.clone().unwrap();
        assert_eq!(
            (failed.number, failed.operation, failed.exit_status),
            (1, "sh", 3)
        );
        assert_eq!(
            owned.commands.run(shell("exit 4")).await.unwrap_err(),
            Fault::Command(2)
        );
        assert!(!owned.commands.settlement_unconfirmed && !owned.commands.interrupted);
    });
}

#[test]
fn a_child_that_outlives_its_bound_is_stopped_and_its_settlement_stays_unconfirmed() {
    let mut owned = start(Duration::from_secs(60));
    let pid = owned.scratch.0.join("pid");
    runtime().block_on(async {
        let mut request = shell(&format!(
            "trap '' TERM; echo $$ > '{}'; sleep 30",
            pid.display()
        ));
        request.timeout = Duration::from_millis(300);
        request.grace = Duration::from_millis(200);
        let started = Instant::now();
        assert_eq!(
            owned.commands.run(request).await.unwrap_err(),
            Fault::TimedOut(1)
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(owned.commands.settlement_unconfirmed && !owned.commands.interrupted);
    });
    assert!(gone(&pid));
    let record = fs::read_to_string(owned.scratch.0.join("private/command-001.json")).unwrap();
    assert!(record.contains("\"exitStatus\": null"));
}

#[test]
fn a_signal_or_the_work_deadline_interrupts_work_but_only_latches_during_cleanup() {
    let mut owned = start(Duration::from_secs(60));
    let pid = owned.scratch.0.join("pid");
    runtime().block_on(async {
        let request = shell(&format!("echo $$ > '{}'; sleep 30", pid.display()));
        let sender = owned.signal.clone();
        let signal = async {
            while !pid.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            sender.send(143).unwrap();
        };
        let (result, ()) = tokio::join!(owned.commands.run(request), signal);
        assert_eq!(result.unwrap_err(), Fault::Interrupted);
        assert!(owned.commands.interrupted && owned.commands.settlement_unconfirmed);
        // A later command of the same work is refused before it starts.
        assert_eq!(
            owned.commands.run(shell("exit 0")).await.unwrap_err(),
            Fault::Interrupted
        );
    });
    assert!(gone(&pid));
    let mut cleanup = owned_cleanup();
    let running = cleanup.scratch.0.join("running");
    runtime().block_on(async {
        let request = shell(&format!(
            "echo $$ > '{}'; /bin/sleep 0.3",
            running.display()
        ));
        let sender = cleanup.signal.clone();
        let signal = async {
            while !running.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            sender.send(130).unwrap();
        };
        let (outcome, ()) = tokio::join!(cleanup.commands.run(request), signal);
        assert_eq!(outcome.unwrap().status, Some(0));
        assert!(super::super::host::Runner::latched(
            &cleanup.commands.runner
        ));
        assert!(!cleanup.commands.interrupted && !cleanup.commands.settlement_unconfirmed);
    });
    let mut late = start(Duration::from_millis(300));
    runtime().block_on(async {
        assert_eq!(
            late.commands.run(shell("sleep 30")).await.unwrap_err(),
            Fault::Interrupted
        );
        assert!(late.commands.settlement_unconfirmed);
    });
}

fn owned_cleanup() -> Owned {
    let mut owned = start(Duration::from_secs(60));
    let end = owned.commands.now() + Duration::from_secs(60);
    owned.commands.cleanup(end);
    owned
}

#[test]
fn cleanup_admits_a_command_only_with_room_for_its_stop_and_reap() {
    let mut owned = start(Duration::from_secs(60));
    let end = owned.commands.now() + Duration::from_secs(8);
    owned.commands.cleanup(end);
    runtime().block_on(async {
        assert_eq!(
            owned.commands.run(shell("exit 0")).await.unwrap_err(),
            Fault::Exhausted
        );
        let end = owned.commands.now() + Duration::from_secs(20);
        owned.commands.cleanup(end);
        owned.commands.run(shell("exit 0")).await.unwrap();
    });
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(owned.scratch.0.join("private/command-001.json")).unwrap(),
    )
    .unwrap();
    let admitted = record["timeoutSeconds"].as_f64().unwrap();
    assert!(admitted <= 10.0 && admitted > 9.0, "{admitted}");
}

#[test]
fn input_and_one_inherited_descriptor_reach_the_child_and_nothing_else_starts() {
    let mut owned = start(Duration::from_secs(60));
    let input = owned.scratch.0.join("input");
    fs::write(&input, "from input\n").unwrap();
    let held = owned.scratch.0.join("held");
    fs::write(&held, "held open\n").unwrap();
    runtime().block_on(async {
        let mut request = shell("cat");
        request.input = Some(input.clone());
        assert_eq!(owned.commands.text(request).await.unwrap(), "from input");
        let descriptor = std::os::fd::OwnedFd::from(fs::File::open(&held).unwrap());
        let number = std::os::fd::AsRawFd::as_raw_fd(&descriptor);
        let mut request = shell(&format!("cat <&{number}"));
        request.inherit = Some(descriptor);
        assert_eq!(owned.commands.text(request).await.unwrap(), "held open");
        let link = owned.scratch.0.join("link");
        symlink(&input, &link).unwrap();
        let mut request = shell("cat");
        request.input = Some(link);
        assert_eq!(
            owned.commands.run(request).await.unwrap_err(),
            Fault::Check("Cannot start a fixture command")
        );
        let missing = Request::new("missing", vec![owned.scratch.0.join("absent").into()]);
        assert_eq!(
            owned.commands.run(missing).await.unwrap_err(),
            Fault::Check("Cannot start a fixture command")
        );
    });
}

#[test]
fn preflight_refuses_an_unsuitable_host_before_probing_anything_later() {
    let scratch = Scratch::new("preflight");
    let paths = Paths {
        config: scratch.0.join("etc"),
        root: scratch.0.join("srv"),
        work: scratch.0.join("run"),
        bench: scratch.0.join("bench"),
        trust: scratch.0.join("trust"),
        checkout: scratch.0.clone(),
    };
    let tool = scratch.0.join("tool");
    fs::write(&tool, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    let tools = [tool.to_str().unwrap()];
    let linux = Platform {
        system: "Linux",
        machine: "x86_64",
        root: true,
    };
    let never = || -> std::io::Result<()> { panic!("probed after a refusal") };
    let never_free = || -> std::io::Result<u64> { panic!("probed after a refusal") };
    let host = "Use a fresh disposable Linux amd64/arm64 host as root; never the public VPS";
    for platform in [
        Platform {
            system: "Darwin",
            machine: "arm64",
            root: true,
        },
        Platform {
            system: "Linux",
            machine: "riscv64",
            root: true,
        },
        Platform {
            system: "Linux",
            machine: "aarch64",
            root: false,
        },
    ] {
        assert_eq!(
            native::preflight(&platform, &paths, &tools, never, never_free),
            Err(Fault::Check(host))
        );
    }
    for machine in ["x86_64", "amd64", "aarch64", "arm64"] {
        let platform = Platform {
            system: "Linux",
            machine,
            root: true,
        };
        assert!(native::preflight(&platform, &paths, &tools, || Ok(()), || Ok(u64::MAX)).is_ok());
    }
    symlink(scratch.0.join("missing"), &paths.bench).unwrap();
    let existing = "Existing service or benchmark path; refusing fixture setup";
    assert_eq!(
        native::preflight(&linux, &paths, &tools, never, never_free),
        Err(Fault::Check(existing))
    );
    fs::remove_file(&paths.bench).unwrap();
    let absent = [scratch.0.join("absent")];
    let absent = [absent[0].to_str().unwrap()];
    let missing = "Required Docker, TLS or namespace tooling is missing";
    assert_eq!(
        native::preflight(&linux, &paths, &absent, never, never_free),
        Err(Fault::Check(missing))
    );
    let busy = || Err(std::io::Error::other("in use"));
    assert_eq!(
        native::preflight(&linux, &paths, &tools, busy, never_free),
        Err(Fault::Check("A fixture port is already in use"))
    );
    let small = || Ok(4 * 1024 * 1024 * 1024 - 1);
    assert_eq!(
        native::preflight(&linux, &paths, &tools, || Ok(()), small),
        Err(Fault::Check(
            "At least 4 GiB of free fixture space is required"
        ))
    );
    let _ = Window::Work(Instant::now());
}

#[test]
fn a_stopped_child_gets_its_grace_to_finish_its_own_cleanup_after_term() {
    let mut owned = start(Duration::from_secs(60));
    let cleaned = owned.scratch.0.join("cleaned");
    runtime().block_on(async {
        let script = format!(
            "trap 'echo owned > \"{}\"; exit 0' TERM; /bin/sleep 30 & wait",
            cleaned.display()
        );
        let mut request = shell(&script);
        request.timeout = Duration::from_millis(300);
        request.grace = Duration::from_secs(5);
        assert_eq!(
            owned.commands.run(request).await.unwrap_err(),
            Fault::TimedOut(1)
        );
    });
    assert_eq!(fs::read_to_string(&cleaned).unwrap(), "owned\n");
}

#[test]
fn the_response_check_runs_only_while_its_retained_executable_is_unchanged() {
    use super::super::host::Runner;
    let mut owned = start(Duration::from_secs(60));
    let policy = owned.scratch.0.join("policy");
    owned.commands.runner.retained(&policy).unwrap();
    assert_eq!(
        owned
            .commands
            .runner
            .retained(&owned.scratch.0.join("other")),
        Err(Fault::Check("HTTP policy binary changed"))
    );
    fs::write(&policy, "#!/bin/sh\nexit 1\n").unwrap();
    assert_eq!(
        owned.commands.runner.retained(&policy),
        Err(Fault::Check("HTTP policy binary changed"))
    );
}
