//! Closed prepared-host receipt projection, distinct from prerequisite success.

use super::{Error, Result, catalog, options, projection};
use serde_json::{Value, json};

#[path = "host_diagnostic.rs"]
mod diagnostic;
#[path = "host_upgrade.rs"]
mod upgrade_evidence;

const OBSERVATIONS: [&str; 28] = [
    "baselinePassed",
    "distributionPublished",
    "reviewedSettingObserved",
    "candidateConsumed",
    "selectionRetained",
    "backupBeforeStopObserved",
    "backupHeaderAndDigestMatched",
    "backupListPassed",
    "databaseContainerUnchanged",
    "exactLedgerObserved",
    "systemToolHashesJoined",
    "naturalRootTrusted",
    "freshActorInventoryJoined",
    "monitorEpochJoined",
    "finiteDaemonHistoryObserved",
    "workloadLockReleased",
    "externalGuardRetained",
    "noObservedExternalPackets",
    "knownFixtureActorsRetired",
    "knownFixtureNetworkRetired",
    "applicationReadinessObserved",
    "sourceCleanObserved",
    "applicationStoppedObserved",
    "sourceDirtyObserved",
    "daemonEffectsUncertain",
    "authenticatedOpsUpgradeObserved",
    "roleNumberingUpgradeObserved",
    "browserProxyUpgradeObserved",
];
const HOST_CHECKPOINTS: [&str; 19] = [
    "host-distribution",
    "host-baseline",
    "host-baseline-observe",
    "host-trust",
    "host-candidate",
    "host-migration-prepare",
    "host-migration-before",
    "host-migration-after",
    "host-proxy-prepare",
    "host-proxy-baseline",
    "host-proxy-verify",
    "host-ready",
    "host-native",
    "host-result",
    "host-observe",
    "host-backup",
    "host-events",
    "host-cleanup",
    "complete",
];
const EVIDENCE: [&str; 4] = [
    "backupPublishedElapsedNs",
    "firstStopDispatchedElapsedNs",
    "firstUnlockElapsedNs",
    "secondLockAcquiredElapsedNs",
];
const MAX_HOST_NS: u64 = 1800 * 1_000_000_000;

pub(super) fn upgrade(value: Value, run_id: &str, binary_hash: &str, raw: i32) -> Result<Value> {
    projection::exact(
        &value,
        &[
            "schemaVersion",
            "scope",
            "runId",
            "passed",
            "changed",
            "repeatChanged",
            "currentBinarySha256",
            "previousBinaryExecuted",
        ],
    )?;
    if raw != 0
        || value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "PreparedHostOpsUpgrade"
        || value["runId"] != run_id
        || value["passed"] != true
        || value["changed"] != true
        || value["repeatChanged"] != false
        || value["currentBinarySha256"] != binary_hash
        || !options::digest(binary_hash, 64)
        || value["previousBinaryExecuted"] != false
    {
        return Err(Error::Refused);
    }
    Ok(value)
}

