//! Native protocol neighbors and frozen-reference contracts; no VM-positive simulation.

use super::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn artifact(id: Value, created: Value) -> Value {
    json!({"name":"simplestchat-production-revision","id":id,"created_at":created})
}
fn listing(rows: Vec<Value>) -> Value {
    json!({"total_count":rows.len(),"artifacts":rows})
}

#[test]
fn server_discovery_uses_unique_newest_instant_without_treating_it_as_admission() {
    let old = artifact(json!(1), json!("2026-10-27T05:00:00Z"));
    let newer = artifact(json!(2), json!("2026-10-27T01:00:00.001-04:00"));
    let unrelated = json!({"name":"another-artifact","id":false,"created_at":null});
    for rows in [
        vec![old.clone(), newer.clone(), unrelated.clone()],
        vec![newer.clone(), unrelated, old.clone()],
        vec![old.clone(), old, newer],
    ] {
        assert_eq!(auth::server_artifact(&listing(rows), "revision"), Ok(2));
    }
    // Discovery must not silently select an older artifact when the newest
    // candidate will fail the unchanged expiry or exact-attempt authentication.
    let mut newest = artifact(json!(3), json!("2026-10-27T05:01:00Z"));
    newest["expired"] = true.into();
    assert_eq!(
        auth::server_artifact(
            &listing(vec![
                artifact(json!(1), json!("2026-10-27T05:00:00Z")),
                newest,
            ]),
            "revision",
        ),
        Ok(3),
    );
}

