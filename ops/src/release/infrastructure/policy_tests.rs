//! The recorded answers of the Python guest checker for every decision of the
//! guest actions; `model_tests` replays the Compose and vocabulary cases.

use super::model_tests::{cases, decoded, refusal};
use super::policy::{self, Owner};
use crate::host::failure::{Failure, Result};
use crate::json::Json;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn answer<T>(result: Result<T>, ok: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(value) => json!({"ok": ok(value)}),
        Err(failure) => refusal(&failure),
    }
}

fn map(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_owned()))
        .collect()
}

/// The bytes a recorded hexadecimal text spells.
fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

fn strings(value: BTreeMap<String, String>) -> Value {
    json!(value)
}

/// The fixed policies the vocabulary recording also holds.
pub(super) fn vocabulary(answer: &Value) {
    let files: serde_json::Map<String, Value> = policy::FILES
        .iter()
        .map(|(name, owner, mode)| {
            let uid = match owner {
                Owner::Root => 0,
                Owner::Postgres => 999,
                Owner::Caddy => 10001,
            };
            ((*name).to_owned(), json!([uid, mode]))
        })
        .collect();
    assert_eq!(Value::Object(files), answer["filePolicies"]);
    let mut secrets = policy::SECRET_NAMES.to_vec();
    secrets.sort_unstable();
    assert_eq!(json!(secrets), answer["secretNames"]);
    let counts: serde_json::Map<String, Value> = super::model::COUNT_NAMES
        .iter()
        .zip(policy::COUNTS)
        .map(|(name, count)| ((*name).to_owned(), json!(count)))
        .collect();
    assert_eq!(Value::Object(counts), answer["counts"]);
    assert_eq!(json!(policy::UNIT_PROPERTIES), answer["unitProperties"]);
    let sql = std::str::from_utf8(super::guest::FIXTURE_SQL).unwrap();
    assert_eq!(json!(sql), answer["fixtureSql"]);
    assert_eq!(json!(super::guest::INSPECTION), answer["inspection"]);
}

/// A run selection is admitted only with its exact schema, owner and revision.
#[test]
fn every_recorded_selection_is_admitted_or_refused_as_the_reference_did() {
    let recorded = cases("selection");
    for (given, expected) in &recorded {
        let text = given["text"].as_str().unwrap();
        let run = given["run"].as_str().unwrap();
        let actual = answer(policy::selection(text.as_bytes(), run), |chosen| {
            json!([chosen.run, chosen.revision])
        });
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 17);
}

