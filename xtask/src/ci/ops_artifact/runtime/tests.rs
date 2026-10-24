//! Whole inert controller traces retain the real acquisition, hash, file,
//! worker and paired-capture paths. Supplied receipts are not runtime evidence.

use super::super::namespace::Budget;
use super::{
    execute,
    fixture::{self, Fake, Fixture, expect_error},
    plan::{self, Plan},
    request::Request,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

async fn run(fixture: &Fixture, fake: &mut Fake, budget: &Budget) -> crate::tooling::Result<()> {
    let plan = Plan::new(fake.clock());
    execute::transaction(
        execute::Inputs {
            path: fixture.request.clone(),
            selected: fixture.pin(),
            caller: fixture.caller.clone(),
            environment: fixture.acquisition.environment.clone(),
            run_id: "1".repeat(32),
        },
        budget.clone(),
        &plan,
        fake,
    )
    .await
}

#[tokio::test]
async fn whole_operation_keeps_one_authenticated_transfer_through_final_replay() {
    let fixture = Fixture::new();
    let mut fake = Fake::new(&fixture);
    let budget = Budget::default();
    run(&fixture, &mut fake, &budget).await.unwrap();
    assert_eq!(
        (fake.runtime_calls, fake.acquire.calls, fake.keeper_calls),
        (4, 9, 1)
    );
    assert_eq!(fake.acquire.replays.len(), 1);
    let saved = fs::read(fixture.output.join("summary.json")).unwrap();
    assert_eq!(saved, fake.acquire.replays[0]);
    let summary: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(
        summary["scope"],
        "AuthenticatedOpsDebianRuntimeAndFirstInstall"
    );
    assert_eq!(summary["passed"], true);
    assert_eq!(summary["selection"], fixture.wire["selection"]);
    assert_eq!(summary["artifactId"], 456);
    assert_eq!(summary["cases"].as_array().unwrap().len(), 28);
    assert!(
        !summary
            .as_object()
            .unwrap()
            .contains_key("fullDeploymentValidated")
    );
    let text = String::from_utf8(saved).unwrap();
    for private in [
        "PRIVATE",
        fixture.acquisition.root.to_str().unwrap(),
        "install-request-json",
    ] {
        assert!(!text.contains(private));
    }
    assert_eq!(fake.transfer_members.len(), 3);
    assert!(
        fake.transfer_members
            .iter()
            .all(|(_, mode, _)| *mode == 0o600)
    );
    assert_eq!(
        fake.keeper_arguments[..3],
        ["-I", "-B", "-X"].map(std::ffi::OsString::from)
    );
    assert_eq!(
        fake.keeper_arguments[3],
        std::ffi::OsString::from(format!(
            "pycache_prefix={}",
            fixture.output.join("python-cache").display()
        ))
    );
    assert_eq!(
        fake.keeper_arguments[4],
        fixture.output.join("source/build/security_vm_ops.py")
    );
    assert_eq!(budget.count(), 0);
    let (attempted, fallback) = budget.close_counts();
    assert!(attempted > 0);
    assert_eq!(fallback, 0);
}

#[tokio::test]
async fn failed_selected_ci_attempt_refuses_before_artifact_access_or_keeper() {
    for (status, conclusion) in [
        ("in_progress", Value::Null),
        ("completed", json!("failure")),
        ("completed", json!("cancelled")),
    ] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        let mut attempt: Value = serde_json::from_slice(&fake.acquire.outputs[2]).unwrap();
        attempt["status"] = json!(status);
        attempt["conclusion"] = conclusion;
        fake.acquire.outputs[2] = fixture::bytes(&attempt);
        expect_error(run(&fixture, &mut fake, &budget).await, 1);
        assert_eq!((fake.acquire.calls, fake.keeper_calls), (3, 0));
        assert!(!fixture.output.join("summary.json").exists());
        assert_eq!(budget.count(), 0);
    }
}

