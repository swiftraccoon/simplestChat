//! Owned inert executables only. No package manager, privilege, engine or host.
use super::*;
use crate::{owned_process::Signals, tooling::Environment};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    directory: PathBuf,
    context: plan::Context,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "controller-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let root = directory.join("checkout");
        let temporary = directory.join("temporary");
        fs::create_dir_all(root.join("ops/ansible")).unwrap();
        fs::create_dir(&temporary).unwrap();
        fs::create_dir(directory.join("bin")).unwrap();
        fs::write(
            root.join("ops/ansible/requirements.txt"),
            b"--require-hashes\nFAKE==1\n",
        )
        .unwrap();
        let mut environment = Environment::new();
        for (name, value) in [
            ("ACT", "true"),
            ("LOCAL_CI_DISPOSABLE", "1"),
            ("LOCAL_CI_RUN_ID", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            ("GITHUB_SHA", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            ("RUNNER_TRACKING_ID", "private-tracking"),
            ("PIP_CONSTRAINT", "private-constraint"),
        ] {
            environment.insert(name.into(), value.into());
        }
        environment.insert("RUNNER_TEMP".into(), temporary.as_os_str().into());
        environment.insert("PATH".into(), directory.join("bin").into());
        environment.insert("CALLS".into(), directory.join("calls").into());
        environment.insert("PIP_TEMPLATE".into(), directory.join("pip-template").into());
        let context = plan::Context::new(&root, environment, "Linux").unwrap();
        let value = Self { directory, context };
        value.script(
            "bin/sudo",
            r#"printf 'sudo %s\n' "$*" >> "$CALLS"
printf 'sudo output\n'; printf 'sudo errors\n' >&2
if [ "$2" = update ]; then exit "${FAIL_UPDATE:-0}"; fi
exit "${FAIL_INSTALL:-0}"
"#,
        );
        value.script(
            "bin/python3",
            r#"printf 'python3 %s\n' "$*" >> "$CALLS"
printf 'venv output\n'
if [ "${FAIL_VENV:-0}" != 0 ]; then exit "$FAIL_VENV"; fi
/bin/mkdir -p "$3/bin"
/bin/cp "$PIP_TEMPLATE" "$3/bin/pip"
/bin/chmod 700 "$3/bin/pip"
/bin/ln -s /bin/sh "$3/bin/python"
"#,
        );
        value.script(
            "pip-template",
            r#"printf 'pip %s\n' "$*" >> "$CALLS"
printf 'pip output\n'; printf 'pip errors\n' >&2
exit "${FAIL_PIP:-0}"
"#,
        );
        value
    }
    fn script(&self, path: &str, script: &str) {
        let path = self.directory.join(path);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{script}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.directory.join("calls")).unwrap_or_default()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.context.finish();
        fs::remove_dir_all(&self.directory).unwrap();
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn startup_diagnostics_admit_only_closed_phases_and_never_private_failure_text() {
    for message in [
        "Release controller executable admission failed.",
        "Release controller executable changed during startup.",
        "Release controller namespace preparation failed.",
        "Release controller worker launch failed.",
        "Release controller worker exited before readiness.",
    ] {
        assert_eq!(diagnostic(message), message);
    }
    for private in [
        "private tool output and credential-shaped input",
        "Release controller executable admission failed.\nprivate suffix",
        "Release controller executable admission failed. private suffix",
        "",
    ] {
        assert_eq!(
            diagnostic(private),
            "Release controller preparation failed."
        );
    }
}
#[test]
fn final_signal_and_deadline_fences_preserve_existing_nonzero_primary() {
    for primary in [1, 100, 124, 127, 130, 143] {
        for signal in [None, Some(130), Some(143)] {
            let result = completion(
                Err(crate::tooling::Failure::new(primary, "private primary")),
                signal,
                true,
            );
            assert_eq!(result.unwrap_err().code, primary);
        }
    }
    assert_eq!(completion(Ok(()), Some(130), true).unwrap_err().code, 130);
    assert_eq!(completion(Ok(()), Some(143), false).unwrap_err().code, 143);
    assert_eq!(completion(Ok(()), None, true).unwrap_err().code, 124);
    completion(Ok(()), None, false).unwrap();
    for primary in [1, 100, 124, 127, 130, 143] {
        assert_eq!(worker::outcome(primary, Some(130), false), primary);
        assert_eq!(worker::outcome(primary, Some(143), false), primary);
    }
    assert_eq!(worker::outcome(0, Some(143), false), 143);
    assert_eq!(worker::outcome(0, None, false), 1);
    assert_eq!(worker::outcome(0, None, true), 0);
}
async fn prepared(fixture: &Fixture) -> (files::Owned, protocol::Terminal) {
    let (owned, intent, digest) = files::Owned::create(&fixture.context).unwrap();
    let mut signals = Signals::new().unwrap();
    let (worker, ()) = tokio::join!(worker::execute(&fixture.context, &mut signals), async {
        loop {
            if owned
                .spool
                .json::<protocol::Ready>("ready.json")
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(fixture.calls().is_empty(), "no CLI before parent commit");
        worker::request(&owned, &intent, &digest, protocol::Action::Commit).unwrap();
    });
    worker.unwrap();
    let terminal = owned.spool.json("terminal.json").unwrap().unwrap();
    (owned, terminal)
}
#[test]
fn exact_stage_argv_environment_and_wait_job_budgets_are_preserved() {
    let fixture = Fixture::new();
    let (program, args) = fixture.context.arguments(plan::Stage::Install);
    assert_eq!(program, "sudo");
    assert_eq!(
        args,
        [
            "apt-get",
            "install",
            "-y",
            "python3-venv",
            "openssl",
            "curl",
            "ca-certificates",
            "util-linux"
        ]
        .map(std::ffi::OsString::from)
    );
    assert_eq!(
        fixture.context.environment[std::ffi::OsStr::new("RUNNER_TRACKING_ID")],
        "private-tracking"
    );
    assert_eq!(
        fixture.context.environment[std::ffi::OsStr::new("PIP_CONSTRAINT")],
        "private-constraint"
    );
    assert_eq!(plan::WAIT, Duration::from_secs(300));
    assert_eq!(plan::WORKER, Duration::from_secs(90 * 60));
}
#[test]
fn worker_dispatch_requires_commit_and_closed_evidence_follows_all_four_stages() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let (owned, terminal) = prepared(&fixture).await;
        assert!(terminal.prepared && terminal.cli_groups_settled && terminal.log_closed);
        assert_eq!(terminal.installer_status, 0);
        assert_eq!(fixture.calls().lines().count(), 4);
        assert_eq!(
            owned.parent.read(plan::STATUS, 16).unwrap().unwrap(),
            b"0\n"
        );
        let log = owned
            .parent
            .read(plan::LOG, plan::MAX_LOG)
            .unwrap()
            .unwrap();
        assert_eq!(terminal.log_sha256, plan::digest(&log));
        assert!(String::from_utf8(log).unwrap().contains("pip errors"));
        owned.finish().unwrap();
    });
}
#[test]
fn each_first_failure_stops_later_stages_and_preserves_original_status() {
    for (name, calls) in [
        ("FAIL_UPDATE", 1),
        ("FAIL_INSTALL", 2),
        ("FAIL_VENV", 3),
        ("FAIL_PIP", 4),
    ] {
        let mut fixture = Fixture::new();
        fixture
            .context
            .environment
            .insert(name.into(), "100".into());
        runtime().block_on(async {
            let (owned, terminal) = prepared(&fixture).await;
            assert_eq!(terminal.installer_status, 100);
            assert!(!terminal.prepared);
            assert_eq!(fixture.calls().lines().count(), calls);
            owned.finish().unwrap();
        });
    }
}
#[test]
fn origin_capability_and_four_kib_request_bound_are_enforced() {
    let mut fixture = Fixture::new();
    let (owned, intent, digest) = files::Owned::create(&fixture.context).unwrap();
    let request = protocol::Request {
        intent_sha256: digest.clone(),
        capability: "x".repeat(32),
        action: protocol::Action::Commit,
    };
    owned.spool.publish("commit.json", &request).unwrap();
    assert!(worker::request(&owned, &intent, &digest, protocol::Action::Commit).is_err());
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.resize(4097, b' ');
    owned.spool.atomic("oversized.json", &bytes).unwrap();
    assert!(
        owned
            .spool
            .json_limit::<protocol::Request>("oversized.json", 4096)
            .is_err()
    );
    fixture.context.origin.run = "foreign".into();
    assert!(owned.intent(&fixture.context).is_err());
    owned.finish().unwrap();
}
#[test]
fn retained_private_modes_links_and_namespace_changes_are_refused() {
    let fixture = Fixture::new();
    let (owned, _, _) = files::Owned::create(&fixture.context).unwrap();
    let intent = fixture
        .context
        .temporary
        .join(plan::SPOOL)
        .join("intent.json");
    fs::set_permissions(&intent, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(owned.spool.json::<protocol::Intent>("intent.json").is_err());
    fs::set_permissions(&intent, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(
        &intent,
        fixture
            .context
            .temporary
            .join(plan::SPOOL)
            .join("linked.json"),
    )
    .unwrap();
    assert!(owned.spool.json::<protocol::Intent>("linked.json").is_err());
    fs::set_permissions(
        fixture.context.temporary.join(plan::SPOOL),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(owned.spool.verify().is_err());
    assert!(owned.finish().is_err());
}
#[test]
fn bounded_combined_pipe_limits_actual_file_bytes() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let (owned, intent, _) = files::Owned::create(&fixture.context).unwrap();
        let log = log::Log::start(&owned.parent, &intent, 8).unwrap();
        use std::io::Write;
        let mut writer = log.writer().unwrap();
        writer.write_all(b"0123456789abcdef").unwrap();
        files::close(writer).unwrap();
        let done = log.finish().await.unwrap();
        assert!(!done.complete);
        assert_eq!(done.bytes, 8);
        assert_eq!(
            fs::read(fixture.context.temporary.join(plan::LOG)).unwrap(),
            b"01234567"
        );
        owned.finish().unwrap();
    });
}
#[test]
fn expired_stage_deadline_dispatches_no_child() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let (owned, intent, digest) = files::Owned::create(&fixture.context).unwrap();
        let log = log::Log::start(&owned.parent, &intent, 8).unwrap();
        let result = (process::Invocation {
            context: &fixture.context,
            owned: &owned,
            intent: &intent,
            digest: &digest,
        })
        .run(
            plan::Stage::Update,
            &log,
            &mut process::Control::new(&mut Signals::new().unwrap()),
            Instant::now(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, 124);
        assert!(result.settled);
        assert!(fixture.calls().is_empty());
        log.finish().await.unwrap();
        owned.finish().unwrap();
    });
}
#[test]
fn changed_then_restored_requirements_never_validate_the_original_snapshot() {
    let fixture = Fixture::new();
    let path = fixture.context.root.join("ops/ansible/requirements.txt");
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"changed").unwrap();
    fs::write(&path, original).unwrap();
    assert!(fixture.context.validate().is_err());
}

