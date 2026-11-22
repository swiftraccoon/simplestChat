//! The recorded answers of the Python guest checker this module replaced, for
//! the Compose model, the placement of a failure and the receipt vocabulary;
//! `policy_tests` replays the rest of the same recording.

use super::model::{self, Fault};
use super::{CLASSES, CODES, CONTEXTS, Diagnostic, PHASES};
use crate::host::failure::Failure;
use crate::json::{self, Json};
use crate::test_support::{Recording, sha256_hex};
use serde_json::{Value, json};

/// Every test of the recording, as `Recording::new` requires them.
pub(super) const TESTS: [&str; 24] = [
    "backupMigrations",
    "backupUnits",
    "compose",
    "counts",
    "diagnostic",
    "dotenv",
    "emptyListing",
    "files",
    "identity",
    "ledger",
    "owned",
    "properties",
    "ready",
    "running",
    "selected",
    "selection",
    "service",
    "serviceResult",
    "snapshot",
    "timerState",
    "unit",
    "vendor",
    "verifyBackup",
    "vocabulary",
];

pub(super) fn recording() -> Recording {
    Recording::new(include_str!("recorded_guest.json"), &TESTS)
}

/// The next recorded case of `test` and what the reference was given, once
/// that input's digest is the recorded one.
pub(super) fn cases(test: &str) -> Vec<(Value, Value)> {
    let recording = recording();
    let mut recorded = recording.cases(test);
    let mut found = Vec::new();
    for case in recording.test(test)["cases"].as_array().unwrap() {
        let given = case["given"].clone();
        let canonical = serde_json::to_vec(&given).unwrap();
        assert_eq!(
            case["input"].as_str(),
            Some(sha256_hex(&canonical).as_str())
        );
        let answer = recorded.answer(&canonical);
        found.push((given, answer));
    }
    recorded.finish();
    found
}

/// A decoded copy of `value`, as the guest decodes what it reads.
pub(super) fn decoded(value: &Value) -> Json {
    json::decode(&serde_json::to_vec(value).unwrap()).unwrap()
}

/// The reference's answer for a failure: a refusal by its message, anything
/// else by the class a receipt names it with.
pub(super) fn refusal(failure: &Failure) -> Value {
    match failure {
        Failure::Refused(message) => json!({"refused": message}),
        other => json!({"class": model::diagnose("unknown", "unspecified", other).labels()[3]}),
    }
}

/// Each recorded service setting is accepted or refused as the reference
/// did: users, isolation, capabilities, privilege boundary, the memory,
/// process and CPU bounds in every recorded spelling, network, init and the
/// application's loopback publication.
#[test]
fn every_recorded_service_setting_is_judged_as_the_reference_judged_it() {
    let mut count = 0;
    for (given, answer) in cases("service") {
        let name = given["name"].as_str().unwrap();
        let actual = match model::service(&decoded(&given["value"]), name) {
            Ok(()) => json!({"ok": null}),
            Err(failure) => refusal(&failure),
        };
        assert_eq!(actual, answer, "{given}");
        count += 1;
    }
    assert_eq!(count, 252);
}

/// A CPU limit is a number only in ASCII: `float()` would read an
/// Arabic-Indic digit, the guest refuses it as no number at all.
#[test]
fn a_cpu_limit_in_non_ascii_digits_is_not_a_number() {
    let service = json!({"user": "10001:10001", "read_only": true, "cap_drop": ["ALL"],
        "security_opt": ["no-new-privileges:true"], "mem_limit": "1073741824",
        "pids_limit": 256, "cpus": "1", "network_mode": "none", "init": true});
    assert_eq!(model::service(&decoded(&service), "migrate"), Ok(()));
    for spelled in ["\u{661}", "\u{661}.5", "\u{ff11}"] {
        let mut changed = service.clone();
        changed["cpus"] = json!(spelled);
        let refused = model::service(&decoded(&changed), "migrate");
        assert_eq!(refused, Err(Failure::Value), "{spelled}");
    }
}

/// A recorded model's project, its exact services and each service's own
/// slot for a failure are the reference's.
#[test]
fn every_recorded_compose_model_is_judged_and_placed_as_the_reference_did() {
    let mut count = 0;
    for (given, answer) in cases("compose") {
        let text = given["text"].as_str().unwrap();
        let actual = match model::object(text.as_bytes())
            .map_err(Fault::Bare)
            .and_then(|value| model::compose(&value))
        {
            Ok((services, _)) => json!({"ok": services}),
            Err(Fault::Placed(diagnostic)) => json!({"diagnostic": diagnostic.to_json()}),
            Err(Fault::Bare(failure)) => refusal(&failure),
        };
        assert_eq!(actual, answer, "{text}");
        count += 1;
    }
    assert_eq!(count, 12);
}

