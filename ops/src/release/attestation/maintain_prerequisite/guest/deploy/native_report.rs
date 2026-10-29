//! Passive native facts captured before status and normal-report admission.
//! Neither a decoded report nor a rollback diagnostic supplies success authority.

use super::{Error, Result};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

const STAGES: &[&str] = &[
    "recovery-admission",
    "stop",
    "selection-compose-read",
    "selection-compose-write",
    "selection-env-read",
    "selection-env-write",
    "selection-images-read",
    "selection-images-write",
    "assets-select",
    "up",
    "application-inspect",
    "application-image",
    "loopback-readiness",
    "public-readiness",
    "database-continuity",
    "proxy-continuity",
    "journal-finalize",
];
const BOUNDARIES: &[&str] = &[
    "operation",
    "originals-before",
    "role-ceiling",
    "stdout-admission",
    "stderr-admission",
    "executor",
    "settlement",
    "exit-status",
    "stderr-seal",
    "stdout-seal",
    "originals-after",
];
const CAUSES: &[&str] = &[
    "input",
    "policy",
    "archive",
    "changed",
    "io",
    "close",
    "deadline",
    "interrupt",
    "terminate",
    "zip",
    "bounds",
    "command",
];
const LABELS: &[(&str, &str)] = &[
    ("native_input_refused", "native-input"),
    ("native_policy_refused", "native-policy"),
    ("native_archive_refused", "native-archive"),
    ("native_input_changed", "native-input-changed"),
    ("OSError", "filesystem"),
    ("native_close_refused", "native-close"),
    ("TimeoutExpired", "deadline"),
    ("Release interrupted", "interrupted"),
    ("command_output_limit", "output-limit"),
    ("ReleaseError", "release-error"),
    ("RuntimeProfileError", "runtime-profile"),
    ("asset_tar_path", "asset-tar-path"),
    ("AssetError", "asset-error"),
    ("Application readiness deadline exceeded", "readiness"),
    ("Insufficient backup headroom", "backup-headroom"),
    ("Empty live database backup", "empty-backup"),
    (
        "Running configuration differs from disk; use reviewed maintenance",
        "running-config",
    ),
    (
        "Browser asset proxy configuration changed; use explicit maintenance",
        "browser-proxy-config",
    ),
    ("Invalid deployed identity", "deployed-identity"),
    ("This image is already selected", "same-image"),
    ("Unexpected public origin", "public-origin"),
    ("Database must be healthy", "database-health"),
    (
        "Schema changes require the explicit maintenance deployment",
        "schema-change",
    ),
    ("Candidate migration mismatch", "candidate-migrations"),
    (
        "Inspect retained migration container first",
        "migration-container",
    ),
    ("Running app differs from selected image", "selected-image"),
];
const ASSET_LABELS: &[&str] = &[
    "asset_capture_count",
    "asset_capture_file",
    "asset_capture_revision",
    "asset_command_owner",
    "asset_generation_count",
    "asset_generation_fields",
    "asset_generation_files",
    "asset_immutable_collision",
    "asset_index_duplicate",
    "asset_index_generations",
    "asset_index_schema",
    "asset_index_size",
    "asset_index_total",
    "asset_lifetime",
    "asset_name",
    "asset_operation_failed",
    "asset_public_files",
    "asset_public_pointer",
    "asset_public_target",
    "asset_record",
    "asset_record_fields",
    "asset_required_capacity",
    "asset_revision",
    "asset_revision_changed",
    "asset_selected_revision",
    "asset_store_digest",
    "asset_store_file_size",
    "asset_store_foreign",
    "asset_store_ownership",
    "asset_store_unindexed",
    "asset_tar_changed",
    "asset_tar_content",
    "asset_tar_content_size",
    "asset_tar_count",
    "asset_tar_duplicate",
    "asset_tar_empty",
    "asset_tar_extension",
    "asset_tar_file_size",
    "asset_tar_header_count",
    "asset_tar_names",
    "asset_tar_path",
    "asset_tar_root",
    "asset_tar_size",
    "asset_tar_total",
    "asset_tar_type",
    "asset_tar_unsupported",
    "asset_union_digest",
    "asset_union_files",
    "asset_union_size",
    "asset_version_schema",
    "asset_version_size",
    "asset_workload_busy",
];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Capture {
    pub(super) native_raw_exit: Option<i32>,
    pub(super) native_report_state: &'static str,
    pub(super) native_passed: Option<bool>,
    pub(super) native_rollback_attempted: Option<bool>,
    pub(super) native_rollback_passed: Option<bool>,
    pub(super) native_phase: Option<&'static str>,
    pub(super) native_failure_label: Option<&'static str>,
    pub(super) native_command_ordinal: Option<u64>,
    pub(super) rollback_failure: Option<Value>,
}
impl Capture {
    pub(super) fn not_observed() -> Self {
        Self {
            native_raw_exit: None,
            native_report_state: "not-observed",
            native_passed: None,
            native_rollback_attempted: None,
            native_rollback_passed: None,
            native_phase: None,
            native_failure_label: None,
            native_command_ordinal: None,
            rollback_failure: None,
        }
    }
    fn invalid() -> Self {
        Self {
            native_report_state: "invalid",
            ..Self::not_observed()
        }
    }
    /// The caller supplies the actual process observation, independently of JSON.
    pub(super) fn with_exit(mut self, status: Option<i32>) -> Self {
        self.native_raw_exit = status;
        self
    }
    #[cfg(test)]
    pub(super) fn projection(&self) -> Value {
        json!({
            "nativeRawExit": self.native_raw_exit,
            "nativeReportState": self.native_report_state,
            "nativePassed": self.native_passed,
            "nativeRollbackAttempted": self.native_rollback_attempted,
            "nativeRollbackPassed": self.native_rollback_passed,
            "nativePhase": self.native_phase,
            "nativeFailureLabel": self.native_failure_label,
            "nativeCommandOrdinal": self.native_command_ordinal,
            "rollbackFailure": self.rollback_failure,
        })
    }
}

