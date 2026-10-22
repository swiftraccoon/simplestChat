//! Refuse missing or contradictory evidence at the untrusted Node boundary.

use super::Receipt;
use serde_json::{Value, json};

const RUN: &str = "00000000-0000-4000-8000-000000000001";
const SAFE: u64 = 9_007_199_254_740_991;

fn complete() -> Value {
    let page = json!({"sockets":2,"navigations":1,
        "received":{"roomJoined":2,"serverRestarting":1},
        "sent":{"joinRoom":2},"malformedFrames":0});
    json!({"schemaVersion":1,"runId":RUN,"completed":true,"phasePassed":true,
        "browserCleanupPassed":true,"cancelled":null,"browserCount":1,
        "records":[{"browserName":"chromium","launchMode":"browser-server",
            "completed":true,"passed":true,"gracefulCloseCompleted":true,
            "forceKillRequested":false,"exitStatusObserved":true,
            "exitCode":0,"signalCode":null}],
        "observation":{"browser":"chromium","phase":"complete","pages":[page,page],
            "recoveryPages":[page,page],
            "captureCalls":[{"user":0,"display":0},{"user":0,"display":0}],
            "pageErrors":0,"blockedExternalRequests":0,"browserVersion":"inert-browser-1",
            "rejoinMilliseconds":100,"draftPreserved":true,
            "messageDelivery":{"before":true,"after":true},
            "navigationCountsBeforeRestart":[1,1]}})
}
fn read(value: &Value) -> anyhow::Result<Receipt> {
    Receipt::read(&serde_json::to_vec(value).unwrap(), RUN, "chromium")
}
fn cannot_pass(value: &Value) {
    assert!(!read(value).is_ok_and(|receipt| receipt.passed()));
}

#[test]
fn complete_observations_pass_and_browser_cleanup_survives_a_failed_journey() {
    assert!(read(&complete()).unwrap().passed());
    for pointer in ["/completed", "/phasePassed"] {
        let mut value = complete();
        *value.pointer_mut(pointer).unwrap() = false.into();
        let receipt = read(&value).unwrap();
        assert!(!receipt.passed());
        assert!(receipt.browser_cleanup());
    }
    let mut value = complete();
    value["cancelled"] = 143.into();
    let receipt = read(&value).unwrap();
    assert!(!receipt.passed());
    assert!(receipt.browser_cleanup());
}

#[test]
fn every_selected_object_requires_its_fields_and_refuses_unknown_fields() {
    for pointer in [
        "",
        "/records/0",
        "/observation",
        "/observation/pages/0",
        "/observation/recoveryPages/0",
        "/observation/captureCalls/0",
        "/observation/messageDelivery",
    ] {
        let original = complete();
        let keys = original
            .pointer(pointer)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            let mut value = complete();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
            assert!(read(&value).is_err(), "Missing field {pointer}/{key}");
        }
        let mut value = complete();
        value.pointer_mut(pointer).unwrap()["unknown"] = true.into();
        assert!(read(&value).is_err(), "Unknown field in {pointer}");
    }
}

#[test]
fn absent_negative_facts_never_become_clean_browser_evidence() {
    for (key, replacement) in [
        ("completed", json!(false)),
        ("passed", json!(false)),
        ("gracefulCloseCompleted", json!(false)),
        ("forceKillRequested", json!(true)),
        ("exitStatusObserved", json!(false)),
        ("exitCode", Value::Null),
        ("exitCode", json!(9)),
        ("signalCode", json!("SIGTERM")),
    ] {
        let mut value = complete();
        value["records"][0][key] = replacement;
        let receipt = read(&value).unwrap();
        assert!(!receipt.browser_cleanup(), "Invalid fact {key}");
        assert!(!receipt.passed());
    }
    for key in [
        "completed",
        "passed",
        "gracefulCloseCompleted",
        "forceKillRequested",
        "exitStatusObserved",
    ] {
        let mut value = complete();
        value["records"][0][key] = Value::Null;
        assert!(read(&value).is_err());
    }
}

#[test]
fn complete_phase_needs_observed_timing_version_draft_delivery_and_navigation() {
    for key in [
        "browserVersion",
        "rejoinMilliseconds",
        "draftPreserved",
        "messageDelivery",
        "navigationCountsBeforeRestart",
        "recoveryPages",
    ] {
        let mut value = complete();
        value["observation"][key] = Value::Null;
        cannot_pass(&value);
    }
    for (pointer, replacement) in [
        ("/observation/phase", json!("after_restart")),
        ("/observation/pages/0/received/roomJoined", json!(1)),
        (
            "/observation/recoveryPages/0/received/serverRestarting",
            json!(0),
        ),
        ("/observation/recoveryPages/0/received/roomJoined", json!(1)),
        ("/observation/pages/0/received/roomClosed", json!(1)),
        ("/observation/pages/0/malformedFrames", json!(1)),
        ("/observation/recoveryPages/0/navigations", json!(0)),
        ("/observation/pageErrors", json!(1)),
        ("/observation/blockedExternalRequests", json!(1)),
        ("/observation/captureCalls/0/user", json!(1)),
        ("/observation/captureCalls/1/display", json!(1)),
        ("/observation/messageDelivery/before", json!(false)),
        ("/observation/messageDelivery/after", json!(false)),
    ] {
        let mut value = complete();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        value.pointer_mut(parent).unwrap()[field] = replacement;
        cannot_pass(&value);
    }
}

