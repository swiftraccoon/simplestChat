//! Fake engines prove sandboxing, collection, exact ownership and failure evidence.

use super::{
    clock::Clock,
    context::Context,
    controller,
    fixtures::{Directory, FakeClock, FakeEngine},
    options::Options,
    ownership::Ownership,
    process::Cancellation,
    step,
    types::*,
};
use std::sync::Arc;

async fn measured_step(
    engine: &FakeEngine,
    directory: &Directory,
    cancellation: &Cancellation,
) -> StepResult {
    let ownership = Ownership::new(&directory.0, "fixture");
    let browser = Browser::default();
    let context = Context {
        engine,
        clock: engine.clock.as_ref(),
        cancellation,
        ownership: &ownership,
        run_id: "fixture",
        server_image: "server",
        generator_image: "generator",
        revisions: ("s".into(), "g".into()),
        shape: Shape {
            server_cpus: 1.0,
            workers: 1,
            generator_cpus: 2.8,
        },
        worker_threshold: 0.7,
        server_memory: "1024m".into(),
        generator_memory: "2048m".into(),
        browser: &browser,
        output: &directory.0,
        server_env: &[],
    };
    step::run(
        &context,
        Plan {
            workload: Workload::Meetings,
            size: 10,
            meeting_size: 5,
            warmup_seconds: 20,
            duration_seconds: 30,
            workers: 1,
        },
        1,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn healthy_fake_step_covers_window_and_collects_before_both_exact_removals() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    let result = measured_step(&engine, &directory, &Cancellation::default()).await;
    assert!(result.error.is_empty(), "{}", result.error);
    assert!(result.generator_passed);
    assert_eq!(result.load_scrapes, 2);
    assert_eq!(result.server_cores, 0.25);
    assert_eq!(result.generator_cores, 0.5);
    assert_eq!(result.worker_load_peak[&0], 0.25);
    assert_eq!(result.memory_peak_bytes, 104857600);
    let step = directory.0.join("01-meetings-10");
    assert!(step.join("collection.json").is_file());
    assert!(!step.join("generator-results.tar").exists());
    assert!(step.join("step.json").is_file());
    assert!(step.join("generator/load_test_results.json").is_file());
    let state = engine.state.lock().unwrap();
    let runs = state
        .calls
        .iter()
        .filter(|(args, _)| args[0] == "run")
        .map(|(args, _)| args)
        .collect::<Vec<_>>();
    assert!(runs[0].windows(2).any(|pair| pair == ["--network", "none"]));
    assert!(
        runs[1]
            .windows(2)
            .any(|pair| pair == ["--network", &format!("container:{:064x}", 1)])
    );
    for args in runs {
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--publish" || arg == "--privileged" || arg == "--network=host")
        );
        for flag in [
            "--read-only",
            "--cap-drop",
            "--security-opt",
            "--memory-swap",
            "--pids-limit",
        ] {
            assert!(args.iter().any(|arg| arg == flag));
        }
    }
    let removed = state
        .calls
        .iter()
        .filter(|(args, _)| args[0] == "rm")
        .collect::<Vec<_>>();
    assert_eq!(removed.len(), 2);
    assert_eq!(removed[0].0.last().unwrap(), &format!("{:064x}", 2));
    assert!(
        removed
            .iter()
            .all(|(args, cleanup)| *cleanup && args.last().unwrap().len() == 64)
    );
    assert!(
        state
            .calls
            .iter()
            .position(|(args, _)| args[0] == "cp")
            .unwrap()
            < state
                .calls
                .iter()
                .position(|(args, _)| args[0] == "rm")
                .unwrap()
    );
}

#[tokio::test]
async fn failed_transfer_keeps_private_archive_and_never_claims_complete_evidence() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    engine.state.lock().unwrap().fail_transfer = true;
    let result = measured_step(&engine, &directory, &Cancellation::default()).await;
    assert!(result.error.contains("evidence collection failed"));
    let step = directory.0.join("01-meetings-10");
    assert!(step.join("generator-results.tar").is_file());
    assert!(step.join("collection.stderr").is_file());
    assert!(!step.join("generator").exists());
    assert!(!step.join("collection.json").exists());
    assert!(step.join("generator.log").is_file());
    assert!(step.join("server.log").is_file());
    assert_eq!(
        engine
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(args, _)| args[0] == "rm")
            .count(),
        2
    );
}