#[test]
fn blocked_replay_output_has_a_deadline_without_changing_shared_stdout_flags() {
    runtime().block_on(async {
        use std::process::{Command, Stdio};
        let (read, write) = nix::unistd::pipe().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exec /bin/cat"])
            .stdout(Stdio::from(write))
            .stderr(Stdio::null());
        let started = Instant::now();
        let error = replay::run(
            command,
            &vec![b'x'; 1024 * 1024],
            &mut Signals::new().unwrap(),
            Instant::now() + plan::RESERVE + Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, 124);
        assert!(started.elapsed() < Duration::from_secs(3));
        nix::unistd::close(read).unwrap();
    });
}

#[test]
fn replay_copies_raw_bytes_and_queued_cancellation_acquires_no_child() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        use std::process::{Command, Stdio};
        let destination = fixture.directory.join("replayed");
        let mut command = Command::new("/bin/cat");
        command.stdout(Stdio::from(fs::File::create(&destination).unwrap()));
        let bytes = b"raw\0\xff\ncombined";
        replay::run(
            command,
            bytes,
            &mut Signals::new().unwrap(),
            Instant::now() + plan::WAIT,
        )
        .await
        .unwrap();
        assert_eq!(fs::read(destination).unwrap(), bytes);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender.send(130).unwrap();
        let mut signals = Signals::injected(receiver);
        let command = Command::new("/unavailable-inert-program");
        assert_eq!(
            replay::run(command, bytes, &mut signals, Instant::now() + plan::WAIT)
                .await
                .unwrap_err()
                .code,
            130
        );
    });
}