/// Property listings must name every requested property once and nothing else.
#[test]
fn every_recorded_property_listing_is_read_as_the_reference_read_it() {
    let recorded = cases("properties");
    for (given, expected) in &recorded {
        let bytes = match given.get("hex") {
            Some(hex) => bytes(hex.as_str().unwrap()),
            None => given["text"].as_str().unwrap().as_bytes().to_vec(),
        };
        let names: Vec<&str> = given["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        let actual = answer(policy::properties(&bytes, &names), strings);
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 13);
}

/// Each recorded unit is accepted or refused for its identity, state and
/// runtime directory as the reference decided.
#[test]
fn every_recorded_unit_is_judged_as_the_reference_judged_it() {
    let recorded = cases("unit");
    for (given, expected) in &recorded {
        let unit = given["unit"].as_str().unwrap();
        let backup = given["backup"].as_bool().unwrap();
        let actual = answer(policy::unit(&map(&given["value"]), unit, backup), |()| {
            Value::Null
        });
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 54);
}

/// Dotenv texts are split and refused as the reference did; a duplicate is
/// refused without naming the line.
#[test]
fn every_recorded_dotenv_text_is_parsed_as_the_reference_parsed_it() {
    let recorded = cases("dotenv");
    for (given, expected) in &recorded {
        let text = given["text"].as_str().unwrap();
        let actual = answer(policy::dotenv(text), strings);
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 15);
}

/// The configuration directory's names, the secrets and every rendered
/// environment are judged as the reference judged them.
#[test]
fn every_recorded_configuration_is_judged_as_the_reference_judged_it() {
    let recorded = cases("files");
    for (case, (given, expected)) in recorded.iter().enumerate() {
        let names: BTreeSet<String> = given["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect();
        let text = |name: &str| given[name].as_str().unwrap().as_bytes();
        let checked = policy::configuration_names(&names).and_then(|()| {
            policy::configuration_values(
                text("secrets"),
                text("app"),
                text("migration"),
                text("proxy"),
                text("admin"),
            )
        });
        let actual = answer(checked, |()| json!(true));
        // The case holds secrets and database URLs: name it by number only.
        assert_eq!(&actual, expected, "case {case}");
    }
    assert_eq!(recorded.len(), 27);
}

/// The record the recorded snapshots were compared with.
fn snapshot_record() -> Json {
    let unit = super::model::sha256(b"unit");
    let file = json!({"sha256": unit, "uid": 0, "gid": 0, "mode": 420});
    decoded(&json!({"schemaVersion": 1, "runId": "a".repeat(32),
        "files": {"/etc/x": {"sha256": "e".repeat(64), "mode": 384},
            "simplestchat-benchmark.service": file, "simplestchat-image-build.service": file},
        "units": {"simplestchat-benchmark.service": {"User": "root"},
            "simplestchat-image-build.service": {"User": "root"}},
        "compose": {"sha256": "f".repeat(64), "services": 4}}))
}

/// A first snapshot records one pass; a later one must find exactly one
/// earlier pass of an equal record.
#[test]
fn every_recorded_snapshot_comparison_is_the_references() {
    let record = snapshot_record();
    let recorded = cases("snapshot");
    for (given, expected) in &recorded {
        let previous = given["previous"].as_str().map(str::as_bytes);
        let actual = match policy::snapshot(previous, &record) {
            Ok(passes) => json!({"ok": {"configurationFiles": 13, "idempotent": passes == 2},
                "passes": passes}),
            Err(failure) => refusal(&failure),
        };
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 15);
}

/// A running container's identity, state, isolation, limits and network are
/// judged as the reference judged them.
#[test]
fn every_recorded_running_container_is_judged_as_the_reference_judged_it() {
    let recorded = cases("running");
    for (given, expected) in &recorded {
        let service = given["service"].as_str().unwrap();
        let image = given["image"].as_str().unwrap();
        let actual = answer(
            policy::running(&decoded(&given["value"]), service, image),
            |id| json!(id),
        );
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 40);
}

/// A listed container must be one full identity of a known service, with the
/// project's name and both ownership labels.
#[test]
fn every_recorded_container_listing_and_ownership_is_the_references() {
    let recorded = cases("owned");
    for (given, expected) in &recorded {
        let service = given["service"].as_str().unwrap();
        let listing = given["listing"].as_str().unwrap().as_bytes();
        let inspection = serde_json::to_vec(&given["value"]).unwrap();
        let known = super::model::SERVICES
            .iter()
            .any(|(name, _)| *name == service);
        let checked = if known {
            policy::listed(listing).and_then(|identifier| {
                policy::owned(&identifier, &inspection, service).map(|_| identifier)
            })
        } else {
            Err(Failure::Refused("Unknown fixture service"))
        };
        assert_eq!(&answer(checked, |id| json!(id)), expected, "{given}");
    }
    assert_eq!(recorded.len(), 13);
}

/// Ledger rows are read and refused as the reference read them.
#[test]
fn every_recorded_ledger_is_read_as_the_reference_read_it() {
    let recorded = cases("ledger");
    for (given, expected) in &recorded {
        let text = given["text"].as_str().unwrap();
        let actual = answer(policy::ledger(text.as_bytes()), strings);
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 15);
}

/// Restored rows equal the synthetic ones exactly as JSON compares them.
#[test]
fn every_recorded_count_comparison_is_the_references() {
    let recorded = cases("counts");
    for (given, expected) in &recorded {
        let equal = policy::counted(&decoded(&given["value"]));
        assert_eq!(&json!({"equal": equal}), expected, "{given}");
    }
    assert_eq!(recorded.len(), 7);
}

/// A receipt's recorded migrations must be the release's, canonical and
/// present, whatever the packaged ledger holds.
#[test]
fn every_recorded_backup_ledger_is_judged_as_the_reference_judged_it() {
    let recorded = cases("backupMigrations");
    for (given, expected) in &recorded {
        let receipt = decoded(&given["receipt"]);
        let packaged: BTreeMap<String, String> = given["packaged"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(version, checksum)| (version.clone(), checksum.as_str().unwrap().into()))
            .collect();
        let actual = answer(policy::recorded_migrations(&receipt, &packaged), strings);
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 15);
}

/// The backup service's identity and sandbox and the timer's exact settings
/// are judged as the reference judged them.
#[test]
fn every_recorded_backup_unit_pair_is_judged_as_the_reference_judged_it() {
    let recorded = cases("backupUnits");
    for (given, expected) in &recorded {
        let checked = policy::backup_units(&map(&given["service"]), &map(&given["timer"]));
        assert_eq!(&answer(checked, |()| Value::Null), expected, "{given}");
    }
    assert_eq!(recorded.len(), 29);
}

/// An image's identity, platform, revision, command and environment names
/// are judged as the reference judged them.
#[test]
fn every_recorded_image_identity_is_judged_as_the_reference_judged_it() {
    let recorded = cases("identity");
    for (given, expected) in &recorded {
        let bytes = serde_json::to_vec(&given["value"]).unwrap();
        let actual = answer(policy::image(&bytes, &"b".repeat(40)), |id| json!(id));
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 24);
}

/// The staged release's selection joins the manifest, the recorded images,
/// the staging record and the inspected image as the reference joined them.
#[test]
fn every_recorded_release_selection_is_joined_as_the_reference_joined_it() {
    let recorded = cases("selected");
    for (given, expected) in &recorded {
        let identity = given["identity"].as_str().unwrap().to_owned();
        let checked = policy::selected(
            &"b".repeat(40),
            given["manifestRevision"].as_str().unwrap(),
            &"9".repeat(64),
            &decoded(&given["images"]),
            &decoded(&given["staged"]),
            |_| Ok(identity),
        );
        // The reference answered with the database image's local identity,
        // which its stand-in daemon always gave as `sha256:7…`; the policy
        // answers with the pinned reference the guest then looks up.
        match (checked, expected.get("ok")) {
            (Ok((server, postgres)), Some(pair)) => {
                assert_eq!(pair, &json!([server, format!("sha256:{}", "7".repeat(64))]));
                assert_eq!(json!(postgres), given["images"]["postgresImage"]);
            }
            (checked, _) => assert_eq!(&answer(checked, |_| Value::Null), expected, "{given}"),
        }
    }
    assert_eq!(recorded.len(), 20);
}

/// Readiness, a completed backup run, the paused timer's states and the QEMU
/// vendor are the reference's.
#[test]
fn every_recorded_readiness_run_state_and_vendor_is_the_references() {
    let recorded = cases("ready");
    for (given, expected) in &recorded {
        let ready = policy::ready(given["text"].as_str().unwrap().as_bytes());
        assert_eq!(&json!({"ready": ready}), expected, "{given}");
    }
    assert_eq!(recorded.len(), 8);
    let recorded = cases("serviceResult");
    for (given, expected) in &recorded {
        let completed = policy::backup_completed(&map(&given["value"]));
        assert_eq!(&json!({"completed": completed}), expected, "{given}");
    }
    assert_eq!(recorded.len(), 4);
    let recorded = cases("timerState");
    for (given, expected) in &recorded {
        // The reference polled once more after each unsettled state.
        let actual = match policy::settled(given["state"].as_str().unwrap()) {
            Ok(true) => json!({"polls": 1}),
            Ok(false) => json!({"polls": 2}),
            Err(failure) => refusal(&failure),
        };
        assert_eq!(&actual, expected, "{given}");
    }
    assert_eq!(recorded.len(), 7);
    let recorded = cases("vendor");
    for (given, expected) in &recorded {
        let vendor = bytes(given["hex"].as_str().unwrap());
        assert_eq!(&json!({"qemu": policy::qemu(&vendor)}), expected, "{given}");
    }
    assert_eq!(recorded.len(), 13);
    let recorded = cases("emptyListing");
    for (given, expected) in &recorded {
        let listing = bytes(given["hex"].as_str().unwrap());
        assert_eq!(
            &json!({"empty": policy::empty(&listing)}),
            expected,
            "{given}"
        );
    }
    assert_eq!(recorded.len(), 11);
}

/// A backup is admitted only for the fixture's own running database, image
/// and complete ledger, a restore only when verified, cleaned up and exact,
/// and no restore container may remain.
#[test]
fn every_recorded_backup_verification_is_judged_as_the_reference_judged_it() {
    let packaged: BTreeMap<String, String> = [("1", "4".repeat(96)), ("2", "5".repeat(96))]
        .into_iter()
        .map(|(v, c)| (v.into(), c))
        .collect();
    let postgres = format!(
        "docker.io/library/postgres:18.1-bookworm@sha256:{}",
        "8".repeat(64)
    );
    let recorded = cases("verifyBackup");
    for (given, expected) in &recorded {
        let receipt = decoded(&json!({"postgresImage": given["receiptImage"],
            "sha256": "1".repeat(64), "backupMigrations": given["recorded"]}));
        let ledger = decoded(&given["ledger"]);
        let report = &given["report"];
        let checked = policy::backup_identity(
            given["postgresId"].as_str().unwrap(),
            &"c".repeat(64),
            &receipt,
            &postgres,
            &packaged,
            || Ok(ledger_value(&ledger)),
        )
        .and_then(|()| {
            let counts = report.get("counts").map(decoded);
            policy::restored(
                report["verified"] == json!(true),
                report["cleanupPassed"] == json!(true),
                counts.as_ref(),
            )
        })
        .and_then(|()| {
            let listing = given["leftover"].as_str().unwrap().as_bytes();
            crate::host::failure::require(
                policy::empty(listing),
                "Fixture restore container remains",
            )
        });
        assert_eq!(&answer(checked, |()| Value::Null), expected, "{given}");
    }
    assert_eq!(recorded.len(), 12);
}

fn ledger_value(ledger: &Json) -> BTreeMap<String, String> {
    ledger
        .object()
        .unwrap()
        .iter()
        .map(|(version, checksum)| (version.clone(), checksum.string().unwrap().to_owned()))
        .collect()
}