#[tokio::test]
async fn uncertain_creation_response_is_removed_only_after_matching_intent_identity() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    engine.state.lock().unwrap().failed_server_response = true;
    let result = measured_step(&engine, &directory, &Cancellation::default()).await;
    assert!(result.error.contains("failed after creation"));
    let state = engine.state.lock().unwrap();
    let remove = state
        .calls
        .iter()
        .find(|(args, _)| args[0] == "rm")
        .unwrap();
    assert_eq!(remove.0.last().unwrap(), &format!("{:064x}", 1));
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.0.join("ownership.json")).unwrap())
            .unwrap();
    assert_eq!(journal["containers"][0]["id"], serde_json::Value::Null);
}

#[tokio::test]
async fn cancellation_preserves_step_and_attempts_both_removals_after_first_failure() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock {
        interrupt_at: Some(2.0),
        ..FakeClock::default()
    }));
    engine.state.lock().unwrap().fail_remove_generator = true;
    let cancellation = Cancellation::default();
    let result = measured_step(&engine, &directory, &cancellation).await;
    assert_eq!(cancellation.code(), 143);
    assert!(result.error.contains("interrupted"));
    assert!(directory.0.join("01-meetings-10/step.json").is_file());
    assert_eq!(
        engine
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(args, _)| args[0] == "rm")
            .count(),
        2
    );
}

#[tokio::test]
async fn missing_marks_or_worker_scrapes_cannot_become_a_capacity_pass() {
    for marks in [true, false] {
        let directory = Directory::new();
        let engine = FakeEngine::new(Arc::new(FakeClock::default()));
        {
            let mut state = engine.state.lock().unwrap();
            state.missing_marks = marks;
            state.missing_loads = !marks;
        }
        let result = measured_step(&engine, &directory, &Cancellation::default()).await;
        assert!(!result.error.is_empty());
        if !marks {
            assert!(result.error.contains("busiest worker is unknown"));
        }
        assert!(!super::judge::judge(&result, Limits::default()).valid);
    }
}

#[tokio::test]
async fn full_fake_calibration_preserves_profiles_bounds_reports_and_journal() {
    let directory = Directory::new();
    let output = directory.0.join("run");
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    let options = Options {
        server_image: "server".into(),
        generator_image: "generator".into(),
        quick: true,
        steps: Some(1),
        monthly_price: Some(0.0),
        ..Options::default()
    };
    controller::run(
        &options,
        &engine,
        engine.clock.as_ref(),
        &Cancellation::default(),
        &output,
    )
    .await
    .unwrap();
    let bytes = std::fs::read(output.join("calibration.json")).unwrap();
    let report = crate::capacity::json::parse(&bytes).unwrap();
    assert_eq!(report["schemaVersion"].integer("schema").unwrap(), 1);
    assert_eq!(report["steps"].array("steps").unwrap().len(), 3);
    assert_eq!(
        report["measurement"]["browser"]["capture"].string(),
        Some("720p")
    );
    assert_eq!(
        report["ceilings"]["meetings"]["bound"].string(),
        Some("step-limited")
    );
    assert!(report["projection"]["lowerBound"].is_true());
    assert_eq!(
        report["cost"]["participantsPerMonthlyDollar"]
            .number("cost")
            .unwrap(),
        f64::INFINITY
    );
    assert!(!bytes.ends_with(b"\n"));
    assert!(output.join("ownership.json").is_file());
    assert!(!output.join("failure.json").exists());
    assert!(engine.clock.now() > 100.0);
}

#[tokio::test]
async fn changed_run_identity_is_never_stopped_copied_or_removed() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    engine.state.lock().unwrap().identity_mismatch = true;
    let result = measured_step(&engine, &directory, &Cancellation::default()).await;
    assert!(result.error.contains("identity mismatch"));
    let state = engine.state.lock().unwrap();
    assert!(
        !state
            .calls
            .iter()
            .any(|(args, _)| ["stop", "cp", "rm", "logs"].contains(&args[0].as_str()))
    );
    assert!(directory.0.join("ownership.json").is_file());
    assert!(directory.0.join("01-meetings-10/step.json").is_file());
}

