//! Full orchestration uses an inert Node role, not a real browser. Its supplied
//! observations are independently compared with the unchanged JavaScript run.
use super::{
    config::{Config, InvalidArguments},
    controller,
    fixtures::Fixture,
};
use serde_json::{Value, json};
use std::{ffi::OsString, sync::atomic::Ordering};

pub(super) async fn oracle_case(options: &Value) -> Value {
    let mut fixture = Fixture::new(options.clone());
    let outcome = fixture.run().await.unwrap();
    let report = &outcome.report;
    let mut observation = serde_json::Map::new();
    for key in [
        "browser",
        "phase",
        "pages",
        "captureCalls",
        "pageErrors",
        "blockedExternalRequests",
        "browserVersion",
        "rejoinMilliseconds",
        "draftPreserved",
        "messageDelivery",
        "navigationCountsBeforeRestart",
    ] {
        observation.insert(key.into(), report[key].clone());
    }
    let servers = report["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|server| {
            json!({"exit":server["exit"],"signals":["SIGTERM"],
            "spawnFailed":false,"processError":false})
        })
        .collect::<Vec<_>>();
    json!({"passed":outcome.code==0,"observation":observation,"servers":servers,
        "browserCleanupPassed":report["browserCleanupPassed"]})
}

#[tokio::test]
async fn complete_three_phase_owner_preserves_identity_environment_and_artifacts() {
    for browser in ["chromium", "firefox", "webkit"] {
        let mut fixture = Fixture::new(json!({"browser":browser}));
        let result = fixture.run().await.unwrap();
        assert_eq!(result.code, 0);
        assert_eq!(result.report["browser"], browser);
        assert_eq!(fixture.state.sequence.load(Ordering::SeqCst), 3);
        assert!(fixture.state.control_closed.load(Ordering::SeqCst));
        let commands = fixture.state.commands.lock().unwrap();
        assert_eq!(commands.len(), 3);
        let servers = commands
            .iter()
            .filter(|v| v["number"] != 0)
            .collect::<Vec<_>>();
        assert_eq!(servers[0]["executable"], servers[1]["executable"]);
        assert_eq!(servers[0]["environment"], servers[1]["environment"]);
        assert_eq!(servers[0]["environment"]["BIND_ADDR"], "127.0.0.1");
        assert_eq!(servers[0]["environment"]["RUN_MIGRATIONS"], "false");
        assert!(servers[0]["environment"].get("DATABASE_URL").is_none());
        let node = commands.iter().find(|v| v["number"] == 0).unwrap();
        assert!(node["environment"].get("NODE_OPTIONS").is_none());
        assert_eq!(node["environment"]["BROWSER_RESTART_ENGINE"], browser);
        drop(commands);
        let trace = fixture.state.trace.lock().unwrap();
        assert!(
            trace.iter().position(|v| v == "stop_end_1").unwrap()
                < trace.iter().position(|v| v == "spawn_2").unwrap()
        );
        assert!(
            trace.iter().position(|v| v == "stop_end_2").unwrap()
                < trace.iter().position(|v| v == "stop_begin_0").unwrap()
        );
        for name in [
            "server-1.log",
            "server-2.log",
            "browser.log",
            "browser-receipt.json",
            "outcome.json",
        ] {
            assert!(fixture.root.join("artifacts").join(name).is_file());
        }
        assert_eq!(
            std::fs::read(fixture.root.join("artifacts/server-1.log")).unwrap(),
            b"out\nerr\n"
        );
    }
}