#[tokio::test]
async fn every_remote_or_runtime_failure_keeps_its_original_status_and_settles_owners() {
    let messages = [
        "Ops artifact acquisition gh version command failed.",
        "Ops artifact acquisition gh repository command failed.",
        "Ops artifact acquisition gh run command failed.",
        "Ops artifact acquisition gh artifact metadata command failed.",
        "Ops artifact acquisition gh artifact download command failed.",
        "Ops artifact verification gh version command failed.",
        "Ops artifact verification gh repository command failed.",
        "Ops artifact verification gh run command failed.",
        "Ops artifact verification gh attestation command failed.",
        "Ops Debian runtime Git identity command failed.",
        "Ops Debian runtime Git clean-check command failed.",
        "Ops Debian runtime Git source-archive command failed.",
        "Ops Debian runtime keeper command failed.",
    ];
    for (index, message) in messages.into_iter().enumerate() {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        if index < 9 {
            fake.acquire.fail = Some((index, 7));
        } else {
            fake.runtime_fail = Some((index - 9, 7));
        }
        let error = run(&fixture, &mut fake, &budget).await.unwrap_err();
        assert_eq!(error.code, 7);
        assert_eq!(error.message, message);
        assert!(error.is_child_exit());
        assert!(!fixture.output.join("summary.json").exists());
        assert!(fake.acquire.replays.is_empty());
        assert_eq!(budget.count(), 0);
        let (attempted, fallback) = budget.close_counts();
        assert!(attempted > 0);
        assert_eq!(fallback, 0);
    }
}

#[tokio::test]
async fn negative_keeper_receipts_supply_only_bound_allowlisted_failure_labels() {
    let labels = [
        ("guard", "Ops Debian runtime keeper guard failed."),
        ("boot", "Ops Debian runtime keeper boot failed."),
        ("stage", "Ops Debian runtime keeper staging failed."),
        ("cases", "Ops Debian runtime keeper cases failed."),
        ("cleanup", "Ops Debian runtime keeper cleanup failed."),
        (
            "deadline",
            "Ops Debian runtime keeper reported its deadline.",
        ),
        (
            "cancelled",
            "Ops Debian runtime keeper reported cancellation.",
        ),
    ];
    for (label, message) in labels {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        fake.runtime_fail = Some((3, 42));
        fake.receipt_mutate = Some(Box::new(move |wire| {
            wire["passed"] = json!(false);
            wire["failure"] = json!(label);
        }));
        let error = run(&fixture, &mut fake, &budget).await.unwrap_err();
        assert_eq!(error.code, 42);
        assert_eq!(error.message, message);
        assert_eq!(fake.keeper_calls, 1);
        assert!(!fixture.output.join("summary.json").exists());
        assert!(fake.acquire.replays.is_empty());
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
    }
}

#[tokio::test]
async fn keeper_diagnostics_refuse_unbound_arbitrary_or_success_receipts() {
    for index in 0..24 {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        fake.runtime_fail = Some((3, 7));
        fake.receipt_mutate = Some(Box::new(move |wire| {
            wire["passed"] = json!(false);
            wire["failure"] = json!("boot");
            match index {
                0 => wire["schemaVersion"] = json!(2),
                1 => wire["scope"] = json!("other"),
                2 => wire["runId"] = json!("f".repeat(32)),
                3 => wire["selection"]["revision"] = json!("f".repeat(40)),
                4 => wire["selection"]["runAttempt"] = json!(99),
                5 => wire["artifactId"] = json!(99),
                6 => wire["transferSha256"] = json!("f".repeat(64)),
                7 => wire["transferBytes"] = json!(1),
                8 => wire["manifestSha256"] = json!("f".repeat(64)),
                9 => wire["manifestBytes"] = json!(1),
                10 => wire["selectionSha256"] = json!("f".repeat(64)),
                11 => wire["selectionBytes"] = json!(1),
                12 => wire["binarySha256"] = json!("f".repeat(64)),
                13 => wire["binaryBytes"] = json!(1),
                14 => wire["installRequestSha256"] = json!("f".repeat(64)),
                15 => wire["cloudImageSha512"] = json!("f".repeat(128)),
                16 => wire["failure"] = json!("PRIVATE arbitrary command/error"),
                17 => wire["failure"] = Value::Null,
                18 => wire["passed"] = json!(true),
                19 => wire["PRIVATE unexpected"] = json!(true),
                20 => wire["install"]["PRIVATE unexpected"] = json!(true),
                21 => wire["cases"][0]["PRIVATE unexpected"] = json!(true),
                22 => wire["vmLifecycle"]["PRIVATE unexpected"] = json!(true),
                _ => wire["cases"][0]["name"] = json!("PRIVATE".repeat(10000)),
            }
        }));
        let error = run(&fixture, &mut fake, &budget).await.unwrap_err();
        assert_eq!(error.code, 7);
        assert_eq!(error.message, "Ops Debian runtime keeper command failed.");
        assert!(!error.message.contains("PRIVATE"));
        assert!(!fixture.output.join("summary.json").exists());
        assert!(fake.acquire.replays.is_empty());
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
    }
}