#[tokio::test]
async fn interrupted_calibration_publishes_partial_honest_report_without_more_workloads() {
    let directory = Directory::new();
    let output = directory.0.join("run");
    let engine = FakeEngine::new(Arc::new(FakeClock {
        interrupt_at: Some(2.0),
        ..FakeClock::default()
    }));
    let cancellation = Cancellation::default();
    let options = Options {
        server_image: "server".into(),
        generator_image: "generator".into(),
        quick: true,
        ..Options::default()
    };
    controller::run(
        &options,
        &engine,
        engine.clock.as_ref(),
        &cancellation,
        &output,
    )
    .await
    .unwrap();
    assert_eq!(cancellation.code(), 143);
    let report = crate::capacity::json::read(&output.join("calibration.json")).unwrap();
    assert_eq!(report["steps"].array("steps").unwrap().len(), 1);
    assert_eq!(
        report["ceilings"]["meetings"]["bound"].string(),
        Some("interrupted")
    );
    assert_eq!(
        report["ceilings"]["webinar"]["bound"].string(),
        Some("not-run")
    );
    assert_eq!(
        report["projection"]["meetingParticipants"]
            .integer("participants")
            .unwrap(),
        0
    );
    assert!(!report["steps"].array("steps").unwrap()[0]["valid"].is_true());
    let state = engine.state.lock().unwrap();
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|(args, _)| args[0] == "run")
            .count(),
        3
    );
    let stops = state
        .calls
        .iter()
        .enumerate()
        .filter(|(_, (args, _))| args[0] == "stop")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let transfer = state
        .calls
        .iter()
        .position(|(args, _)| args[0] == "cp")
        .unwrap();
    assert_eq!(stops.len(), 2);
    assert!(stops.iter().all(|index| *index < transfer));
}

#[tokio::test]
async fn a_generator_overrun_is_bounded_and_invalidates_its_otherwise_passing_summary() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    engine.state.lock().unwrap().never_finish = true;
    let result = measured_step(&engine, &directory, &Cancellation::default()).await;
    assert_eq!(
        result.error,
        "the generator did not finish before its deadline"
    );
    assert!(engine.clock.now() < 260.0);
    assert!(!super::judge::judge(&result, Limits::default()).valid);
    assert_eq!(
        engine
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(args, _)| args[0] == "rm")
            .count(),
        2
    );
}

#[test]
fn ownership_journal_refuses_another_run_or_symlink_without_replacing_inputs() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let ownership = Ownership::new(&directory.0, "first");
    ownership
        .remember("capacity-server-first-1", "server", &format!("{:064x}", 1))
        .unwrap();
    let path = directory.0.join("ownership.json");
    let original = std::fs::read(&path).unwrap();
    assert!(
        Ownership::new(&directory.0, "second")
            .remember("capacity-server-second-1", "server", "")
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let other = Directory::new();
    symlink(&path, other.0.join("ownership.json")).unwrap();
    assert!(
        Ownership::new(&other.0, "first")
            .remember("capacity-server-first-2", "server", "")
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn an_interrupted_monitor_retains_completed_cpu_marks_but_stays_invalid() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock {
        interrupt_at: Some(66.0),
        ..FakeClock::default()
    }));
    let result = measured_step(&engine, &directory, &Cancellation::default()).await;
    assert_eq!(result.error, "capacity interrupted");
    assert_eq!(result.load_scrapes, 2);
    assert_eq!(result.server_cores, 0.25);
    assert_eq!(result.generator_cores, 0.5);
    assert!(!super::judge::judge(&result, Limits::default()).valid);
}

#[tokio::test]
async fn interruption_after_completed_window_keeps_pass_but_labels_unfinished_search() {
    let directory = Directory::new();
    let output = directory.0.join("run");
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    let cancellation = Cancellation::default();
    engine.state.lock().unwrap().cancel_on_transfer = Some(cancellation.clone());
    let options = Options {
        server_image: "server".into(),
        generator_image: "generator".into(),
        quick: true,
        ..Options::default()
    };
    controller::run(
        &options,
        &engine,
        engine.clock.as_ref(),
        &cancellation,
        &output,
    )
    .await
    .unwrap();
    assert_eq!(cancellation.code(), 143);
    let report = crate::capacity::json::read(&output.join("calibration.json")).unwrap();
    assert!(report["steps"].array("steps").unwrap()[0]["passed"].is_true());
    assert_eq!(
        report["ceilings"]["meetings"]["bound"].string(),
        Some("interrupted")
    );
    assert_eq!(
        report["ceilings"]["webinar"]["bound"].string(),
        Some("not-run")
    );
    assert!(report["projection"]["lowerBound"].is_true());
}

#[tokio::test]
async fn a_failed_host_probe_leaves_private_failure_receipt_and_exact_creation_intent() {
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let output = directory.0.join("run");
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    engine.state.lock().unwrap().fail_probe = true;
    let options = Options {
        server_image: "server".into(),
        generator_image: "generator".into(),
        ..Options::default()
    };
    let error = controller::run(
        &options,
        &engine,
        engine.clock.as_ref(),
        &Cancellation::default(),
        &output,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("fixture probe failed"));
    let failure: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("failure.json")).unwrap()).unwrap();
    assert_eq!(failure["complete"], false);
    assert_eq!(failure["interrupted"], 0);
    assert!(!output.join("calibration.json").exists());
    assert!(output.join("ownership.json").is_file());
    assert_eq!(
        std::fs::metadata(output.join("failure.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        engine
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(args, _)| args[0] != "rm")
    );
}