fn launcher_facts(facts: &Value, expected: &Value, success: bool) -> Result<()> {
    projection::native_facts(facts)?;
    projection::expected_facts(facts, if success { "positive" } else { "dirty-source" })?;
    if facts["opsSha256"] != expected["binary"]["sha256"]
        || facts["launcherSha256"] != expected["sourcePins"]["ops/ansible/files/deploy-public.sh"]
        || facts["launcherRawStatus"].as_i64() != Some(if success { 0 } else { 1 })
    {
        return Err(Error::Refused);
    }
    Ok(())
}
fn host_report(value: &Value, expected: &Value, positive: bool) -> Result<()> {
    projection::exact(
        value,
        &[
            "action",
            "revision",
            "passed",
            "phase",
            "launcherPassed",
            "environmentChanges",
            "nativeLauncher",
            "maintenanceEvidence",
        ],
    )?;
    if value["action"] != "maintain"
        || value["revision"] != expected["revision"]
        || value["passed"].as_bool().is_none()
        || (!value["launcherPassed"].is_null() && value["launcherPassed"].as_bool().is_none())
        || !value["phase"].as_str().is_some_and(|phase| {
            [
                "stage",
                "preflight",
                "maintenance_stop",
                "launcher",
                "verify",
                "complete",
            ]
            .contains(&phase)
        })
    {
        return Err(Error::Refused);
    }
    let proxy_upgrade = expected["scenario"] == "host-proxy-upgrade";
    let changes = &value["environmentChanges"];
    let valid_changes = if proxy_upgrade {
        changes.is_null()
    } else {
        changes.as_array().is_some_and(|changes| {
            changes.is_empty() || changes == &[Value::String("MAX_ROOMS".into())]
        })
    };
    if !valid_changes {
        return Err(Error::Refused);
    }
    if !value["nativeLauncher"].is_null() {
        projection::exact(&value["nativeLauncher"], &["facts"])?;
        launcher_facts(
            &value["nativeLauncher"]["facts"],
            expected,
            options::host_success(expected["scenario"].as_str().unwrap_or("")),
        )?;
    }
    let evidence = &value["maintenanceEvidence"];
    projection::exact(evidence, &EVIDENCE)?;
    for name in EVIDENCE {
        if !evidence[name].is_null()
            && evidence[name]
                .as_u64()
                .is_none_or(|value| value > MAX_HOST_NS)
        {
            return Err(Error::Refused);
        }
    }
    if positive {
        let success = options::host_success(expected["scenario"].as_str().unwrap_or(""));
        if value["passed"].as_bool() != Some(success)
            || value["phase"] != if success { "complete" } else { "launcher" }
            || value["launcherPassed"].as_bool() != Some(success)
            || (!proxy_upgrade && changes != &json!(["MAX_ROOMS"]))
            || value["nativeLauncher"].is_null()
        {
            return Err(Error::Refused);
        }
        let backup = evidence[EVIDENCE[0]].as_u64().ok_or(Error::Refused)?;
        let stop = evidence[EVIDENCE[1]].as_u64().ok_or(Error::Refused)?;
        let unlock = evidence[EVIDENCE[2]].as_u64().ok_or(Error::Refused)?;
        if backup > stop || stop > unlock {
            return Err(Error::Refused);
        }
        if success {
            let second = evidence[EVIDENCE[3]].as_u64().ok_or(Error::Refused)?;
            if unlock > second {
                return Err(Error::Refused);
            }
        } else if !evidence[EVIDENCE[3]].is_null() {
            return Err(Error::Refused);
        }
    }
    Ok(())
}