#[test]
fn server_discovery_refuses_ambiguous_or_malformed_matching_candidates() {
    let good = artifact(json!(1), json!("2026-10-27T05:00:00Z"));
    for bad in [
        artifact(json!(2), json!("2026-10-27T01:00:00-04:00")),
        artifact(json!(2), json!("not-a-timestamp")),
        artifact(json!(2), json!("2026-10-27T05:00:00")),
        artifact(json!(2), json!("0".repeat(65))),
        artifact(json!(2), Value::Null),
        artifact(json!(0), json!("2026-10-27T05:01:00Z")),
        artifact(json!(true), json!("2026-10-27T05:01:00Z")),
        artifact(json!(2.0), json!("2026-10-27T05:01:00Z")),
        artifact(json!(1_u64 << 53), json!("2026-10-27T05:01:00Z")),
    ] {
        assert!(auth::server_artifact(&listing(vec![good.clone(), bad]), "revision").is_err());
    }
    for invalid in [
        listing(vec![]),
        listing(vec![json!({"name":"another-artifact"})]),
        json!({"total_count":0,"artifacts":[good.clone()]}),
        json!({"total_count":true,"artifacts":[good.clone()]}),
        listing(vec![good; 101]),
    ] {
        assert!(auth::server_artifact(&invalid, "revision").is_err());
    }
}

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
fn closed_capture_checkpoints_preserve_failed_guest_metadata_without_positive_completion() {
    let scenario = "timeout-stopped-launcher";
    for checkpoint in [
        "capture-live",
        "capture-bounds",
        "capture-eof",
        "capture-stop",
        "capture-wait",
        "capture-status",
        "capture-short",
        "capture-close",
        "capture-data-arp",
        "capture-data-ipv4",
        "capture-data-ipv6",
        "capture-data-icmpv6-router-advertisement",
        "capture-data-other",
        "capture-data-unknown",
    ] {
        let mut failed = case(scenario);
        failed["passed"] = false.into();
        failed["failure"] = "refused".into();
        failed["checkpoint"] = checkpoint.into();
        failed["finalStatus"] = 1.into();
        failed["cleanupPassed"] = false.into();
        failed["observations"]["noObservedExternalPackets"] = false.into();
        let decoded = decode(&serde_json::to_vec(&failed).unwrap()).unwrap();
        let admitted = projection::guest(decoded, &expected(scenario)).unwrap();
        assert_eq!(admitted, failed);
        assert_eq!(admitted.as_object().unwrap().len(), 14);
        assert_eq!(admitted["passed"], false);
        assert_eq!(admitted["nativeRawExit"], 124);
        assert_eq!(admitted["nativeFrame"]["failure"], "deadline");

        let mut passed = case(scenario);
        passed["checkpoint"] = checkpoint.into();
        let decoded = decode(&serde_json::to_vec(&passed).unwrap()).unwrap();
        assert_eq!(
            projection::guest(decoded, &expected(scenario)),
            Err(Error::Refused)
        );
    }
}
#[test]
fn capture_checkpoint_neighbors_refuse_unknown_or_unsafe_diagnostics() {
    let mut failed = case("positive");
    failed["passed"] = false.into();
    failed["failure"] = "refused".into();
    failed["finalStatus"] = 1.into();
    for checkpoint in [
        json!("capture"),
        json!("capture-data"),
        json!("capture-data-ethernet"),
        json!("capture-data-IPv4"),
        json!("capture-data-ipv4 "),
        json!("capture-data-icmpv6-router-advertisement/SECRET"),
        json!("capture-live\nSECRET"),
        json!("capture-data-unknown\u{0}SECRET"),
        json!("SECRET"),
        Value::Null,
        json!(false),
        json!(1),
        json!([]),
        json!({"capture-data-arp":true}),
    ] {
        let mut row = failed.clone();
        row["checkpoint"] = checkpoint;
        let decoded = decode(&serde_json::to_vec(&row).unwrap()).unwrap();
        let error = projection::guest(decoded, &expected("positive")).unwrap_err();
        assert_eq!(error, Error::Refused);
        assert!(!format!("{error:?}").contains("SECRET"));
    }
    // Checkpoint vocabulary adds neither a failure category nor a schema field.
    failed["checkpoint"] = "capture-data-arp".into();
    for (name, value) in [
        ("failure", json!("capture-data-arp")),
        ("captureData", json!("SECRET")),
    ] {
        let mut row = failed.clone();
        row[name] = value;
        let decoded = decode(&serde_json::to_vec(&row).unwrap()).unwrap();
        assert_eq!(
            projection::guest(decoded, &expected("positive")),
            Err(Error::Refused)
        );
    }
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
fn ops_receipt() -> Value {
    let rows = catalog::OPS_CASES
        .iter()
        .map(|(name, status)| {
            json!({"name":name,
        "exitStatus":status,"passed":true})
        })
        .collect::<Vec<_>>();
    json!({"schemaVersion":1,"scope":"OpsDebianGuestReceipt","runId":"a".repeat(32),
        "passed":true,"failure":null,"cases":rows,
        "install":{"firstChanged":true,"secondChanged":false}})
}
#[test]
fn all_thirty_three_original_ops_cases_and_install_repeat_are_mandatory() {
    let value = ops_receipt();
    assert!(
        projection::admit_ops(value.clone(), &"a".repeat(32), 0)
            .unwrap()
            .require_success()
            .is_ok()
    );
    let mut missing = value.clone();
    missing["cases"].as_array_mut().unwrap().pop();
    assert!(projection::admit_ops(missing, &"a".repeat(32), 0).is_err());
    assert!(projection::admit_ops(value, &"a".repeat(32), 7).is_err());
}
#[test]
fn negative_ops_prefix_is_preserved_before_positive_completion_is_refused() {
    let mut value = ops_receipt();
    value["passed"] = false.into();
    value["failure"] = "cases".into();
    value["cases"].as_array_mut().unwrap().truncate(6);
    // The frozen guest clamps a signalled candidate's recorded status to zero,
    // while retaining passed=false from its actual unexpected signal status.
    value["cases"][5]["exitStatus"] = 0.into();
    value["cases"][5]["passed"] = false.into();
    value["install"] = json!({"firstChanged":null,"secondChanged":null});
    let mut report = json!({"opsCases":[],"passed":false});
    assert_eq!(
        execute::observe_ops_receipt(
            &mut report,
            &serde_json::to_vec(&value).unwrap(),
            &"a".repeat(32),
            1,
        ),
        Err(Error::Refused)
    );
    assert_eq!(report["opsCases"], value["cases"]);
    assert_eq!(report["install"], value["install"]);
    assert_eq!(report["opsGuestFailure"], "cases");
    assert_eq!(report["passed"], false);
    assert_eq!(
        report["installDiagnostic"],
        json!({"checkpoint":"guest-result","rawExit":1})
    );
}
#[test]
fn closed_failed_guest_receipts_remain_negative_with_empty_or_complete_prefixes() {
    for failure in [
        "guard",
        "transfer",
        "boot",
        "stage",
        "cases",
        "cleanup",
        "cancelled",
        "deadline",
        "unavailable",
    ] {
        for complete in [false, true] {
            let mut value = ops_receipt();
            value["passed"] = false.into();
            value["failure"] = failure.into();
            if !complete {
                value["cases"] = json!([]);
                value["install"] = json!({"firstChanged":null,"secondChanged":null});
            }
            let raw = match failure {
                "deadline" => 124,
                "cancelled" => 143,
                _ => 1,
            };
            let receipt = projection::admit_ops(value.clone(), &"a".repeat(32), raw).unwrap();
            assert_eq!(receipt.cases(), &value["cases"]);
            assert_eq!(receipt.require_success(), Err(Error::Refused));
            assert!(projection::admit_ops(value, &"a".repeat(32), 0).is_err());
        }
    }
}
#[test]
fn failed_receipt_neighbors_never_publish_candidate_strings_or_invalid_fields() {
    let mut base = ops_receipt();
    base["passed"] = false.into();
    base["failure"] = "cases".into();
    base["cases"].as_array_mut().unwrap().truncate(2);
    for (pointer, bad) in [
        ("/failure", json!("SECRET")),
        ("/failure", Value::Null),
        ("/runId", json!("SECRET")),
        ("/schemaVersion", json!(true)),
        ("/passed", json!(0)),
        ("/cases/0/name", json!("SECRET")),
        ("/cases/0/exitStatus", json!(true)),
        ("/cases/0/exitStatus", json!(1.0)),
        ("/cases/0/exitStatus", json!(-1)),
        ("/cases/0/exitStatus", json!(256)),
        ("/cases/0/exitStatus", json!(0)),
        ("/cases/0/passed", json!("SECRET")),
        ("/install/firstChanged", json!(1)),
        ("/install/secondChanged", json!("SECRET")),
    ] {
        let mut value = base.clone();
        *value.pointer_mut(pointer).unwrap() = bad;
        let mut report = json!({"opsCases":[]});
        assert!(
            execute::observe_ops_receipt(
                &mut report,
                &serde_json::to_vec(&value).unwrap(),
                &"a".repeat(32),
                1,
            )
            .is_err()
        );
        assert_eq!(report["opsCases"], json!([]));
        assert!(report.get("install").is_none());
        assert!(report.get("opsGuestFailure").is_none());
        assert_eq!(
            report["installDiagnostic"]["checkpoint"],
            "guest-projection"
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
    }
    for extra in [false, true] {
        let mut value = base.clone();
        if extra {
            value["cases"] = ops_receipt()["cases"].clone();
            value["cases"].as_array_mut().unwrap().push(json!({
                "name":"SECRET","exitStatus":0,"passed":false
            }));
        } else {
            value["cases"].as_array_mut().unwrap().swap(0, 1);
        }
        assert!(projection::admit_ops(value, &"a".repeat(32), 1).is_err());
    }
}
#[test]
fn bounded_decode_projection_and_child_failures_remain_distinct_without_raw_output() {
    for (bytes, checkpoint) in [
        (vec![b'x'; 65537], "guest-bounds"),
        (b"SECRET".to_vec(), "guest-decode"),
        (b"{\"SECRET\":1}".to_vec(), "guest-projection"),
        (b"{\"SECRET\":1,\"SECRET\":2}".to_vec(), "guest-decode"),
    ] {
        let mut report = json!({"opsCases":[]});
        assert!(execute::observe_ops_receipt(&mut report, &bytes, &"a".repeat(32), 1).is_err());
        assert_eq!(
            report["installDiagnostic"],
            json!({"checkpoint":checkpoint,"rawExit":1})
        );
        assert_eq!(report["opsCases"], json!([]));
        assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
    }
    for error in [Error::Io, Error::Deadline, Error::Child(255)] {
        let mut report = json!({"installDiagnostic":{"checkpoint":"stage-command","rawExit":null}});
        assert_eq!(execute::install_command_error(&mut report, error), error);
        assert_eq!(
            report["installDiagnostic"]["rawExit"],
            match error {
                Error::Child(status) => json!(status),
                _ => Value::Null,
            }
        );
        assert_eq!(report["installDiagnostic"]["checkpoint"], "stage-command");
    }
}
#[test]
fn successful_receipt_preserves_the_existing_summary_shape() {
    let value = ops_receipt();
    let mut report = json!({"opsCases":[]});
    execute::observe_ops_receipt(
        &mut report,
        &serde_json::to_vec(&value).unwrap(),
        &"a".repeat(32),
        0,
    )
    .unwrap();
    assert_eq!(
        report,
        json!({"opsCases":value["cases"],"install":value["install"]})
    );
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
    assert_eq!(catalog::guest_sources().len(), 52);
    assert_eq!(catalog::HELPERS.len(), 15);
    for old_proof in [
        "build/security_vm_maintain_prerequisite_guest.py",
        "build/security_vm_stage_guest.py",
        "build/caddy_probe_guest.py",
        "build/caddy_materials.py",
    ] {
        assert!(!catalog::guest_sources().contains(old_proof));
    }
    assert!(catalog::sources().contains("build/caddy_probe_guest.py"));
    assert!(
        !catalog::sources()
            .iter()
            .any(|name| name.contains("private"))
    );
}
#[test]
fn workflow_entry_keeps_caps_and_selects_the_installed_native_guest() {
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
    let session = include_str!("session.rs");
    assert!(session.contains("\"/usr/local/libexec/simplestchat-public/simplestchat-ops\".into()"));
    assert!(
        session.contains("\"maintain-prerequisite\".into()")
            && session.contains("\"guest\".into()")
    );
    assert!(!session.contains("security_vm_maintain_prerequisite_guest.py"));
}
#[test]
fn retained_catalog_covers_every_native_guest_and_hosted_producer_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let catalog = catalog::sources();
    for prefix in [
        "ops/src/release/attestation/maintain_prerequisite",
        "xtask/src/ci/ops_artifact/hosted",
    ] {
        let mut pending = vec![root.join(prefix)];
        let mut actual = std::collections::BTreeSet::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if entry.path().extension().is_some_and(|value| value == "rs") {
                    assert!(kind.is_file());
                    actual.insert(
                        entry
                            .path()
                            .strip_prefix(root)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    );
                }
            }
        }
        let retained = catalog
            .iter()
            .filter(|name| name.starts_with(&format!("{prefix}/")))
            .cloned()
            .collect();
        assert_eq!(actual, retained, "{prefix}");
    }
    for name in [
        "ops/Cargo.toml",
        "xtask/Cargo.toml",
        "ops/ops-build-inputs.lock.json",
        "xtask/src/ci/ops_artifact/recipe.rs",
        "xtask/src/ci/ops_artifact/resolution.rs",
    ] {
        assert!(catalog.contains(name), "{name}");
    }
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