/// Validate only the producer's closed four-field passive recovery fact.
/// The ordinal names admitted logs; it proves neither dispatch nor completion.
pub(super) fn rollback_failure(value: &Value) -> Result<()> {
    super::super::protocol::exact(value, &["stage", "boundary", "cause", "logOrdinal"])?;
    for (field, choices) in [
        ("stage", STAGES),
        ("boundary", BOUNDARIES),
        ("cause", CAUSES),
    ] {
        if !value[field]
            .as_str()
            .is_some_and(|name| choices.contains(&name))
        {
            return Err(Error::Input);
        }
    }
    if !value["logOrdinal"].is_null()
        && !value["logOrdinal"]
            .as_u64()
            .is_some_and(|ordinal| ordinal > 0)
    {
        return Err(Error::Input);
    }
    Ok(())
}

fn failure_label(value: &str) -> (&'static str, Option<u64>) {
    if let Some((_, label)) = LABELS.iter().find(|(name, _)| *name == value) {
        return (label, None);
    }
    if let Some(label) = ASSET_LABELS.iter().find(|name| **name == value) {
        return (label, None);
    }
    if let Some(number) = value
        .strip_prefix("Command ")
        .and_then(|value| value.strip_suffix(" failed; inspect private output"))
        .filter(|number| number.len() == 3 && number.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|number| number.parse::<u64>().ok())
        .filter(|number| (1..=192).contains(number))
    {
        return ("native-command", Some(number));
    }
    ("other-refusal", None)
}

/// Decode bounded observations before any expected-exit or successful-trial gate.
/// Full report identity, evidence custody and outcome equivalence remain separate.
pub(super) fn capture(bytes: &[u8], maximum: usize) -> Capture {
    let maximum = maximum.min(2 * 1024 * 1024);
    if crate::json::decode_limited(bytes, maximum).is_err() {
        return Capture::invalid();
    }
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Capture::invalid();
    };
    if value.get("action").and_then(Value::as_str) != Some("deploy") {
        return Capture::invalid();
    }
    let mut captured = Capture::not_observed();
    captured.native_report_state = "decoded";
    captured.native_passed = value.get("passed").and_then(Value::as_bool);
    captured.native_rollback_attempted = value.get("rollbackAttempted").and_then(Value::as_bool);
    captured.native_rollback_passed = value.get("rollbackPassed").and_then(Value::as_bool);
    captured.native_phase = match value.get("phase").and_then(Value::as_str) {
        Some("stage") => Some("stage"),
        Some("preflight") => Some("preflight"),
        Some("replace_application") => Some("replace_application"),
        Some("complete") => Some("complete"),
        _ => None,
    };
    if let Some(failure) = value.get("failure").and_then(Value::as_str) {
        let (label, ordinal) = failure_label(failure);
        captured.native_failure_label = Some(label);
        captured.native_command_ordinal = ordinal;
    }
    if let Some(failure) = value.get("rollbackFailure") {
        if rollback_failure(failure).is_err()
            || captured.native_passed != Some(false)
            || captured.native_rollback_attempted != Some(true)
            || captured.native_rollback_passed != Some(false)
            || captured.native_phase != Some("replace_application")
        {
            return Capture::invalid();
        }
        captured.rollback_failure = Some(failure.clone());
    }
    captured
}