#[test]
fn identities_bounds_and_counter_names_are_checked_before_success() {
    for (pointer, replacement) in [
        ("/schemaVersion", json!(2)),
        ("/runId", json!("00000000-0000-4000-8000-000000000002")),
        ("/records/0/browserName", json!("firefox")),
        ("/records/0/launchMode", json!("unknown")),
        ("/observation/browser", json!("firefox")),
        ("/observation/phase", json!("unknown")),
        ("/cancelled", json!(1)),
        ("/browserCount", json!(0)),
        ("/observation/pages/0/sockets", json!(SAFE + 1)),
        ("/observation/recoveryPages/0/sockets", json!(SAFE + 1)),
        ("/observation/captureCalls/0/user", json!(SAFE + 1)),
        ("/observation/rejoinMilliseconds", json!(SAFE + 1)),
        ("/observation/browserVersion", json!("x".repeat(257))),
        ("/observation/browserVersion", json!("private\nversion")),
        ("/records/0/signalCode", json!("sigterm")),
    ] {
        let mut value = complete();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(read(&value).is_err(), "Invalid field {pointer}");
    }
    for key in ["", "room_joined", "Room1", "é", &"a".repeat(65)] {
        let mut value = complete();
        value["observation"]["pages"][0]["sent"][key] = 1.into();
        assert!(read(&value).is_err());
    }
    let mut value = complete();
    for pages in ["pages", "recoveryPages"] {
        let map = value["observation"][pages][0]["sent"]
            .as_object_mut()
            .unwrap();
        map.clear();
        for length in 1..=64 {
            map.insert("a".repeat(length), 0.into());
        }
    }
    assert!(read(&value).unwrap().passed());
    value["observation"]["pages"][0]["sent"]["other"] = 0.into();
    assert!(read(&value).is_err());
}

#[test]
fn final_leave_and_shutdown_counters_do_not_replace_the_recovery_assertion_boundary() {
    let mut value = complete();
    for index in 0..2 {
        value["observation"]["pages"][index]["navigations"] = 2.into();
        value["observation"]["pages"][index]["received"]["serverRestarting"] = 2.into();
        value["observation"]["pages"][index]["sent"]["afterRecovery"] = 1.into();
    }
    let receipt = read(&value).unwrap();
    assert!(receipt.passed());
    let retained = receipt.observation();
    assert_eq!(retained["recoveryPages"][0]["navigations"], 1);
    assert_eq!(retained["pages"][0]["navigations"], 2);
    assert_eq!(
        retained["recoveryPages"][0]["received"]["serverRestarting"],
        1
    );
    assert_eq!(retained["pages"][0]["received"]["serverRestarting"], 2);
    for key in ["roomJoined", "serverRestarting"] {
        let mut bad = value.clone();
        bad["observation"]["recoveryPages"][0]["received"][key] = 0.into();
        cannot_pass(&bad);
    }
    let mut final_join = value;
    final_join["observation"]["pages"][0]["received"]["roomJoined"] = 3.into();
    cannot_pass(&final_join);
}

#[test]
fn checkpoint_shape_and_final_counter_consistency_are_mandatory() {
    for checkpoint in [Value::Null, json!([]), json!({}), json!(true)] {
        let mut value = complete();
        value["observation"]["recoveryPages"] = checkpoint;
        cannot_pass(&value);
    }
    let mut missing = complete();
    missing["observation"]
        .as_object_mut()
        .unwrap()
        .remove("recoveryPages");
    assert!(read(&missing).is_err());
    for (field, lower) in [
        ("sockets", json!(1)),
        ("navigations", json!(0)),
        ("received/roomJoined", json!(1)),
        ("received/serverRestarting", json!(0)),
        ("sent/joinRoom", json!(1)),
    ] {
        let mut value = complete();
        *value
            .pointer_mut(&format!("/observation/pages/0/{field}"))
            .unwrap() = lower;
        assert!(read(&value).is_err(), "Final counter decreased: {field}");
    }
    for field in ["roomClosed", "malformedFrames"] {
        let mut value = complete();
        if field == "roomClosed" {
            value["observation"]["recoveryPages"][0]["received"][field] = 1.into();
            value["observation"]["pages"][0]["received"][field] = 1.into();
        } else {
            value["observation"]["recoveryPages"][0][field] = 1.into();
            value["observation"]["pages"][0][field] = 1.into();
        }
        cannot_pass(&value);
    }
}

#[test]
fn duplicate_fields_and_noninteger_counters_do_not_enter_the_projection() {
    let bytes = serde_json::to_string(&complete()).unwrap();
    for changed in [
        bytes.replacen(
            "\"schemaVersion\":1",
            "\"schemaVersion\":1,\"schemaVersion\":1",
            1,
        ),
        bytes.replacen("\"roomJoined\":2", "\"roomJoined\":2,\"roomJoined\":2", 1),
        bytes.replacen("\"sockets\":2", "\"sockets\":2.0", 1),
        bytes.replacen("\"sockets\":2", "\"sockets\":true", 1),
    ] {
        assert_ne!(changed, bytes);
        assert!(Receipt::read(changed.as_bytes(), RUN, "chromium").is_err());
    }
    assert!(Receipt::read(&vec![b' '; 32 * 1024 + 1], RUN, "chromium").is_err());
}