/// Every recorded failure is labelled as the reference labelled it: a coded
/// refusal by its code, any other refusal unclassified, each other failure
/// by its class, and unknown phases and contexts by their fallbacks.
#[test]
fn every_recorded_failure_is_labelled_as_the_reference_labelled_it() {
    let mut count = 0;
    for (given, answer) in cases("diagnostic") {
        let message = given["message"].as_str().unwrap();
        let known = CODES
            .iter()
            .map(|(text, _)| *text)
            .find(|text| *text == message);
        let failure = match given["kind"].as_str().unwrap() {
            "release" => Failure::Refused(known.unwrap_or("Some other refusal")),
            "os" => Failure::Os,
            "value" => Failure::Value,
            "key" => Failure::Key,
            "subprocess" => Failure::Timeout,
            // No guest check fails with a class of its own; the receipt keeps
            // the reference's fallback for one.
            "runtime" => {
                assert_eq!(answer["exceptionClass"], "unexpected_error");
                count += 1;
                continue;
            }
            other => panic!("unrecorded kind {other}"),
        };
        let phase = given["phase"].as_str().unwrap();
        let context = given["context"].as_str().unwrap();
        let actual = model::diagnose(phase, context, &failure).to_json();
        assert_eq!(actual, answer, "{given}");
        count += 1;
    }
    assert_eq!(count, 48);
    // A failed command is the reference's release error, unclassified.
    let command = model::diagnose("database", "unspecified", &Failure::Command);
    assert_eq!(
        command.labels(),
        ["database", "unspecified", "unclassified", "release_error"]
    );
}

/// The receipt vocabulary, the protected file policies, the secret names, the
/// synthetic rows, the service users, the unit properties and the fixture's
/// SQL and inspection format are the reference's own.
#[test]
fn the_receipt_vocabulary_and_fixed_policies_are_the_references() {
    let [(_, answer)] = cases("vocabulary").try_into().unwrap();
    let mut phases = PHASES.to_vec();
    phases.sort_unstable();
    assert_eq!(json!(phases), answer["phases"]);
    let mut contexts = CONTEXTS.to_vec();
    contexts.sort_unstable();
    assert_eq!(json!(contexts), answer["contexts"]);
    let codes: serde_json::Map<String, Value> = CODES
        .iter()
        .map(|(text, code)| ((*text).to_owned(), json!(code)))
        .collect();
    assert_eq!(Value::Object(codes), answer["codes"]);
    let mut classes = answer["classes"].as_array().unwrap().clone();
    classes.push(json!("unexpected_error"));
    assert_eq!(json!(CLASSES), json!(classes));
    let services: serde_json::Map<String, Value> = model::SERVICES
        .iter()
        .map(|(name, user)| ((*name).to_owned(), json!(user)))
        .collect();
    assert_eq!(Value::Object(services), answer["services"]);
    let counts: serde_json::Map<String, Value> = answer["counts"].as_object().unwrap().clone();
    assert_eq!(counts.keys().map(String::as_str).collect::<Vec<_>>(), {
        let mut names = model::COUNT_NAMES.to_vec();
        names.sort_unstable();
        names
    });
    #[cfg(feature = "maintain-guest")]
    super::policy_tests::vocabulary(&answer);
    // A reviewed label survives; anything else becomes its fallback.
    let diagnostic = Diagnostic::labelled("private", "private", "private", "private");
    assert_eq!(
        diagnostic.labels(),
        ["unknown", "unspecified", "unclassified", "unexpected_error"]
    );
}

/// The public Compose check judges bytes as a snapshot does and reports
/// only fixed labels.
#[test]
fn the_public_compose_check_decodes_strictly_and_reports_fixed_labels() {
    let services = json!({
        "simplestchat": {"user": "10001:10001", "read_only": true, "cap_drop": ["ALL"],
            "security_opt": ["no-new-privileges:true"], "mem_limit": "1073741824",
            "pids_limit": 256, "cpus": 1, "init": true,
            "ports": [{"protocol": "tcp", "target": 3000, "published": "3000",
                "host_ip": "127.0.0.1"}]},
        "migrate": {"user": "10001:10001", "read_only": true, "cap_drop": ["ALL"],
            "security_opt": ["no-new-privileges:true"], "mem_limit": "1073741824",
            "pids_limit": 256, "cpus": 1, "network_mode": "none", "init": true},
        "postgres": {"user": "999:999", "read_only": true, "cap_drop": ["ALL"],
            "security_opt": ["no-new-privileges:true"], "mem_limit": "1073741824",
            "pids_limit": 128, "cpus": "1", "network_mode": "none"},
        "caddy": {"user": "10001:10001", "read_only": true, "cap_drop": ["ALL"],
            "cap_add": ["NET_BIND_SERVICE"], "security_opt": ["no-new-privileges:true"],
            "mem_limit": "268435456", "pids_limit": 128, "cpus": 0.5, "init": true},
    });
    let model = json!({"name": "simplestchat-public", "services": services});
    let bytes = serde_json::to_vec(&model).unwrap();
    assert_eq!(super::compose_model(&bytes), Ok(4));
    let duplicate = br#"{"name":"simplestchat-public","name":"other","services":{}}"#;
    let refused = super::compose_model(duplicate).unwrap_err();
    assert_eq!(
        refused.labels(),
        [
            "snapshot_compose",
            "unspecified",
            "unclassified",
            "value_error"
        ]
    );
    let mut root = model.clone();
    root["services"]["migrate"]["user"] = json!("0:0");
    let placed = super::compose_model(&serde_json::to_vec(&root).unwrap()).unwrap_err();
    assert_eq!(
        placed.labels(),
        [
            "snapshot_compose",
            "migrate",
            "compose_user",
            "release_error"
        ]
    );
    assert_eq!(
        placed.to_string(),
        "snapshot_compose/migrate: compose_user (release_error)"
    );
}