#[tokio::test]
async fn closed_keeper_receipt_cannot_replace_original_pins_cases_or_lifecycle() {
    for index in 0..12 {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        fake.receipt_mutate = Some(Box::new(move |wire| match index {
            0 => wire["binarySha256"] = json!("f".repeat(64)),
            1 => wire["manifestBytes"] = json!(1),
            2 => wire["installRequestSha256"] = json!("f".repeat(64)),
            3 => wire["selection"]["runAttempt"] = json!(1),
            4 => wire["cloudImageSha512"] = json!("f".repeat(128)),
            5 => wire["cases"][0]["passed"] = json!(false),
            6 => wire["cases"].as_array_mut().unwrap().swap(0, 1),
            7 => wire["vmLifecycle"]["qmpEofObserved"] = json!(false),
            8 => wire["vmLifecycle"]["qemuReturnCode"] = json!(true),
            9 => wire["install"]["secondChanged"] = json!(true),
            10 => wire["cleanupPassed"] = json!(false),
            _ => wire["PRIVATE-unexpected"] = json!(true),
        }));
        expect_error(run(&fixture, &mut fake, &budget).await, 1);
        assert_eq!(fake.keeper_calls, 1);
        assert!(!fixture.output.join("summary.json").exists());
        assert!(fake.acquire.replays.is_empty());
        assert_eq!(budget.count(), 0);
    }
}

#[tokio::test]
async fn original_witnesses_refuse_mutation_during_keeper_and_after_summary_replay() {
    for during_replay in [false, true] {
        for index in 0..if during_replay { 4 } else { 5 } {
            let fixture = Fixture::new();
            let mut fake = Fake::new(&fixture);
            let budget = Budget::default();
            let target = match index {
                0 => fixture.request.clone(),
                1 => fixture.output.join("acquired/input/simplestchat-ops"),
                2 => fixture.output.join("transfer.tar"),
                3 => fixture.caller.join("build/security_vm_ops_guest.py"),
                _ => fixture.acquisition.root.join("python3"),
            };
            let mutate = move || {
                let mut body = fs::read(&target).unwrap();
                body[0] ^= 1;
                fs::write(&target, body).unwrap();
            };
            if during_replay {
                fake.acquire.replay_mutate = Some(Box::new(mutate));
            } else {
                let mut mutate = Some(mutate);
                fake.mutate = Some(Box::new(move |at| {
                    if at == 3 {
                        mutate.take().unwrap()();
                    }
                }));
            }
            expect_error(run(&fixture, &mut fake, &budget).await, 1);
            assert_eq!(fake.keeper_calls, 1);
            assert_eq!(budget.count(), 0);
            assert_eq!(fixture.output.join("summary.json").exists(), during_replay);
        }
    }
}

#[tokio::test]
async fn replay_refuses_identical_file_or_directory_replacements_without_rebaselining() {
    for index in 0..4 {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        let selected = match index {
            0 => fixture.request.clone(),
            1 => fixture.output.join("acquired"),
            2 => fixture.output.clone(),
            _ => fixture.caller.join("build"),
        };
        let displaced = fixture.acquisition.root.join("displaced-original");
        fake.acquire.replay_mutate = Some(Box::new(move || {
            if index == 0 {
                let bytes = fs::read(&selected).unwrap();
                fs::rename(&selected, &displaced).unwrap();
                fs::write(&selected, bytes).unwrap();
                fs::set_permissions(&selected, fs::Permissions::from_mode(0o600)).unwrap();
            } else {
                fs::rename(&selected, &displaced).unwrap();
                fs::create_dir(&selected).unwrap();
                fs::set_permissions(&selected, fs::Permissions::from_mode(0o700)).unwrap();
                for entry in fs::read_dir(&displaced).unwrap() {
                    let entry = entry.unwrap();
                    fs::rename(entry.path(), selected.join(entry.file_name())).unwrap();
                }
            }
        }));
        expect_error(run(&fixture, &mut fake, &budget).await, 1);
        assert_eq!(fake.acquire.replays.len(), 1);
        assert_eq!(
            fs::read(fixture.output.join("summary.json")).unwrap(),
            fake.acquire.replays[0]
        );
        assert_eq!(budget.count(), 0);
    }
}

