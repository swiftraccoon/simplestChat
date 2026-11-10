use super::*;
use serde_json::json;
fn expected() -> Value {
    json!({"runId":"a".repeat(32),"revision":"b".repeat(40),"previousRevision":"c".repeat(40),"binary":{"sha256":"d".repeat(64)}})
}
fn ready() -> Value {
    json!({"schemaVersion":1,"kind":"prepared","runId":"a".repeat(32),"revision":"b".repeat(40),"previousRevision":"c".repeat(40),"passed":true,"binarySha256":"d".repeat(64),"firstImage":format!("sha256:{}","e".repeat(64)),"dependencySha256":"f".repeat(64),"migrationSha256":"1".repeat(64)})
}
fn diagnostic() -> Value {
    json!({"stage":"up","boundary":"executor","cause":"command","logOrdinal":193})
}
fn capture() -> Value {
    json!({"checkpoint":"negative","failure":"refused","trial":"negative","nativeRawExit":1,
    "stdoutState":"decoded","outcomeState":"decoded","nativePassed":false,"rollbackAttempted":true,"rollbackPassed":false,
    "rollbackFailure":diagnostic(),"outcomeRollbackFailure":diagnostic(),"nativePhase":"replace_application","nativeFailureLabel":"readiness","nativeCommandOrdinal":null})
}
fn row(negative: bool) -> Value {
    json!({"case":if negative{"owned-candidate-stop-rollback"}else{"first-A-to-B"},"passed":true,
    "exitStatus":i64::from(negative),"rollbackAttempted":negative,"rollbackPassed":negative,
    "retainedBackup":{"bytes":32,"sha256":"2".repeat(64),"listed":true,"restoreRehearsed":false},
    "actorHistory":{"actorIds":["1".repeat(64),"2".repeat(64),"3".repeat(64),"4".repeat(64)],"validatorCount":2,"assetCaptureCount":2,
    "allCreatedStartedAsRequiredExitedZeroAndRemoved":true,"historyRows":20,"nativeIsolationInspected":false},
    "selected":{"revision":"b".repeat(40),"image":format!("sha256:{}","f".repeat(64)),"appId":"5".repeat(64)},
    "injectedCandidateId":if negative{json!("6".repeat(64))}else{Value::Null},"manualStopObserved":negative,
    "dependencyProfile":if negative{"negative-inspection-response-barrier-v1"}else{"original-docker-cli"},"seconds":2.0})
}
fn result() -> Value {
    let mut value = json!({"schemaVersion":1,"kind":"result","runId":"a".repeat(32),"revision":"b".repeat(40),"previousRevision":"c".repeat(40),
        "passed":true,"cleanupPassed":true,"binarySha256":"d".repeat(64),"firstImage":format!("sha256:{}","e".repeat(64)),"currentImage":format!("sha256:{}","f".repeat(64)),
        "trials":[row(false),row(true)],"nativeValidatorIsolationIndependentlyInspected":false,"phaseSeconds":{"prepare":1.0,"positive":2.0,"negative":3.0},
        "separateProbe":{"passed":true,"isolationIndependentlyInspected":true,"migrationMapMatched":true,"exitStatus":0,"containerId":"7".repeat(64)},"failureCapture":null});
    for name in OBSERVATIONS {
        value[name] = true.into();
    }
    value
}
#[test]
fn complete_two_trial_proof_keeps_all_original_success_obligations() {
    let expected = expected();
    let ready = prepared(ready(), &expected).unwrap();
    assert!(guest(result(), &expected, Some(&ready)).is_ok());
    for name in OBSERVATIONS {
        let mut changed = result();
        changed[name] = false.into();
        assert!(guest(changed, &expected, Some(&ready)).is_err(), "{name}");
    }
    for field in ["cleanupPassed", "passed"] {
        let mut changed = result();
        changed[field] = false.into();
        assert!(guest(changed, &expected, Some(&ready)).is_err());
    }
    let mut changed = result();
    changed["trials"][1]["rollbackPassed"] = false.into();
    assert!(guest(changed, &expected, Some(&ready)).is_err());
    let mut changed = result();
    changed["separateProbe"]["isolationIndependentlyInspected"] = false.into();
    assert!(guest(changed, &expected, Some(&ready)).is_err());
    let mut changed = result();
    changed["failureCapture"] = capture();
    assert!(guest(changed, &expected, Some(&ready)).is_err());
    assert!(guest(result(), &expected, None).is_err());
}
#[test]
fn failed_rollback_is_retained_before_unrelated_result_validation_and_never_passes() {
    let mut failed = result();
    failed["passed"] = false.into();
    failed["cleanupPassed"] = false.into();
    failed["failureCapture"] = capture();
    failed["trials"] = json!([row(false)]);
    let captured = capture_failure(&failed, &expected()).unwrap().unwrap();
    assert_eq!(captured["rollbackFailure"]["logOrdinal"], 193);
    let mut unrelated = failed.clone();
    unrelated["separateProbe"] = json!({"unrecognized":true});
    assert!(capture_failure(&unrelated, &expected()).unwrap().is_some());
    assert!(guest(unrelated, &expected(), Some(&ready())).is_err());
    let admitted = guest(failed, &expected(), Some(&ready())).unwrap();
    assert_eq!(admitted["passed"], false);
    let mut foreign = admitted;
    foreign["previousRevision"] = "0".repeat(40).into();
    assert!(capture_failure(&foreign, &expected()).is_err());
}
#[test]
fn capture_domains_states_nulls_and_raw_neighbors_stay_closed() {
    assert!(failure_capture(&capture()).is_ok());
    for (field, value) in [
        ("checkpoint", json!("raw/path")),
        ("failure", json!("unknown")),
        ("stdoutState", json!("invalid")),
        ("nativeCommandOrdinal", json!(193)),
        ("nativeFailureLabel", json!("secret")),
        ("nativeRawExit", json!(256)),
    ] {
        let mut changed = capture();
        changed[field] = value;
        assert!(failure_capture(&changed).is_err(), "{field}");
    }
    let mut extra = capture();
    extra["raw"] = "private bytes".into();
    assert!(failure_capture(&extra).is_err());
    let mut mismatch = capture();
    mismatch["outcomeRollbackFailure"]["cause"] = "io".into();
    assert!(failure_capture(&mismatch).is_ok());
    assert_ne!(
        mismatch["rollbackFailure"],
        mismatch["outcomeRollbackFailure"]
    );
    let mut failed = result();
    failed["passed"] = false.into();
    failed["failureCapture"] = mismatch.clone();
    let retained = capture_failure(&failed, &expected()).unwrap().unwrap();
    assert_eq!(retained, mismatch);
    let mut forged = result();
    forged["failureCapture"] = mismatch;
    assert!(guest(forged, &expected(), Some(&ready())).is_err());
    let mut independent = capture();
    independent["stdoutState"] = "invalid".into();
    for field in [
        "nativePassed",
        "rollbackAttempted",
        "rollbackPassed",
        "nativePhase",
        "nativeFailureLabel",
        "nativeCommandOrdinal",
        "rollbackFailure",
    ] {
        independent[field] = Value::Null;
    }
    assert!(failure_capture(&independent).is_ok());
    for ordinal in [Value::Null, json!(1), json!(193), json!(u64::MAX)] {
        let mut value = diagnostic();
        value["logOrdinal"] = ordinal;
        assert!(super::super::deploy_capture::rollback_failure(&value).is_ok());
    }
    for ordinal in [json!(0), json!(-1), json!(true), json!(1.0)] {
        let mut value = diagnostic();
        value["logOrdinal"] = ordinal;
        assert!(super::super::deploy_capture::rollback_failure(&value).is_err());
    }
}

