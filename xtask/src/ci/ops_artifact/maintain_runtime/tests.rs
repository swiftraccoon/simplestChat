//! Native protocol neighbors and frozen-reference contracts; no VM-positive simulation.

use super::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn expected(scenario: &str) -> Value {
    json!({"runId":"a".repeat(32),"revision":"b".repeat(40),"scenario":scenario,
        "sourcePins":{"ops/ansible/files/deploy-public.sh":"c".repeat(64)},
        "binary":{"sha256":"d".repeat(64),"bytes":123}})
}
fn case(scenario: &str) -> Value {
    let status = match scenario {
        "positive" => 0,
        "dirty-source" => 1,
        "cancel-owned-launcher" => 143,
        "timeout-stopped-launcher" => 124,
        _ => panic!("fixed fixture"),
    };
    let booleans = [
        "namespaceMapped",
        "pidfdMapped",
        "credentialsMatched",
        "privateGo",
        "deadlinesInherited",
        "launcherStarted",
        "nestedClientObserved",
        "launcherStopped",
        "termSeen",
        "workCapExpired",
        "leaderReaped",
        "echild",
        "stdoutEof",
        "stderrEof",
        "stdoutClosed",
        "stderrClosed",
        "capabilitiesClosed",
        "toolsUnchanged",
        "wrapperReaped",
        "wrapperNaturalExit",
        "initControlFlagsRestored",
        "runControlFlagsRestored",
        "chldRegistrationClosed",
        "wrapperStdoutEof",
        "wrapperStderrEof",
        "wrapperStdoutClosed",
        "wrapperStderrClosed",
    ];
    let mut facts = serde_json::Map::new();
    for name in booleans {
        let value = match name {
            "nestedClientObserved" | "launcherStopped" => status == 143 || status == 124,
            "termSeen" => status == 143,
            "workCapExpired" => status == 124,
            _ => true,
        };
        facts.insert(name.into(), value.into());
    }
    for name in [
        "stdoutSha256",
        "stderrSha256",
        "unshareSha256",
        "timeoutSha256",
    ] {
        facts.insert(name.into(), "e".repeat(64).into());
    }
    facts.insert("opsSha256".into(), "d".repeat(64).into());
    facts.insert("launcherSha256".into(), "c".repeat(64).into());
    facts.insert("adoptedReaped".into(), 0.into());
    facts.insert("adoptedRawStatuses".into(), json!([]));
    facts.insert("wrapperRawStatus".into(), status.into());
    facts.insert(
        "launcherRawStatus".into(),
        if status == 0 { 0.into() } else { (-9).into() },
    );
    facts.insert("stdoutBytes".into(), 10.into());
    facts.insert("stderrBytes".into(), 20.into());
    facts.insert(
        "launcherWorkElapsedNs".into(),
        if status == 124 {
            json!(900_000_000_000u64)
        } else {
            100.into()
        },
    );
    let failure = match status {
        0 => Value::Null,
        1 => "command".into(),
        143 => "terminate".into(),
        124 => "deadline".into(),
        _ => unreachable!(),
    };
    let native = json!({"schemaVersion":1,"experiment":"maintain-launcher-prerequisite",
        "trial":if status==0||status==1 {"normal"} else {scenario},"passed":status==0,
        "phase":if status==0 {"complete"} else {"result"},"failure":failure,"facts":facts});
    let mut observations = serde_json::Map::new();
    for name in [
        "sourceCommitMatched",
        "sourceShallowBoundaryVerified",
        "sourceRolesUnchanged",
        "sourceTreeClean",
        "dirtyReadmeObserved",
        "naturalTlsPrepared",
        "naturalMaterialsUnchanged",
        "freshLauncherTlsObserved",
        "applicationReadyObserved",
        "expectedDatabaseStateObserved",
        "ownerAndLobbyObserved",
        "backupHeaderAndDigestMatched",
        "workloadLockReleased",
        "freshActorInventoryJoined",
        "collectorMarkerJoined",
        "collectorFinalized",
        "observedExpectedDaemonJoins",
        "noObservedUnexpectedMutatingEvents",
        "externalGuardRetained",
        "noObservedExternalPackets",
        "knownFixtureActorsRetired",
        "knownFixtureNetworkRetired",
        "systemToolHashesJoined",
        "daemonEffectsUncertain",
    ] {
        let value = match name {
            "sourceTreeClean" => scenario != "dirty-source",
            "dirtyReadmeObserved" => scenario == "dirty-source",
            "freshLauncherTlsObserved"
            | "applicationReadyObserved"
            | "ownerAndLobbyObserved"
            | "backupHeaderAndDigestMatched" => scenario == "positive",
            "daemonEffectsUncertain" => false,
            _ => true,
        };
        observations.insert(name.into(), value.into());
    }
    let expected = expected(scenario);
    json!({"schemaVersion":1,"scope":"MaintainLauncherPrerequisiteGuest",
        "runId":expected["runId"],"revision":expected["revision"],"scenario":scenario,
        "passed":true,"failure":null,"checkpoint":"complete","finalStatus":0,
        "cleanupPassed":true,"nativeRawExit":status,"nativeFrame":native,
        "observations":observations,"sourcePins":expected["sourcePins"]})
}
#[test]
fn four_expected_native_statuses_stay_distinct_from_the_guest_verdict() {
    for scenario in options::CASES {
        let value = case(scenario);
        assert_eq!(
            projection::guest(value.clone(), &expected(scenario)),
            Ok(value)
        );
    }
}
#[test]
fn phase_status_identity_hash_and_timeout_neighbors_refuse_without_secret_projection() {
    for (pointer, value) in [
        ("/nativeFrame/phase", json!("admission")),
        ("/nativeRawExit", json!(7)),
        ("/nativeFrame/facts/opsSha256", json!("f".repeat(64))),
        ("/nativeFrame/facts/stdoutSha256", Value::Null),
        ("/nativeFrame/facts/wrapperNaturalExit", json!(false)),
        ("/cleanupPassed", json!(false)),
        ("/sourcePins/private-key", json!("SECRET")),
    ] {
        let mut row = case("positive");
        if pointer.ends_with("private-key") {
            row["sourcePins"]["private-key"] = value;
        } else {
            *row.pointer_mut(pointer).unwrap() = value;
        }
        let error = projection::guest(row, &expected("positive")).unwrap_err();
        assert_eq!(error, Error::Refused);
        assert!(!format!("{error:?}").contains("SECRET"));
    }
    let mut row = case("timeout-stopped-launcher");
    row["nativeFrame"]["facts"]["launcherWorkElapsedNs"] = json!(899_999_999_999u64);
    assert!(projection::guest(row, &expected("timeout-stopped-launcher")).is_err());
}
#[test]
fn incomplete_refusals_keep_null_hashes_actual_large_counts_and_unknown_statuses() {
    let mut row = case("positive");
    row["passed"] = false.into();
    row["failure"] = "refused".into();
    row["nativeRawExit"] = Value::Null;
    row["nativeFrame"]["passed"] = false.into();
    row["nativeFrame"]["failure"] = "bounds".into();
    let facts = &mut row["nativeFrame"]["facts"];
    facts["stdoutSha256"] = Value::Null;
    facts["wrapperRawStatus"] = Value::Null;
    facts["adoptedReaped"] = json!(1000);
    facts["adoptedRawStatuses"] = json!(vec![-9; 256]);
    facts["stdoutBytes"] = json!(u64::MAX);
    assert_eq!(
        projection::guest(row.clone(), &expected("positive")),
        Ok(row)
    );
}
#[test]
fn strict_wire_rejects_duplicate_keys_trailing_records_nonfinite_and_deep_input() {
    for data in [
        b"{\"passed\":true,\"passed\":false}".as_slice(),
        b"{}\n{}",
        b"{\"x\":NaN}",
        b"\xef\xbb\xbf{}",
        b"{\"x\":1e400}",
    ] {
        assert!(decode(data).is_err());
    }
    let deep = format!("{}0{}", "[".repeat(130), "]".repeat(130));
    assert!(decode(deep.as_bytes()).is_err());
    assert_eq!(
        decode(b"{\"x\":18446744073709551615}").unwrap()["x"].as_u64(),
        Some(u64::MAX)
    );
}
#[test]
fn all_thirty_three_original_ops_cases_and_install_repeat_are_mandatory() {
    let rows = catalog::OPS_CASES
        .iter()
        .map(|(name, status)| {
            json!({"name":name,
        "exitStatus":status,"passed":true})
        })
        .collect::<Vec<_>>();
    let value = json!({"schemaVersion":1,"scope":"OpsDebianGuestReceipt","runId":"a".repeat(32),
        "passed":true,"failure":null,"cases":rows,
        "install":{"firstChanged":true,"secondChanged":false}});
    assert!(projection::ops(value.clone(), &"a".repeat(32), 0).is_ok());
    let mut missing = value.clone();
    missing["cases"].as_array_mut().unwrap().pop();
    assert!(projection::ops(missing, &"a".repeat(32), 0).is_err());
    assert!(projection::ops(value, &"a".repeat(32), 7).is_err());
}
#[test]
fn native_catalog_is_bound_to_every_original_literal_role_and_thirty_six_migrations() {
    let reference = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../build/security_vm_maintain_prerequisite_guest.py"
    ));
    let ops = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../build/security_vm_ops_guest.py"
    ));
    for name in catalog::guest_sources() {
        let literal = if let Some(name) = name.strip_prefix("ops/ansible/templates/public-") {
            name.strip_suffix(".j2").unwrap()
        } else {
            name.as_str()
        };
        assert!(reference.contains(&format!("\"{literal}\"")), "{name}");
    }
    for (name, _) in catalog::OPS_CASES {
        assert!(ops.contains(&format!("\"{name}\"")));
    }
    assert_eq!(catalog::MIGRATIONS.len(), 36);
    assert!(
        !catalog::sources()
            .iter()
            .any(|name| name.contains("private"))
    );
}
#[test]
fn workflow_entry_changes_outer_execution_without_replacing_the_guest_or_caps() {
    let workflow = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../.github/workflows/maintenance-launcher-runtime.yml"
    ));
    assert!(workflow.contains("target/debug/xtask ops-artifact maintain-runtime"));
    assert!(!workflow.contains("python3 -B build/security_vm_maintain_prerequisite.py"));
    assert!(workflow.contains("timeout-minutes: 45") && workflow.contains("timeout-minutes: 35"));
    for case in options::CASES {
        assert!(workflow.contains(case));
    }
    let original = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../build/security_vm_maintain_prerequisite.py"
    ));
    assert!(original.contains("SECONDS = 1800") && original.contains("SETUP = 510"));
    assert!(original.contains("DEBT = NATIVE + OBSERVATION"));
}
#[test]
fn earlier_io_identity_survives_later_setup_expiry_and_failed_rephase() {
    let mut control = control::Control::new(Instant::now()).unwrap();
    control.record(Error::Io);
    control.start = Instant::now() - Duration::from_secs(511);
    assert_eq!(control.advance(), Err(Error::Deadline));
    assert!(control.setup);
    assert_eq!(control.first, Some(Error::Io));
    assert_eq!(control.status, 0);
    control.settle();
    assert_eq!(control.finish(), Err(Error::Io));
}
#[test]
fn outward_failure_is_one_and_only_actual_outer_signals_use_signal_status() {
    assert_eq!(outward_status(0, true), 0);
    assert_eq!(outward_status(0, false), 1);
    assert_eq!(outward_status(124, false), 1);
    assert_eq!(outward_status(7, false), 1);
    assert_eq!(outward_status(130, false), 130);
    assert_eq!(outward_status(143, false), 143);
}
#[test]
fn executable_measurement_refuses_weak_elf_fields_before_transfer() {
    let mut value = json!({"sha256":"a".repeat(64),"bytes":123,"architecture":"x86_64",
        "elfClass":64,"fileType":"dynamic","programHeaders":9,"neededCount":0});
    for name in [
        "littleEndian",
        "noInterpreter",
        "noNeeded",
        "pieFlag",
        "bindNow",
        "nonExecutableStack",
        "noWritableExecutableLoad",
        "relroPresent",
        "noRpath",
        "noRunpath",
        "noTextRelocations",
        "noLoaderHooks",
    ] {
        value[name] = true.into();
    }
    assert!(projection::executable(&value, &"a".repeat(64), 123).is_ok());
    for (key, bad) in [
        ("noInterpreter", json!(false)),
        ("neededCount", json!(1)),
        ("architecture", json!("aarch64")),
        ("pieFlag", json!(0)),
        ("programHeaders", json!(0)),
    ] {
        let mut row = value.clone();
        row[key] = bad;
        assert_eq!(
            projection::executable(&row, &"a".repeat(64), 123),
            Err(Error::Refused)
        );
    }
}