#[tokio::test]
async fn replay_signal_deadline_failure_and_uncertain_cleanup_never_become_success() {
    for index in 0..6 {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        let code = match index {
            0 => {
                fake.acquire.replay_fail = Some(7);
                7
            }
            1 => {
                fake.acquire.replay_cancel = 130;
                130
            }
            2 => {
                fake.acquire.replay_cancel = 143;
                143
            }
            3 => {
                fake.acquire.replay_advance = Duration::from_secs(26);
                124
            }
            4 => {
                fake.acquire.cleanup = false;
                1
            }
            _ => {
                fixture::cancelled(&fake, 130);
                130
            }
        };
        expect_error(run(&fixture, &mut fake, &budget).await, code);
        assert_eq!(budget.count(), 0);
        if index < 4 {
            assert!(fixture.output.join("summary.json").is_file());
        } else {
            assert!(!fixture.output.join("summary.json").exists());
        }
    }
}

#[tokio::test]
async fn keeper_setup_headroom_is_fixed_and_not_a_resettable_timeout() {
    let fixture = Fixture::new();
    let mut fake = Fake::new(&fixture);
    let budget = Budget::default();
    // This capture is created only after the keeper phase has acquired its
    // original end. Advancing during transfer would spend transfer time and
    // correctly leave the subsequent keeper's setup slot untouched.
    fake.setup_late = Some(fixture.output.join("process-3.stdout"));
    expect_error(run(&fixture, &mut fake, &budget).await, 124);
    assert_eq!(
        (fake.acquire.calls, fake.runtime_calls, fake.keeper_calls),
        (9, 3, 0)
    );
    assert!(!fixture.output.join("summary.json").exists());
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn request_privacy_checkout_identity_and_source_closure_precede_remote_acquisition() {
    for index in 0..5 {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        match index {
            0 => fs::set_permissions(&fixture.request, fs::Permissions::from_mode(0o644)).unwrap(),
            1 => fake.revision = "b".repeat(40),
            2 => fake.git_status = b" M build/security_vm_ops.py\n".to_vec(),
            3 => fs::write(
                fixture.caller.join("build/security_vm_ops.py"),
                b"different selected source\n",
            )
            .unwrap(),
            _ => {
                let cache = fixture.output.join("python-cache/foreign.pyc");
                fake.mutate = Some(Box::new(move |at| {
                    if at == 0 {
                        fs::write(&cache, b"untrusted ignored bytecode").unwrap();
                    }
                }));
            }
        }
        expect_error(run(&fixture, &mut fake, &budget).await, 1);
        assert_eq!((fake.acquire.calls, fake.keeper_calls), (0, 0));
        assert!(!fixture.output.join("summary.json").exists());
        assert_eq!(budget.count(), 0);
    }
}

#[test]
fn git_archive_binds_exact_head_objects_and_rejects_unmeasured_headers_or_trailers() {
    use super::{
        files::Output,
        sources::{self, Sources},
        worker::Checkpoint,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    let fixture = Fixture::new();
    let budget = Budget::default();
    let clock = Arc::new(fixture::Clock::new());
    let start = *clock.0.lock().unwrap();
    let checkpoint = Checkpoint {
        clock,
        start,
        end: start + Duration::from_secs(60),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let output = Output::new(&fixture.output, &fixture.caller, &budget, &checkpoint).unwrap();
    let sources = Sources::open(&fixture.caller, &output, &budget, &checkpoint).unwrap();
    let revision = "a".repeat(40);
    let original = fixture::source_archive(&fixture.caller, &revision);
    sources.archive(&original, &revision).unwrap();
    for index in 0..6 {
        let mut bytes = original.clone();
        match index {
            0 => bytes[512 + 11] ^= 1,
            1 => bytes[1536] ^= 1,
            2 => bytes.push(1),
            3 => {
                let _ = bytes.pop();
            }
            4 => bytes.drain(..1024).for_each(drop),
            _ => bytes[0] ^= 1,
        }
        expect_error(sources.archive(&bytes, &revision), 1);
    }
    for (name, kind, body, accepted) in [
        ("build/", b'5', b"".as_slice(), true),
        ("foreign/", b'5', b"", false),
        ("extra", b'x', b"", false),
        ("build/security_vm_ops.py", b'0', b"duplicate", false),
    ] {
        let mut builder = tar::Builder::new(Vec::new());
        fixture::tar_entry(
            &mut builder,
            "pax_global_header",
            b'g',
            format!("52 comment={revision}\n").as_bytes(),
        );
        fixture::tar_entry(&mut builder, name, kind, body);
        for name in sources::NAMES {
            fixture::tar_entry(
                &mut builder,
                name,
                b'0',
                &fs::read(fixture.caller.join(name)).unwrap(),
            );
        }
        builder.finish().unwrap();
        let bytes = builder.into_inner().unwrap();
        assert_eq!(sources.archive(&bytes, &revision).is_ok(), accepted);
    }
    fs::write(
        fixture.caller.join("build/security_vm_ops.py"),
        b"observable later source mutation",
    )
    .unwrap();
    expect_error(sources.check(&checkpoint), 1);
    assert_eq!(budget.count(), 0);
}

#[test]
fn runtime_request_is_original_pinned_closed_and_does_not_accept_target_or_host_overrides() {
    let fixture = Fixture::new();
    let original = fixture::bytes(&fixture.wire);
    assert!(Request::decode(&original, &fixture.pin()).is_ok());
    let mut changed = original.clone();
    changed.push(b' ');
    expect_error(Request::decode(&changed, &fixture.pin()), 1);
    for key in ["host", "executable", "root", "timeout", "script"] {
        let mut wire = fixture.wire.clone();
        wire[key] = json!("PRIVATE");
        let body = fixture::bytes(&wire);
        expect_error(Request::decode(&body, &super::super::sha(&body)), 1);
    }
    for value in [
        json!(true),
        json!(1.0),
        json!(0),
        json!(9_007_199_254_740_992u64),
    ] {
        let mut wire = fixture.wire.clone();
        wire["artifactId"] = value;
        let body = fixture::bytes(&wire);
        expect_error(Request::decode(&body, &super::super::sha(&body)), 1);
    }
    assert!(original.len() < super::request::MAXIMUM as usize);
}

#[test]
fn complete_phase_tail_and_keeper_dispatch_preserve_the_original_entry() {
    let clock = std::sync::Arc::new(fixture::Clock::new());
    let plan = Plan::new(clock.clone());
    assert_eq!(
        plan::PHASES.iter().copied().sum::<Duration>(),
        Duration::from_secs(3900)
    );
    assert_eq!(plan::ENTRY, Duration::from_secs(4200));
    expect_error(plan.begin(1), 1);
    let end = plan.begin(0).unwrap();
    clock.advance(Duration::from_secs(150));
    expect_error(plan.complete(0, end), 124);
    let clock = std::sync::Arc::new(fixture::Clock::new());
    let plan = Plan::new(clock.clone());
    clock.advance(Duration::from_secs(301));
    expect_error(plan.begin(0), 124);
}

#[tokio::test]
async fn cancelled_file_worker_joins_and_explicitly_closes_a_late_successful_owner() {
    use super::super::namespace::Held;
    use super::worker;
    use crate::{
        ci::{command::Command, process::Executor},
        tooling::Result,
    };
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    };
    struct Control {
        cancelled: Arc<AtomicU8>,
        observed: Arc<AtomicBool>,
    }
    impl Executor for Control {
        async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
            panic!("file worker must not dispatch")
        }
        async fn ready(&mut self) -> Result<()> {
            let code = self.cancelled.load(Ordering::SeqCst);
            if code == 0 {
                Ok(())
            } else {
                self.observed.store(true, Ordering::SeqCst);
                Err(super::failure(code))
            }
        }
    }
    let budget = Budget::default();
    let worker_budget = budget.clone();
    let cancelled = Arc::new(AtomicU8::new(0));
    let observed = Arc::new(AtomicBool::new(false));
    let mut control = Control {
        cancelled: cancelled.clone(),
        observed: observed.clone(),
    };
    let clock = Arc::new(fixture::Clock::new());
    let start = *clock.0.lock().unwrap();
    let plan = Plan::new(clock);
    let (ready, started) = tokio::sync::oneshot::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = gate.clone();
    let work = worker::work(
        &plan,
        &mut control,
        start + Duration::from_secs(60),
        move |checkpoint| {
            checkpoint.check()?;
            let held = Held::root(&worker_budget)?;
            let _ = ready.send(());
            let (lock, wake) = &*worker_gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            // Deliberately late success: the owner must be settled even though the
            // supervisor already observed cancellation while this worker was held.
            Ok(held)
        },
        Held::finish,
    );
    let interrupt = async {
        started.await.unwrap();
        cancelled.store(130, Ordering::SeqCst);
        for _ in 0..1000 {
            if observed.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let (lock, wake) = &*gate;
        *lock.lock().unwrap() = true;
        wake.notify_one();
    };
    let (result, ()) = tokio::join!(work, interrupt);
    expect_error(result, 130);
    assert!(observed.load(Ordering::SeqCst));
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts(), (1, 0));
}