#[test]
fn equal_packaged_migration_maps_are_required_on_actual_retained_originals() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for (left, right, accepted) in [
        (
            json!({"migrations":{"001_one.sql":"a"}}),
            json!({"migrations":{"001_one.sql":"a"}}),
            true,
        ),
        (
            json!({"migrations":{"001_one.sql":"a"}}),
            json!({"migrations":{"001_one.sql":"b"}}),
            false,
        ),
        (
            json!({"migrations":{"001_one.sql":"a"}}),
            json!({"migrations":{"002_two.sql":"a"}}),
            false,
        ),
        (json!({"migrations":{}}), json!({"migrations":{}}), false),
    ] {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "deploy-schema-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for (name, value) in [("current", left), ("previous", right)] {
            let path = root.join(name);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut control = Control::new(std::time::Instant::now()).unwrap();
        let mut current = Original::open(&root.join("current"), 1024 * 1024, &mut control).unwrap();
        let mut previous =
            Original::open(&root.join("previous"), 1024 * 1024, &mut control).unwrap();
        let observed = same_migrations(&current, &previous, &mut control);
        let closed_current = current.finish(&mut control);
        let closed_previous = previous.finish(&mut control);
        let completed = control.finish();
        fs::remove_file(root.join("current")).unwrap();
        fs::remove_file(root.join("previous")).unwrap();
        fs::remove_dir(root).unwrap();
        assert_eq!(observed.is_ok(), accepted);
        assert!(closed_current.is_ok());
        assert!(closed_previous.is_ok());
        assert!(completed.is_ok());
    }
}