#[derive(Default)]
struct Captured(Vec<u8>);
impl wait::Output for Captured {
    async fn replay(
        &mut self,
        _context: &plan::Context,
        bytes: &[u8],
        _signals: &mut Signals,
        _deadline: Instant,
    ) -> crate::tooling::Result<()> {
        self.0.extend(bytes);
        Ok(())
    }
}
#[test]
fn collector_replays_completed_failure_but_refuses_unbound_log_before_output() {
    let mut fixture = Fixture::new();
    fixture
        .context
        .environment
        .insert("FAIL_UPDATE".into(), "100".into());
    runtime().block_on(async {
        let (owned, _) = prepared(&fixture).await;
        let mut output = Captured::default();
        let error = wait::collect(
            &fixture.context,
            &owned,
            &mut Signals::new().unwrap(),
            Instant::now() + plan::WAIT,
            &mut output,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, 1);
        assert!(!output.0.is_empty());
        fs::write(
            fixture.context.temporary.join(plan::LOG),
            b"PRIVATE_FORGED_LOG",
        )
        .unwrap();
        let mut output = Captured::default();
        assert!(
            wait::collect(
                &fixture.context,
                &owned,
                &mut Signals::new().unwrap(),
                Instant::now() + plan::WAIT,
                &mut output
            )
            .await
            .is_err()
        );
        assert!(output.0.is_empty());
        owned.finish().unwrap();
    });
}
#[test]
fn malformed_terminal_during_cancel_reserve_cannot_replace_known_primary_status() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new();
        runtime().block_on(async {
            let (owned, _, _) = files::Owned::create(&fixture.context).unwrap();
            owned.spool.atomic("terminal.json", b"{malformed").unwrap();
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            if code != 124 {
                sender.send(code).unwrap();
            }
            let mut signals = Signals::injected(receiver);
            let deadline = Instant::now()
                + if code == 124 {
                    plan::RESERVE
                } else {
                    plan::WAIT
                };
            let error = wait::collect(
                &fixture.context,
                &owned,
                &mut signals,
                deadline,
                &mut Captured::default(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, code);
            owned.finish().unwrap();
        });
    }
}

