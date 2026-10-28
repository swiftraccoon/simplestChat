//! Closed host-purpose neighbors; no local test claims a signed host proof.
use super::*;

fn chronology(success: bool) -> Value {
    json!({
        "backupPublishedElapsedNs":1,"firstStopDispatchedElapsedNs":2,
        "firstUnlockElapsedNs":3,
        "secondLockAcquiredElapsedNs":if success{json!(4)}else{Value::Null}
    })
}
#[test]
fn chronological_evidence_requires_real_second_hold_only_after_success() {
    evidence(&chronology(true), true).unwrap();
    evidence(&chronology(false), false).unwrap();
    assert!(evidence(&chronology(false), true).is_err());
    assert!(evidence(&chronology(true), false).is_err());
    for (name, value) in [
        ("backupPublishedElapsedNs", json!(true)),
        ("firstStopDispatchedElapsedNs", json!(0)),
        ("firstUnlockElapsedNs", Value::Null),
        ("secondLockAcquiredElapsedNs", json!(1_800_000_000_001u64)),
    ] {
        let mut neighbor = chronology(true);
        neighbor[name] = value;
        assert!(evidence(&neighbor, true).is_err(), "{name}");
    }
    let mut extra = chronology(true);
    extra["retryEpoch"] = 0.into();
    assert!(evidence(&extra, true).is_err());
}
#[test]
fn negative_projection_requires_actual_borrowed_launcher_facts() {
    let mut report = json!({"action":"maintain","revision":"a".repeat(40),"passed":false,
        "phase":"launcher","launcherPassed":false,"environmentChanges":["MAX_ROOMS"],
        "nativeLauncher":{"facts":{}},"maintenanceEvidence":chronology(false)});
    assert_eq!(
        projection(&report, false, &"a".repeat(40), false)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        8
    );
    report["nativeLauncher"] = Value::Null;
    assert!(projection(&report, false, &"a".repeat(40), false).is_err());
    report["nativeLauncher"] = json!({"facts":{},"assumedCleanup":true});
    assert!(projection(&report, false, &"a".repeat(40), false).is_err());
}
#[test]
fn all_host_observations_are_closed_and_each_is_required() {
    for scenario in [
        "host-success",
        "host-launcher-refusal",
        "host-migration-upgrade",
        "host-proxy-upgrade",
    ] {
        let mut observations = Observed::new();
        assert_eq!(observations.0.len(), 28);
        assert!(observations.validate(scenario).is_err());
        for name in NAMES {
            let expected = match name {
                "sourceCleanObserved" | "applicationReadinessObserved" => {
                    scenario != "host-launcher-refusal"
                }
                "sourceDirtyObserved" | "applicationStoppedObserved" => {
                    scenario == "host-launcher-refusal"
                }
                "daemonEffectsUncertain" => false,
                "reviewedSettingObserved" | "candidateConsumed" => scenario != "host-proxy-upgrade",
                "roleNumberingUpgradeObserved" => scenario == "host-migration-upgrade",
                "browserProxyUpgradeObserved" => scenario == "host-proxy-upgrade",
                _ => true,
            };
            observations.set(name, expected).unwrap();
        }
        observations.validate(scenario).unwrap();
        for name in NAMES {
            let expected = observations.0[name];
            observations.set(name, !expected).unwrap();
            assert!(observations.validate(scenario).is_err(), "{name}");
            observations.set(name, expected).unwrap();
        }
        assert_eq!(observations.set("retryAllowed", true), Err(Error::Input));
    }
}

#[test]
fn proxy_projection_preserves_absent_setting_changes_and_requires_actual_upgrade_flag() {
    let mut report = json!({"action":"maintain","revision":"a".repeat(40),"passed":true,
        "phase":"complete","launcherPassed":true,"browserAssetProxyUpgraded":true,
        "nativeLauncher":{"facts":{}},"maintenanceEvidence":chronology(true)});
    let projected = projection(&report, true, &"a".repeat(40), true).unwrap();
    assert!(projected["environmentChanges"].is_null());
    for changes in [Value::Null, json!([]), json!(["MAX_ROOMS"])] {
        report["environmentChanges"] = changes;
        assert!(projection(&report, true, &"a".repeat(40), true).is_err());
    }
    report.as_object_mut().unwrap().remove("environmentChanges");
    report["browserAssetProxyUpgraded"] = false.into();
    assert!(projection(&report, true, &"a".repeat(40), true).is_err());
}
