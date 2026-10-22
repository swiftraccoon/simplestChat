//! Complete fake-operation/status/coverage tests, separate from retained JS oracles.

use super::{
    config::{Config, Mode},
    controller,
    fixtures::Fixture,
};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

pub(super) async fn oracle_case(options: &Value, diagnostics: bool) -> Value {
    let fixture = Fixture::new(options.clone(), diagnostics);
    match fixture.run().await {
        Ok(outcome) => {
            let shutdown = outcome.shutdown.map(|mut v| {
                v.shutdown_ms = 0.0;
                serde_json::to_value(v).unwrap()
            });
            let diagnostic = outcome.diagnostic.map(|mut v| {
                v["artifactDirectory"] = "<owned-artifacts>".into();
                if !v["shutdownMs"].is_null() {
                    v["shutdownMs"] = 0.into()
                }
                v
            });
            json!({"passed":outcome.code==0,"shutdown":shutdown,"diagnostic":diagnostic})
        }
        Err(_) => json!({"passed":false,"shutdown":null,"diagnostic":null}),
    }
}
#[tokio::test]
async fn both_complete_smokes_preserve_core_fields_and_retire_before_report() {
    for diagnostics in [false, true] {
        let fixture = Fixture::new(json!({}), diagnostics);
        let outcome = fixture.run().await.unwrap();
        assert_eq!(outcome.code, 0);
        let shutdown = outcome.shutdown.unwrap();
        assert_eq!(shutdown.peer_close_code, 1000);
        assert!(shutdown.joined && shutdown.server_restarting);
        assert_eq!(shutdown.close_code, 1001);
        assert_eq!(shutdown.exit_code, 0);
        assert!(fixture.state.retired.load(Ordering::SeqCst));
        assert_eq!(fixture.state.sockets_closed.load(Ordering::SeqCst), 2);
        let env = fixture.state.environment.lock().unwrap();
        assert_eq!(env["REGISTRATION_ENABLED"], "false");
        assert_eq!(env["RUN_MIGRATIONS"], "false");
        assert!(env.get("DATABASE_URL").is_none() && env.get("JWT_SECRET").is_none());
        if diagnostics {
            let value = outcome.diagnostic.unwrap();
            assert_eq!(value["passed"], true);
            assert_eq!(value["records"], 2);
            assert_eq!(
                value["evidence"],
                json!({"joinRoom":true,"roomLockWait":true})
            );
            for name in [
                "server-diagnostics.jsonl",
                "server-diagnostics-report.json",
                "diagnostics-smoke.json",
            ] {
                assert!(fixture.config.output.join(name).exists());
            }
            let saved: Value = serde_json::from_slice(
                &std::fs::read(fixture.config.output.join("diagnostics-smoke.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(saved, value);
        }
    }
}
#[tokio::test]
async fn every_admitted_failure_still_joins_child_and_closes_all_sockets() {
    for options in [
        json!({"readyBody":{}}),
        json!({"socketError":true}),
        json!({"neverJoin":true}),
        json!({"admissionError":true}),
        json!({"wrongReason":true}),
        json!({"terminalRoom":true}),
        json!({"noRestartNotice":true}),
        json!({"noClose":true}),
        json!({"closeCode":1006}),
        json!({"cleanClose":false}),
        json!({"peerCloseCode":1006}),
        json!({"peerCloseClean":false}),
        json!({"exitCode":42}),
        json!({"exitSignal":"SIGTERM"}),
        json!({"cleanupFails":true}),
        json!({"unknownRetirement":true}),
        json!({"streamFailure":true}),
    ] {
        let fixture = Fixture::new(options.clone(), true);
        let outcome = fixture.run().await.unwrap();
        assert_ne!(outcome.code, 0, "{options}");
        assert!(fixture.state.retired.load(Ordering::SeqCst), "{options}");
        let trace = fixture.state.trace.lock().unwrap();
        assert!(trace.iter().any(|v| v == "stop_end"), "{options}");
        let connects = trace.iter().filter(|v| *v == "connect").count();
        let expected = if options["socketError"] == true {
            0
        } else {
            connects
        };
        assert_eq!(
            fixture.state.sockets_closed.load(Ordering::SeqCst) as usize,
            expected,
            "{options}"
        );
    }
}
#[tokio::test]
async fn failed_wire_drain_cannot_cancel_the_pending_stop_future() {
    let fixture = Fixture::new(json!({"wrongReason":true}), true);
    assert_ne!(fixture.run().await.unwrap().code, 0);
    let trace = fixture.state.trace.lock().unwrap();
    let begin = trace.iter().position(|v| v == "stop_begin").unwrap();
    let seen = trace.iter().position(|v| v == "restart_observed").unwrap();
    let end = trace.iter().position(|v| v == "stop_end").unwrap();
    assert!(begin < seen && seen < end);
    assert_eq!(fixture.state.stop_count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn complete_writer_retirement_is_required_before_any_recorder_read() {
    for options in [
        json!({"unknownRetirement":true}),
        json!({"streamFailure":true}),
        json!({"cleanupFails":true}),
    ] {
        let fixture = Fixture::new(options, true);
        let outcome = fixture.run().await.unwrap();
        assert_ne!(outcome.code, 0);
        assert!(
            !fixture
                .config
                .output
                .join("server-diagnostics-report.json")
                .exists()
        );
        assert_eq!(
            outcome.diagnostic.unwrap()["diagnosticCoverageComplete"],
            false
        );
    }
}
#[tokio::test]
async fn recorder_coverage_and_required_operations_remain_independent_from_workload() {
    for options in [
        json!({"missing":true}),
        json!({"noSummary":true}),
        json!({"summary":{"dropped":1}}),
        json!({"summary":{"unfinished":1}}),
        json!({"raw":"{\"PRIVATE\":1}\n"}),
    ] {
        let fixture = Fixture::new(options, true);
        let outcome = fixture.run().await.unwrap();
        assert_eq!(outcome.code, 1);
        let value = outcome.diagnostic.unwrap();
        assert_eq!(value["workloadPassed"], true);
        assert_eq!(value["serverShutdownPassed"], true);
        assert_eq!(value["passed"], false);
        assert!(!value.to_string().contains("PRIVATE"));
    }
}
#[tokio::test]
async fn cancellation_and_elapsed_completion_fences_never_return_success() {
    let fixture = Fixture::new(json!({"cancelBefore":true}), true);
    assert_eq!(controller::status(&fixture.run().await.err().unwrap()), 143);
    for options in [
        json!({"status":503,"cancelPolling":true}),
        json!({"cancelPublication":true}),
    ] {
        let fixture = Fixture::new(options, true);
        assert_eq!(fixture.run().await.unwrap().code, 143);
        assert!(fixture.state.retired.load(Ordering::SeqCst));
        let value: Value = serde_json::from_slice(
            &std::fs::read(fixture.config.output.join("diagnostics-smoke.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["passed"], false);
    }
    let fixture = Fixture::new(json!({"drainMs":20000}), false);
    assert_eq!(fixture.run().await.unwrap().code, 124);
    assert!(fixture.state.retired.load(Ordering::SeqCst));
    let trace = fixture.state.trace.lock().unwrap();
    assert!(
        trace
            .iter()
            .position(|value| value == "wire_closed")
            .unwrap()
            < trace.iter().position(|value| value == "stop_end").unwrap()
    );
}
#[test]
fn arguments_are_closed_and_guest_environment_is_not_inherited() {
    let fixture = Fixture::new(json!({}), false);
    for arguments in [
        vec![],
        vec!["--binary".into(), "relative".into()],
        vec![
            "--binary".into(),
            fixture.config.binary.clone().into(),
            "--server".into(),
            "http://foreign".into(),
        ],
        vec![
            "--binary".into(),
            fixture.config.binary.clone().into(),
            "--binary".into(),
            fixture.config.binary.clone().into(),
        ],
    ] {
        assert!(
            Config::parse(
                Mode::Shutdown,
                &arguments,
                fixture.root.clone(),
                Default::default()
            )
            .is_err()
        );
    }
}

#[test]
fn private_output_encoding_is_admitted_before_any_report_or_owner_is_created() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new(json!({}), true);
    let mut bytes = fixture.root.as_os_str().as_encoded_bytes().to_vec();
    bytes.extend_from_slice(b"/invalid-\xff");
    let output = std::ffi::OsString::from_vec(bytes);
    assert!(
        Config::parse(
            Mode::Diagnostics,
            &[
                "--binary".into(),
                fixture.config.binary.clone().into(),
                "--output".into(),
                output
            ],
            fixture.root.clone(),
            Default::default(),
        )
        .is_err()
    );
    assert!(!fixture.config.output.exists());
}

#[test]
fn a_new_default_results_parent_is_private_and_existing_parent_is_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(json!({}), true);
    for expected in [0o700, 0o755] {
        let config = Config::parse(
            Mode::Diagnostics,
            &["--binary".into(), fixture.config.binary.clone().into()],
            fixture.root.clone(),
            Default::default(),
        )
        .unwrap();
        let parent = config.output.parent().unwrap();
        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            expected
        );
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[tokio::test]
async fn delayed_success_and_retirement_control_codes_are_never_erased() {
    for options in [
        json!({"readyElapsed":20000}),
        json!({"connectElapsed":20000}),
    ] {
        let fixture = Fixture::new(options, false);
        assert_eq!(fixture.run().await.unwrap().code, 124);
        assert!(fixture.state.retired.load(Ordering::SeqCst));
        if fixture.state.options["connectElapsed"] == 20000 {
            assert_eq!(fixture.state.sockets_closed.load(Ordering::SeqCst), 1);
        }
    }
    let fixture = Fixture::new(json!({"stopStatus":143}), true);
    assert_eq!(fixture.run().await.unwrap().code, 143);
    let fixture = Fixture::new(json!({"admissionError":true,"stopStatus":143}), true);
    assert_eq!(fixture.run().await.unwrap().code, 1);
    let evidence: Value = serde_json::from_slice(
        &std::fs::read(fixture.config.output.join("shutdown-retirement.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(evidence["groupAndStreamsRetired"], false);
}

#[tokio::test]
async fn acquisition_post_fences_retain_socket_and_clock_crossing_never_panics() {
    let fixture = Fixture::new(json!({"cancelConnect":true}), true);
    assert_eq!(fixture.run().await.unwrap().code, 143);
    assert_eq!(fixture.state.sockets_closed.load(Ordering::SeqCst), 1);
    assert!(fixture.state.retired.load(Ordering::SeqCst));
    let fixture = Fixture::new(json!({"clockJumpOnRead":5}), false);
    assert_eq!(fixture.run().await.unwrap().code, 124);
    assert!(fixture.state.retired.load(Ordering::SeqCst));
}