#[tokio::test]
async fn recovery_checkpoint_controls_the_verdict_while_final_counters_remain_observations() {
    for (option, expected) in [
        ("advanceFinalCounters", 0),
        ("invalidRecoveryPages", 1),
        ("missingRecoveryPages", 1),
    ] {
        let mut fixture = Fixture::new(json!({option:true}));
        let result = fixture.run().await.unwrap();
        assert_eq!(result.code, expected, "{option}");
        assert_eq!(fixture.state.sequence.load(Ordering::SeqCst), 3);
        assert!(fixture.state.control_closed.load(Ordering::SeqCst));
        if expected == 0 {
            assert_eq!(result.report["recoveryPages"][0]["navigations"], 1);
            assert_eq!(result.report["pages"][0]["navigations"], 2);
            assert_eq!(
                result.report["recoveryPages"][0]["received"]["serverRestarting"],
                1
            );
            assert_eq!(result.report["pages"][0]["received"]["serverRestarting"], 2);
        }
        let trace = fixture.state.trace.lock().unwrap();
        for owner in 0..3 {
            assert!(
                trace
                    .iter()
                    .any(|event| event == &format!("stop_end_{owner}"))
            );
        }
    }
}
#[tokio::test]
async fn unknown_cleanup_streams_receipt_and_endpoint_never_supply_success() {
    for option in [
        "unknownRetirement",
        "streamFails",
        "cleanupFails",
        "missingReceipt",
        "controlCloseFails",
        "startFailure",
    ] {
        let mut options = json!({});
        options[option] = true.into();
        let mut fixture = Fixture::new(options);
        let result = fixture.run().await.unwrap();
        assert_ne!(result.code, 0, "{option}");
        assert!(
            fixture.state.control_closed.load(Ordering::SeqCst),
            "{option}"
        );
        let trace = fixture.state.trace.lock().unwrap();
        assert!(trace.iter().any(|v| v == "stop_end_0"), "{option}");
        if option == "missingReceipt" {
            assert!(!fixture.root.join("artifacts/browser-receipt.json").exists());
        }
    }
}
#[tokio::test]
async fn cancellation_during_transition_starts_node_retirement_before_waiting_for_server() {
    let mut fixture = Fixture::new(json!({"heldStop":true,"tailBounds":true}));
    let result = fixture.run().await.unwrap();
    assert_eq!(result.code, 143);
    assert_eq!(fixture.state.servers.load(Ordering::SeqCst), 1);
    let trace = fixture.state.trace.lock().unwrap();
    assert!(
        trace.iter().position(|v| v == "stop_begin_0").unwrap()
            < trace.iter().position(|v| v == "stop_end_1").unwrap()
    );
    assert!(trace.iter().any(|v| v == "stop_end_0"));
    assert_eq!(fixture.state.clock.load(Ordering::SeqCst), 76000);
    assert!(fixture.state.control_closed.load(Ordering::SeqCst));
}
#[tokio::test]
async fn acquired_second_server_is_retired_after_start_cancellation() {
    let mut fixture = Fixture::new(json!({"cancelStart":true}));
    assert_eq!(fixture.run().await.unwrap().code, 143);
    let trace = fixture.state.trace.lock().unwrap();
    assert!(trace.iter().any(|v| v == "stop_end_2"));
    assert!(trace.iter().any(|v| v == "stop_end_0"));
}
#[tokio::test]
async fn first_operation_or_ack_failure_precedes_later_cancellation_and_writes_settle() {
    for options in [
        json!({"stopFails":true,"heldAck":true}),
        json!({"ackFails":true}),
    ] {
        let mut fixture = Fixture::new(options);
        assert_eq!(fixture.run().await.unwrap().code, 1);
        let trace = fixture.state.trace.lock().unwrap();
        assert!(trace.iter().any(|v| v == "stop_end_0"));
        assert!(trace.iter().any(|v| v == "control_closed"));
        if fixture.state.flag("heldAck") {
            assert!(
                trace.iter().position(|v| v == "stop_begin_0").unwrap()
                    < trace.iter().position(|v| v == "ack_end").unwrap()
            );
        }
    }
}
#[tokio::test]
async fn pending_terminal_signal_precedes_a_later_retirement_failure() {
    let mut fixture = Fixture::new(json!({"cancelTerminal":true,"cleanupFails":true}));
    // Earlier first-server failure must remain primary. Isolate the late Node
    // failure by making only Node stop fail in the dedicated fixture below.
    assert_eq!(fixture.run().await.unwrap().code, 1);
    let mut fixture = Fixture::new(json!({"cancelTerminal":true,"nodeStopFails":true}));
    assert_eq!(fixture.run().await.unwrap().code, 143);
}
#[tokio::test]
async fn completion_observed_after_acceptance_or_internal_deadline_cannot_pass() {
    for at in [120000, 175000] {
        let mut fixture = Fixture::new(json!({"nodeStopAt":at}));
        assert_eq!(fixture.run().await.unwrap().code, 124);
        assert!(fixture.state.control_closed.load(Ordering::SeqCst));
    }
}
#[tokio::test]
async fn invalid_node_stdout_is_not_promoted_to_a_validated_receipt_artifact() {
    let mut fixture = Fixture::new(json!({"rawReceipt":"{\"PRIVATE\":true}"}));
    let result = fixture.run().await.unwrap();
    assert_eq!(result.code, 1);
    assert!(!fixture.root.join("artifacts/browser-receipt.json").exists());
    assert!(
        !std::fs::read_to_string(fixture.root.join("artifacts/outcome.json"))
            .unwrap()
            .contains("PRIVATE")
    );
}
#[tokio::test]
async fn saved_observations_and_final_exit_have_explicit_publication_boundaries() {
    let mut fixture = Fixture::new(json!({"cancelBeforePublication":true}));
    assert_eq!(fixture.run().await.unwrap().code, 143);
    let saved: Value = serde_json::from_slice(
        &std::fs::read(fixture.root.join("artifacts/outcome.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["passed"], false);
    let mut fixture = Fixture::new(json!({"latePublication":true}));
    assert_eq!(fixture.run().await.unwrap().code, 124);
    let saved: Value = serde_json::from_slice(
        &std::fs::read(fixture.root.join("artifacts/outcome.json")).unwrap(),
    )
    .unwrap();
    // The command never claims the file certifies its own close or later fence.
    assert_eq!(saved["passed"], true);
}
#[tokio::test]
async fn observed_node_control_status_is_preserved_without_host_cancellation() {
    for held in [false, true] {
        for code in [1, 124, 130, 143] {
            let mut fixture = Fixture::new(json!({"nodeExit":code,"heldNodeExit":held}));
            assert_eq!(fixture.run().await.unwrap().code, code);
            assert_eq!(fixture.state.cancel.load(Ordering::SeqCst), 0);
            assert!(fixture.state.control_closed.load(Ordering::SeqCst));
        }
        for (signal, code) in [("SIGINT", 130), ("SIGTERM", 143)] {
            let mut fixture = Fixture::new(json!({"nodeSignal":signal,"heldNodeExit":held}));
            assert_eq!(fixture.run().await.unwrap().code, code);
            assert_eq!(fixture.state.cancel.load(Ordering::SeqCst), 0);
        }
    }
}
#[test]
fn non_utf8_workspace_is_refused_before_any_selected_resource_admission() {
    use std::os::unix::ffi::OsStringExt;
    let mut fixture = Fixture::new(json!({}));
    let original = fixture.config.take().unwrap();
    let args = vec![
        "--binary".into(),
        original.guest.binary.clone().into(),
        "--output".into(),
        fixture.root.join("other").into(),
        "--browser".into(),
        "chromium".into(),
    ];
    original.finish().unwrap();
    let root = std::path::PathBuf::from(OsString::from_vec(b"/invalid-\xff".to_vec()));
    let error = Config::parse(&args, root, Default::default())
        .err()
        .unwrap();
    assert!(error.is::<InvalidArguments>());
}
#[test]
fn argument_and_environment_domains_are_closed_without_caller_options() {
    let mut fixture = Fixture::new(json!({}));
    let cfg = fixture.config.take().unwrap();
    for args in [
        vec![],
        vec!["--browser".into(), "unknown".into()],
        vec!["--binary".into(), "relative".into()],
        vec![
            "--output".into(),
            "relative".into(),
            "--node-options".into(),
            "private".into(),
        ],
    ] {
        assert!(
            Config::parse(&args, fixture.root.clone(), Default::default())
                .err()
                .unwrap()
                .is::<InvalidArguments>()
        );
    }
    assert!(
        !cfg.node_environment(crate::bench::smoke::config::Ports {
            http: 45101,
            media: 45102
        })
        .contains_key(&OsString::from("NODE_OPTIONS"))
    );
    cfg.finish().unwrap();
}
#[test]
fn native_status_projection_is_fixed() {
    for code in [1, 2, 124, 130, 143] {
        assert_eq!(controller::status(&controller::Failure(code).into()), code);
    }
}