#[cfg(test)]
mod tests {
    use super::*;
    fn diagnostic() -> Value {
        json!({"stage":"stop", "boundary":"exit-status", "cause":"command", "logOrdinal":193})
    }
    fn report() -> Value {
        json!({"action":"deploy", "passed":false, "rollbackAttempted":true,
            "rollbackPassed":false, "phase":"replace_application",
            "failure":"Application readiness deadline exceeded", "rollbackFailure":diagnostic()})
    }
    fn observed(value: &Value) -> Capture {
        capture(&serde_json::to_vec(value).unwrap(), 131072)
    }
    #[test]
    fn every_producer_domain_and_positive_ordinal_is_admitted_without_a_command_cap() {
        for (field, choices) in [
            ("stage", STAGES),
            ("boundary", BOUNDARIES),
            ("cause", CAUSES),
        ] {
            for choice in choices {
                let mut value = diagnostic();
                value[field] = json!(choice);
                assert_eq!(rollback_failure(&value), Ok(()));
            }
        }
        for ordinal in [
            Value::Null,
            json!(1),
            json!(192),
            json!(193),
            json!(u64::MAX),
        ] {
            let mut value = diagnostic();
            value["logOrdinal"] = ordinal;
            assert_eq!(rollback_failure(&value), Ok(()));
        }
    }
    #[test]
    fn malformed_or_private_diagnostic_fields_never_enter_the_projection() {
        for (field, hostile) in [
            ("stage", json!("INERT_PRIVATE")),
            ("boundary", json!("private/path")),
            ("cause", json!({"secret":"INERT_PRIVATE"})),
            ("logOrdinal", json!(0)),
            ("logOrdinal", json!(-1)),
            ("logOrdinal", json!(1.0)),
            ("logOrdinal", json!(true)),
            ("logOrdinal", json!("193")),
            ("secret", json!("INERT_PRIVATE")),
        ] {
            let mut value = report();
            value["rollbackFailure"][field] = hostile;
            assert!(rollback_failure(&value["rollbackFailure"]).is_err());
            assert_eq!(
                observed(&value).projection(),
                Capture::invalid().projection()
            );
        }
        let mut missing = diagnostic();
        missing.as_object_mut().unwrap().remove("cause");
        assert!(rollback_failure(&missing).is_err());
        let mut value = report();
        value["rollbackFailure"] = missing;
        assert_eq!(
            observed(&value).projection(),
            Capture::invalid().projection()
        );
    }
    #[test]
    fn capture_precedes_normal_schema_and_exit_authority_and_keeps_old_facts() {
        let value = report(); // Deliberately lacks timestamps, revision and evidence.
        let captured = observed(&value).with_exit(Some(1));
        assert_eq!(captured.native_report_state, "decoded");
        assert_eq!(captured.native_passed, Some(false));
        assert_eq!(captured.native_rollback_attempted, Some(true));
        assert_eq!(captured.native_rollback_passed, Some(false));
        assert_eq!(captured.native_failure_label, Some("readiness"));
        assert_eq!(captured.rollback_failure, Some(diagnostic()));
        assert_eq!(captured.projection().as_object().unwrap().len(), 9);
        assert_eq!(captured.projection()["nativeRawExit"], 1);
        assert_eq!(
            observed(&value).with_exit(Some(-15)).projection()["nativeRawExit"],
            -15
        );
        assert_eq!(
            observed(&value).with_exit(Some(0)).rollback_failure,
            Some(diagnostic())
        );
        assert_eq!(Capture::not_observed().native_report_state, "not-observed");
        assert_eq!(capture(b"", 100).native_report_state, "invalid");
    }
    #[test]
    fn successful_reports_omit_the_native_extension_and_unknown_text_stays_closed() {
        for flags in [
            json!({"passed":true}),
            json!({"passed":false,"rollbackAttempted":false}),
            json!({"passed":false,"rollbackAttempted":true,"rollbackPassed":true}),
        ] {
            let mut value = flags;
            value["action"] = json!("deploy");
            value["phase"] = json!("complete");
            value["failure"] = json!("INERT_PRIVATE/path");
            let captured = observed(&value);
            assert_eq!(captured.native_report_state, "decoded");
            assert_eq!(captured.native_failure_label, Some("other-refusal"));
            assert_eq!(captured.rollback_failure, None);
            assert!(!captured.projection().to_string().contains("INERT_PRIVATE"));
            for extension in [Value::Null, diagnostic()] {
                value["rollbackFailure"] = extension;
                assert_eq!(observed(&value).native_report_state, "invalid");
            }
        }
        for ordinal in ["001", "192", "000", "193", "01", "0001"] {
            let (label, number) =
                failure_label(&format!("Command {ordinal} failed; inspect private output"));
            let valid = matches!(ordinal, "001" | "192");
            assert_eq!(label == "native-command", valid);
            assert_eq!(number.is_some(), valid);
        }
    }
    #[test]
    fn every_old_failure_label_keeps_its_exact_mapping_without_prefix_admission() {
        for (input, expected) in LABELS {
            assert_eq!(failure_label(input), (*expected, None));
        }
        for input in ASSET_LABELS {
            let expected = LABELS
                .iter()
                .find(|(name, _)| name == input)
                .map_or(*input, |(_, label)| *label);
            assert_eq!(failure_label(input), (expected, None));
        }
        for input in [
            "asset_tar_path/private",
            "native_policy_refused/private",
            "Command 193 failed; inspect private output",
        ] {
            assert_eq!(failure_label(input), ("other-refusal", None));
        }
        let captured = observed(&json!({"action":"deploy", "passed":"false",
            "rollbackAttempted":0, "rollbackPassed":[], "phase":{}}));
        assert_eq!(captured.native_passed, None);
        assert_eq!(captured.native_rollback_attempted, None);
        assert_eq!(captured.native_rollback_passed, None);
        assert_eq!(captured.native_phase, None);
    }
    #[test]
    fn duplicate_trailing_nonfinite_wrong_action_and_oversize_bytes_are_invalid() {
        for raw in [
            b"{\"action\":\"deploy\",\"action\":\"deploy\"}".as_slice(),
            b"{\"action\":\"deploy\"} {}",
            b"{\"action\":\"deploy\",\"passed\":NaN}",
            b"{\"action\":\"deploy\",\"passed\":1e999}",
            b"{\"action\":\"other\"}",
            b"[]",
        ] {
            assert_eq!(capture(raw, 131072), Capture::invalid());
        }
        let raw = serde_json::to_vec(&report()).unwrap();
        assert_eq!(capture(&raw, raw.len() - 1), Capture::invalid());
        assert_eq!(capture(&raw, raw.len()).native_report_state, "decoded");
    }
    #[test]
    fn native_stream_budget_does_not_inherit_the_smaller_protocol_record_cap() {
        let mut value = report();
        value["private"] = json!("x".repeat(131073));
        let raw = serde_json::to_vec(&value).unwrap();
        let captured = capture(&raw, 2 * 1024 * 1024);
        assert_eq!(captured.native_report_state, "decoded");
        assert_eq!(captured.rollback_failure, Some(diagnostic()));
        assert!(captured.projection().to_string().len() < 1024);
        let mut padded = serde_json::to_vec(&report()).unwrap();
        padded.resize(2 * 1024 * 1024, b' ');
        assert_eq!(capture(&padded, usize::MAX).native_report_state, "decoded");
        padded.push(b' ');
        assert_eq!(capture(&padded, usize::MAX), Capture::invalid());
    }
    #[test]
    fn decoder_domains_match_the_actual_native_producer_and_frozen_label_maps() {
        let producer = include_str!("../../../deploy/rollback_diagnostic.rs");
        assert_eq!(
            producer
                .matches("#[serde(rename_all = \"kebab-case\")]")
                .count(),
            3
        );
        for (name, expected) in [
            ("Stage", STAGES),
            ("Boundary", BOUNDARIES),
            ("FailureCause", CAUSES),
        ] {
            let marker = format!("enum {name} {{");
            let body = producer
                .split_once(&marker)
                .unwrap()
                .1
                .split_once('}')
                .unwrap()
                .0;
            let actual = body
                .lines()
                .filter_map(|line| line.trim().strip_suffix(','))
                .map(|name| {
                    let mut result = String::new();
                    for (index, letter) in name.chars().enumerate() {
                        if index > 0 && letter.is_ascii_uppercase() {
                            result.push('-');
                        }
                        result.push(letter.to_ascii_lowercase());
                    }
                    result
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
        let original = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../build/security_vm_deploy_guest.py"
        ));
        let body = original
            .split_once("FAILURE_LABELS = {")
            .unwrap()
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        let labels = body
            .lines()
            .filter(|line| line.contains('"'))
            .map(|line| {
                let parts = line.split('"').collect::<Vec<_>>();
                (parts[1], parts[3])
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, LABELS);
        let body = original
            .split_once("STATIC_ASSET_FAILURES = frozenset(")
            .unwrap()
            .1
            .split_once("\n)")
            .unwrap()
            .0;
        let labels = body
            .lines()
            .filter_map(|line| line.trim().strip_prefix('"'))
            .map(|line| line.strip_suffix("\",").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(labels, ASSET_LABELS);
    }
}