pub(super) fn guest(value: Value, expected: &Value) -> Result<Value> {
    projection::exact(
        &value,
        &[
            "schemaVersion",
            "scope",
            "runId",
            "revision",
            "scenario",
            "passed",
            "failure",
            "checkpoint",
            "finalStatus",
            "cleanupPassed",
            "bootstrapRawExit",
            "bootstrapFrame",
            "hostRawExit",
            "hostReport",
            "observations",
            "sourcePins",
            "upgradeEvidence",
            "hostDiagnostic",
        ],
    )?;
    if expected["scope"] != "PreparedHostMaintenanceRequest"
        || !expected["scenario"]
            .as_str()
            .is_some_and(|case| options::HOST_CASES.contains(&case))
        || value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "PreparedHostMaintenanceGuest"
        || value["passed"].as_bool().is_none()
        || value["cleanupPassed"].as_bool().is_none()
        || !value["checkpoint"].as_str().is_some_and(|mark| {
            projection::GUEST_CHECKPOINTS.contains(&mark)
                || HOST_CHECKPOINTS.contains(&mark)
                || diagnostic::STAGES.contains(&mark)
        })
        || !value["finalStatus"]
            .as_u64()
            .is_some_and(|raw| [0, 1, 124, 130, 143].contains(&raw))
        || (!value["failure"].is_null()
            && !value["failure"].as_str().is_some_and(|kind| {
                [
                    "input",
                    "changed",
                    "command",
                    "bounds",
                    "io",
                    "deadline",
                    "cancelled",
                    "refused",
                    "native-status",
                ]
                .contains(&kind)
            }))
    {
        return Err(Error::Refused);
    }
    for name in ["runId", "revision", "scenario", "sourcePins"] {
        if value[name] != expected[name] {
            return Err(Error::Refused);
        }
    }
    let sources = value["sourcePins"].as_object().ok_or(Error::Refused)?;
    if sources
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != catalog::guest_sources()
        || sources
            .values()
            .any(|hash| !hash.as_str().is_some_and(|hash| options::digest(hash, 64)))
    {
        return Err(Error::Refused);
    }
    for name in ["bootstrapRawExit", "hostRawExit"] {
        if !value[name].is_null()
            && !value[name]
                .as_i64()
                .is_some_and(|raw| (-127..=255).contains(&raw))
        {
            return Err(Error::Refused);
        }
    }
    if !value["bootstrapFrame"].is_null() {
        projection::native(&value["bootstrapFrame"])?;
    }
    let positive = value["passed"] == true;
    diagnostic::validate(
        &value["hostDiagnostic"],
        positive,
        value["checkpoint"].as_str().ok_or(Error::Refused)?,
    )?;
    if !value["hostReport"].is_null() {
        host_report(&value["hostReport"], expected, positive)?;
    }
    projection::exact(&value["observations"], &OBSERVATIONS)?;
    for name in OBSERVATIONS {
        if value["observations"][name].as_bool().is_none() {
            return Err(Error::Refused);
        }
    }
    upgrade_evidence::validate(
        &value["upgradeEvidence"],
        expected,
        positive,
        &value["observations"],
    )?;
    if positive {
        let success = options::host_success(expected["scenario"].as_str().unwrap_or(""));
        if !value["failure"].is_null()
            || value["checkpoint"] != "complete"
            || value["finalStatus"].as_u64() != Some(0)
            || value["cleanupPassed"] != true
            || value["bootstrapRawExit"].as_i64() != Some(0)
            || value["hostRawExit"].as_i64() != Some(if success { 0 } else { 1 })
            || value["bootstrapFrame"].is_null()
            || value["hostReport"].is_null()
        {
            return Err(Error::Refused);
        }
        projection::expected_native(&value["bootstrapFrame"], 0, "positive")?;
        launcher_facts(&value["bootstrapFrame"]["facts"], expected, true)?;
        for name in OBSERVATIONS {
            let wanted = match name {
                "daemonEffectsUncertain" => false,
                "applicationReadinessObserved" | "sourceCleanObserved" => success,
                "applicationStoppedObserved" | "sourceDirtyObserved" => !success,
                "reviewedSettingObserved" | "candidateConsumed" => {
                    expected["scenario"] != "host-proxy-upgrade"
                }
                "roleNumberingUpgradeObserved" => expected["scenario"] == "host-migration-upgrade",
                "browserProxyUpgradeObserved" => expected["scenario"] == "host-proxy-upgrade",
                _ => true,
            };
            if value["observations"][name].as_bool() != Some(wanted) {
                return Err(Error::Refused);
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_case(scenario: &str) -> (Value, Value) {
        let success = options::host_success(scenario);
        let pins = catalog::guest_sources()
            .into_iter()
            .map(|name| (name, "d".repeat(64)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let helpers = catalog::HELPERS
            .into_iter()
            .map(|name| (name, "c".repeat(64)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let expected = json!({"schemaVersion":1,
            "scope":"PreparedHostMaintenanceRequest", "runId":"a".repeat(32),
            "revision":"b".repeat(40), "scenario":scenario,
            "setupSeconds":1560,"bodySeconds":3660,"endSeconds":3780,
            "sourcePins":pins,"helperPins":helpers,
            "binary":{"sha256":"e".repeat(64),"bytes":268435456},
            "manifest":{"sha256":"c".repeat(64),"bytes":131072},
            "archive":{"sha256":"c".repeat(64),"bytes":2147483648u64},
            "sourceBundle":{"sha256":"c".repeat(64),"bytes":335544320},
            "sourceTree":"b".repeat(40),"engineVersion":"29.8.2",
            "postgresImage":format!("postgres:17-alpine@sha256:{}","c".repeat(64)),
            "caddyImage":concat!("ghcr.io/swiftraccoon/simplestchat-caddy:",
                "2.11.7-73c81fdbfe625d3df3989ba807064c81fbe2c071",
                "@sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0")});
        let mut facts = json!({"adoptedReaped":0,"adoptedRawStatuses":[],"wrapperRawStatus":0,
            "launcherRawStatus":0,"stdoutBytes":0,"stderrBytes":0,"launcherWorkElapsedNs":1});
        for name in projection::BOOLEAN_FACTS {
            facts[name] = (!matches!(
                name,
                "termSeen" | "workCapExpired" | "nestedClientObserved" | "launcherStopped"
            ))
            .into();
        }
        for name in projection::HASH_FACTS {
            facts[name] = "c".repeat(64).into();
        }
        facts["opsSha256"] = "e".repeat(64).into();
        facts["launcherSha256"] = "d".repeat(64).into();
        let bootstrap = json!({"schemaVersion":1,"experiment":"maintain-launcher-prerequisite",
            "trial":"normal","passed":true,"phase":"complete","failure":null,"facts":facts});
        let second = if success { Some(4) } else { None };
        let evidence = json!({"backupPublishedElapsedNs":1,"firstStopDispatchedElapsedNs":2,
            "firstUnlockElapsedNs":3,"secondLockAcquiredElapsedNs":second});
        let mut observations = json!({});
        for name in OBSERVATIONS {
            observations[name] = match name {
                "daemonEffectsUncertain" => false,
                "applicationReadinessObserved" | "sourceCleanObserved" => success,
                "applicationStoppedObserved" | "sourceDirtyObserved" => !success,
                "reviewedSettingObserved" | "candidateConsumed" => scenario != "host-proxy-upgrade",
                "roleNumberingUpgradeObserved" => scenario == "host-migration-upgrade",
                "browserProxyUpgradeObserved" => scenario == "host-proxy-upgrade",
                _ => true,
            }
            .into();
        }
        let mut host_facts = facts;
        if !success {
            host_facts["wrapperRawStatus"] = 1.into();
            host_facts["launcherRawStatus"] = 1.into();
        }
        let native = json!({"facts":host_facts});
        let environment_changes = if scenario == "host-proxy-upgrade" {
            Value::Null
        } else {
            json!(["MAX_ROOMS"])
        };
        let report = json!({"action":"maintain","revision":expected["revision"],"passed":success,
            "phase":if success {"complete"} else {"launcher"},"launcherPassed":success,
            "environmentChanges":environment_changes,
            "nativeLauncher":native,
            "maintenanceEvidence":evidence});
        let receipt = json!({"schemaVersion":1,"scope":"PreparedHostMaintenanceGuest",
            "runId":expected["runId"],"revision":expected["revision"],"scenario":scenario,
            "passed":true,"failure":null,"checkpoint":"complete","finalStatus":0,
            "cleanupPassed":true,"bootstrapRawExit":0,"bootstrapFrame":bootstrap,
            "hostRawExit":if success {0} else {1},"hostReport":report,"observations":observations,
            "sourcePins":expected["sourcePins"],
            "upgradeEvidence":upgrade_evidence::fixture(scenario, &expected),
            "hostDiagnostic":null});
        (receipt, expected)
    }
    fn fixture(success: bool) -> (Value, Value) {
        fixture_case(if success {
            "host-success"
        } else {
            "host-launcher-refusal"
        })
    }
    #[test]
    fn separate_host_cases_require_both_real_operation_and_bootstrap_shapes() {
        for scenario in options::HOST_CASES {
            let (receipt, expected) = fixture_case(scenario);
            assert!(guest(receipt.clone(), &expected).is_ok());
            let mut old_scope = expected.clone();
            old_scope["scope"] = "MaintainLauncherPrerequisiteRequest".into();
            assert!(guest(receipt, &old_scope).is_err());
        }
    }
    #[test]
    fn complete_host_request_and_passing_receipts_fit_the_existing_session_frame() {
        for scenario in options::HOST_CASES {
            let (receipt, request) = fixture_case(scenario);
            assert_eq!(request.as_object().unwrap().len(), 18);
            assert_eq!(request["sourcePins"].as_object().unwrap().len(), 52);
            assert_eq!(request["helperPins"].as_object().unwrap().len(), 15);
            assert_eq!(receipt.as_object().unwrap().len(), 18);
            assert_eq!(receipt["observations"].as_object().unwrap().len(), 28);
            assert_eq!(receipt["upgradeEvidence"].as_object().unwrap().len(), 3);
            for facts in [
                &receipt["bootstrapFrame"]["facts"],
                &receipt["hostReport"]["nativeLauncher"]["facts"],
            ] {
                assert_eq!(facts.as_object().unwrap().len(), 40);
            }
            guest(receipt.clone(), &request).unwrap();
            let mut negative = receipt.clone();
            negative["passed"] = false.into();
            negative["failure"] = "refused".into();
            negative["finalStatus"] = 1.into();
            negative["cleanupPassed"] = false.into();
            negative["hostDiagnostic"] = diagnostic::fixture();
            negative["checkpoint"] = negative["hostDiagnostic"]["stage"].clone();
            guest(negative.clone(), &request).unwrap();
            for value in [&request, &receipt, &negative] {
                let bytes = super::super::auth::encoded(value).unwrap();
                assert_eq!(bytes.last(), Some(&b'\n'));
                assert!(
                    bytes.len() <= 16384,
                    "{scenario}: {} bytes including newline",
                    bytes.len()
                );
            }
            let mut oversized = negative;
            oversized["hostDiagnostic"]["observedCommandSha256"] = "a".repeat(16384).into();
            assert!(super::super::auth::encoded(&oversized).unwrap().len() > 16384);
            assert!(guest(oversized, &request).is_err());
        }
    }
    #[test]
    fn diagnostics_cannot_change_a_positive_verdict_or_outlive_the_first_failure_checkpoint() {
        for scenario in options::HOST_CASES {
            let (receipt, expected) = fixture_case(scenario);
            let mut changed = receipt.clone();
            changed["hostDiagnostic"] = diagnostic::fixture();
            changed["checkpoint"] = changed["hostDiagnostic"]["stage"].clone();
            assert!(guest(changed.clone(), &expected).is_err());
            changed["passed"] = false.into();
            changed["failure"] = "refused".into();
            changed["finalStatus"] = 1.into();
            guest(changed.clone(), &expected).unwrap();
            changed["checkpoint"] = "host-cleanup".into();
            assert!(guest(changed, &expected).is_err());
            let mut changed = receipt;
            changed.as_object_mut().unwrap().remove("hostDiagnostic");
            assert!(guest(changed, &expected).is_err());
        }
    }
    #[test]
    fn ordering_evidence_refuses_missing_boolean_reverse_or_unobserved_lock_epochs() {
        for success in [true, false] {
            let (receipt, expected) = fixture(success);
            for replacement in [Value::Null, json!(true), json!(MAX_HOST_NS + 1)] {
                let mut invalid = receipt.clone();
                invalid["hostReport"]["maintenanceEvidence"]["firstUnlockElapsedNs"] = replacement;
                assert!(guest(invalid, &expected).is_err());
            }
            let mut reversed = receipt.clone();
            reversed["hostReport"]["maintenanceEvidence"]["backupPublishedElapsedNs"] = 3.into();
            assert!(guest(reversed, &expected).is_err());
            let mut second = receipt;
            second["hostReport"]["maintenanceEvidence"]["secondLockAcquiredElapsedNs"] =
                if success { Value::Null } else { 4.into() };
            assert!(guest(second, &expected).is_err());
        }
    }
    #[test]
    fn each_required_observation_and_current_binary_are_fenced() {
        for scenario in options::HOST_CASES {
            let (receipt, expected) = fixture_case(scenario);
            for name in OBSERVATIONS {
                let mut invalid = receipt.clone();
                invalid["observations"][name] =
                    (!receipt["observations"][name].as_bool().unwrap()).into();
                assert!(guest(invalid, &expected).is_err(), "{name}");
            }
            let mut wrong = receipt;
            wrong["bootstrapFrame"]["facts"]["opsSha256"] = "f".repeat(64).into();
            assert!(guest(wrong, &expected).is_err());
        }
    }
    #[test]
    fn host_wire_rejects_extra_fields_and_contradictory_refusal_result() {
        let (receipt, expected) = fixture(false);
        for field in ["foreign", "nativeFrame"] {
            let mut invalid = receipt.clone();
            invalid[field] = Value::Null;
            assert!(guest(invalid, &expected).is_err());
        }
        let mut invalid = receipt.clone();
        invalid["hostRawExit"] = 0.into();
        assert!(guest(invalid, &expected).is_err());
        let mut invalid = receipt;
        invalid["hostReport"]["phase"] = "complete".into();
        assert!(guest(invalid, &expected).is_err());
    }
    #[test]
    fn both_host_cases_require_complete_launcher_facts_and_exact_statuses() {
        for scenario in options::HOST_CASES {
            let (receipt, expected) = fixture_case(scenario);
            let mut absent = receipt.clone();
            absent["hostReport"]["nativeLauncher"] = Value::Null;
            assert!(guest(absent, &expected).is_err());
            for (name, value) in [
                ("launcherRawStatus", json!(2)),
                ("wrapperRawStatus", json!(2)),
                ("capabilitiesClosed", json!(false)),
            ] {
                let mut invalid = receipt.clone();
                invalid["hostReport"]["nativeLauncher"]["facts"][name] = value;
                assert!(guest(invalid, &expected).is_err(), "{name}");
            }
        }
    }
    #[test]
    fn proxy_only_normalizes_absent_environment_changes_and_unused_candidate_observations() {
        for scenario in options::HOST_CASES {
            let (receipt, expected) = fixture_case(scenario);
            let proxy = scenario == "host-proxy-upgrade";
            assert_eq!(receipt["hostReport"]["environmentChanges"].is_null(), proxy);
            for name in ["reviewedSettingObserved", "candidateConsumed"] {
                assert_eq!(receipt["observations"][name].as_bool(), Some(!proxy));
            }
            let mut changed = receipt.clone();
            changed["hostReport"]["environmentChanges"] = if proxy {
                json!(["MAX_ROOMS"])
            } else {
                Value::Null
            };
            assert!(guest(changed, &expected).is_err());
            let mut changed = receipt;
            changed["hostReport"]
                .as_object_mut()
                .unwrap()
                .remove("environmentChanges");
            assert!(guest(changed, &expected).is_err());
        }
    }
    #[test]
    fn negative_host_receipts_keep_validated_partial_upgrade_evidence() {
        let (mut receipt, expected) = fixture_case("host-migration-upgrade");
        receipt["passed"] = false.into();
        receipt["failure"] = "command".into();
        receipt["checkpoint"] = "host-migration-after".into();
        receipt["finalStatus"] = 1.into();
        receipt["hostRawExit"] = 1.into();
        receipt["hostReport"] = Value::Null;
        receipt["observations"]["roleNumberingUpgradeObserved"] = false.into();
        receipt["upgradeEvidence"]["migrationAfter"] = Value::Null;
        guest(receipt.clone(), &expected).unwrap();
        receipt["upgradeEvidence"]["migrationBefore"]["snapshot"]["memberRole"] = 2.into();
        assert!(guest(receipt, &expected).is_err());
        for scenario in ["host-migration-upgrade", "host-proxy-upgrade"] {
            let (receipt, expected) = fixture_case(scenario);
            for name in ["migrationBefore", "migrationAfter", "proxy"] {
                if !receipt["upgradeEvidence"][name].is_null() {
                    let mut changed = receipt.clone();
                    changed["upgradeEvidence"][name] = Value::Null;
                    assert!(guest(changed, &expected).is_err(), "{scenario}: {name}");
                }
            }
        }
    }
    #[test]
    fn upgrade_joins_actual_status_run_and_authenticated_current_binary() {
        let run = "a".repeat(32);
        let hash = "b".repeat(64);
        let receipt = json!({"schemaVersion":1,"scope":"PreparedHostOpsUpgrade",
            "runId":run,"passed":true,"changed":true,"repeatChanged":false,
            "currentBinarySha256":hash,"previousBinaryExecuted":false});
        assert!(upgrade(receipt.clone(), &run, &hash, 0).is_ok());
        assert!(upgrade(receipt.clone(), &run, &hash, 1).is_err());
        assert!(upgrade(receipt.clone(), &"c".repeat(32), &hash, 0).is_err());
        for (name, value) in [
            ("repeatChanged", json!(true)),
            ("previousBinaryExecuted", json!(true)),
            ("currentBinarySha256", json!("c".repeat(64))),
            ("syntheticStatus", json!(0)),
        ] {
            let mut invalid = receipt.clone();
            invalid[name] = value;
            assert!(upgrade(invalid, &run, &hash, 0).is_err(), "{name}");
        }
    }
}