fn idle() -> std::process::Child {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}
#[test]
fn precommit_failure_and_expired_ready_retire_acquired_child_without_handoff() {
    for variant in ["deadline", "wrong-ready", "validation"] {
        let fixture = Fixture::new();
        runtime().block_on(async {
            use std::cell::Cell;
            let pid = Cell::new(None);
            let released = Cell::new(false);
            let checks = Cell::new(0);
            let mut unrelated = idle();
            let result = launcher::start_with(
                &fixture.context,
                &mut Signals::new().unwrap(),
                |_, owned, _, digest| {
                    owned.spool.publish(
                        "ready.json",
                        &protocol::Ready {
                            intent_sha256: if variant == "wrong-ready" {
                                "wrong".into()
                            } else {
                                digest.into()
                            },
                        },
                    )?;
                    let child = idle();
                    pid.set(rustix::process::Pid::from_raw(child.id() as i32));
                    Ok(child)
                },
                |child| {
                    released.set(true);
                    drop(crate::owned_process::Group::new(child));
                },
                |_, _, _| panic!("no commit before accepted startup"),
                || {
                    checks.set(checks.get() + 1);
                    if variant == "validation" && checks.get() == 2 {
                        Err(plan::failure())
                    } else {
                        Ok(())
                    }
                },
                if variant == "deadline" {
                    Duration::ZERO
                } else {
                    plan::STARTUP
                },
            )
            .await;
            assert!(result.is_err());
            assert!(!released.get());
            assert!(matches!(
                rustix::process::waitid(
                    rustix::process::WaitId::Pid(pid.get().unwrap()),
                    rustix::process::WaitIdOptions::EXITED
                        | rustix::process::WaitIdOptions::NOHANG
                        | rustix::process::WaitIdOptions::NOWAIT
                ),
                Err(rustix::io::Errno::CHILD)
            ));
            assert!(unrelated.try_wait().unwrap().is_none());
            drop(crate::owned_process::Group::new(unrelated));
        });
    }
}
#[test]
fn uncertain_visible_commit_never_restores_parent_signal_or_reap_authority() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        use std::cell::{Cell, RefCell};
        let released = Cell::new(false);
        let receiver = RefCell::new(None);
        let result = launcher::start_with(
            &fixture.context,
            &mut Signals::new().unwrap(),
            |_, owned, _, digest| {
                owned.spool.publish(
                    "ready.json",
                    &protocol::Ready {
                        intent_sha256: digest.into(),
                    },
                )?;
                Ok(idle())
            },
            |child| {
                released.set(true);
                *receiver.borrow_mut() = Some(child);
            },
            |owned, intent, digest| {
                assert!(released.get());
                worker::request(owned, intent, digest, protocol::Action::Commit)?;
                Err(plan::failure()) // uncertainty after atomic visibility
            },
            || Ok(()),
            plan::STARTUP,
        )
        .await;
        assert!(result.is_err());
        let mut child = receiver.into_inner().unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "launcher did not kill detached worker"
        );
        let owned = files::Owned::open(&fixture.context).unwrap();
        let (intent, digest) = owned.intent(&fixture.context).unwrap();
        let cancel: protocol::Request = owned.spool.json("cancel.json").unwrap().unwrap();
        assert_eq!(cancel.intent_sha256, digest);
        assert_eq!(cancel.capability, intent.capability);
        owned.finish().unwrap();
        drop(crate::owned_process::Group::new(child));
    });
}
#[test]
fn cancellation_after_disarm_before_commit_uses_only_capability_request() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        use std::cell::RefCell;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let detached = RefCell::new(None);
        let result = launcher::start_with(
            &fixture.context,
            &mut signals,
            |_, owned, _, digest| {
                owned.spool.publish(
                    "ready.json",
                    &protocol::Ready {
                        intent_sha256: digest.into(),
                    },
                )?;
                Ok(idle())
            },
            |child| {
                *detached.borrow_mut() = Some(child);
                sender.send(130).unwrap();
            },
            |_, _, _| panic!("cancelled before commit"),
            || Ok(()),
            plan::STARTUP,
        )
        .await;
        assert_eq!(result.unwrap_err().code, 130);
        let mut child = detached.into_inner().unwrap();
        assert!(child.try_wait().unwrap().is_none());
        drop(crate::owned_process::Group::new(child));
    });
}
#[test]
fn expired_worker_lease_refuses_already_present_valid_commit() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        let (owned, intent, digest) = files::Owned::create(&fixture.context).unwrap();
        worker::request(&owned, &intent, &digest, protocol::Action::Commit).unwrap();
        let mut signals = Signals::new().unwrap();
        let mut control = process::Control::new(&mut signals);
        assert!(
            !worker::committed(&owned, &intent, &digest, &mut control, Instant::now())
                .await
                .unwrap()
        );
        assert_eq!(control.code, Some(124));
        assert!(fixture.calls().is_empty());
        owned.finish().unwrap();
    });
}
#[test]
fn published_record_is_stable_before_postpublication_directory_sync() {
    let fixture = Fixture::new();
    let (owned, _, _) = files::Owned::create(&fixture.context).unwrap();
    owned
        .spool
        .atomic_after("probe.json", b"{\"value\":1}", || {
            assert_eq!(
                owned.spool.read("probe.json", 4096).unwrap().unwrap(),
                b"{\"value\":1}"
            );
        })
        .unwrap();
    owned.finish().unwrap();
}
#[test]
fn pipe_original_descriptors_are_cloexec_in_executed_inert_child() {
    let fixture = Fixture::new();
    runtime().block_on(async {
        use std::process::{Command, Stdio};
        let (owned, intent, _) = files::Owned::create(&fixture.context).unwrap();
        let log = log::Log::start(&owned.parent, &intent, 8).unwrap();
        let [read, write] = log.pipe_fds();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "test ! -e /dev/fd/$CHECK_READ && test ! -e /dev/fd/$CHECK_WRITE",
            ])
            .env("CHECK_READ", read.to_string())
            .env("CHECK_WRITE", write.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        replay::run(
            command,
            b"",
            &mut Signals::new().unwrap(),
            Instant::now() + plan::WAIT,
        )
        .await
        .unwrap();
        assert!(log.finish().await.unwrap().complete);
        owned.finish().unwrap();
    });
}
