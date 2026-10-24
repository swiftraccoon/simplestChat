//! Pure projections and tiny owned files; no candidate, guest or verifier runs.

use super::*;
use crate::ci::ops_artifact::fixture::Fixture;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

fn expected() -> Expected {
    Expected::new(&"a".repeat(40), "123", "1", "456").unwrap()
}
fn receipt(selected: &Expected, count: usize) -> Value {
    serde_json::json!({
        "schemaVersion":1, "scope":"OpsDebianKeeperReceipt",
        "selection":selected.selection, "artifactId":selected.artifact,
        "passed":false, "failure":"cases",
        "cases":CASES.iter().take(count).map(|&(name,status)| {
            serde_json::json!({"name":name,"exitStatus":status,"passed":true})
        }).collect::<Vec<_>>(),
        "ignoredPrivateText":"private-token https://private.invalid/?sig=secret",
    })
}
fn write_receipt(root: &Path, value: &Value) -> PathBuf {
    let directory = root.join("ops-runtime/keeper");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("summary.json");
    fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
fn semantic_postcondition_failure_is_last_observed_not_first_failed() {
    let selected = expected();
    let diagnostic = Diagnostic::from_bytes(
        &serde_json::to_vec(&receipt(&selected, 6)).unwrap(),
        &selected,
        "failure",
    )
    .unwrap();
    assert!(diagnostic.receipt_available && diagnostic.diagnostic_only);
    assert_eq!(diagnostic.last_observed_case, Some("source-help"));
    assert_eq!(diagnostic.last_exit_status, Some(0));
    assert_eq!(diagnostic.last_exit_matched, Some(true));
    let bytes = serde_json::to_string(&diagnostic).unwrap();
    assert!(!bytes.contains("private-token") && !bytes.contains("https://"));
    assert!(!bytes.contains("firstFailedCase") && !bytes.contains("\"passed\""));
}

#[test]
fn mismatched_identity_unknown_labels_and_boolean_attempt_never_reflect_external_text() {
    let selected = expected();
    for mutation in ["artifact", "label", "bool-attempt", "phase"] {
        let mut value = receipt(&selected, 1);
        match mutation {
            "artifact" => value["artifactId"] = Value::from(999),
            "label" => {
                value["cases"][0]["name"] = Value::from("https://private.invalid/?sig=secret")
            }
            "bool-attempt" => value["selection"]["runAttempt"] = Value::from(true),
            "phase" => value["failure"] = Value::from("private-token"),
            _ => unreachable!(),
        }
        assert!(
            Diagnostic::from_bytes(&serde_json::to_vec(&value).unwrap(), &selected, "failure")
                .is_err()
        );
    }
}

#[test]
fn complete_prefix_stays_passive_and_inconsistent_positive_receipts_refuse() {
    let selected = expected();
    let mut value = receipt(&selected, CASES.len());
    value["passed"] = Value::from(true);
    value["failure"] = Value::Null;
    let diagnostic =
        Diagnostic::from_bytes(&serde_json::to_vec(&value).unwrap(), &selected, "success").unwrap();
    assert!(diagnostic.diagnostic_only && diagnostic.receipt_available);
    assert_eq!(diagnostic.observed_case_count, 33);
    value["cases"][0]["passed"] = Value::from(false);
    assert!(
        Diagnostic::from_bytes(&serde_json::to_vec(&value).unwrap(), &selected, "success").is_err()
    );
}

#[test]
fn source_read_closes_every_original_and_retention_is_private_fresh_and_bounded() {
    let fixture = Fixture::new();
    let selected = expected();
    let original = write_receipt(&fixture.root, &receipt(&selected, 6));
    let before = fs::read(&original).unwrap();
    let budget = Budget::default();
    let diagnostic = collect(&fixture.root, &selected, "failure", &budget);
    let retained = retain(&fixture.root, &diagnostic, &budget);
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
    retained.unwrap();
    let path = fixture.root.join("ops-runtime-diagnostics/diagnostic.json");
    let bytes = fs::read(&path).unwrap();
    assert!(bytes.len() <= MAX_DIAGNOSTIC);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::read(&original).unwrap(), before);
    assert!(retain(&fixture.root, &diagnostic, &budget).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
}

#[test]
fn summary_or_parent_symlinks_and_oversized_reports_produce_only_unavailable_context() {
    for mutation in ["symlink", "parent-symlink", "oversize"] {
        let fixture = Fixture::new();
        let selected = expected();
        let path = write_receipt(&fixture.root, &receipt(&selected, 6));
        match mutation {
            "symlink" => {
                let foreign = fixture.root.join("outside-secret");
                fs::write(&foreign, b"private-token").unwrap();
                fs::remove_file(&path).unwrap();
                symlink(&foreign, &path).unwrap();
            }
            "parent-symlink" => {
                let parent = path.parent().unwrap();
                let foreign = fixture.root.join("foreign");
                fs::rename(parent, &foreign).unwrap();
                symlink(&foreign, parent).unwrap();
            }
            "oversize" => fs::write(&path, vec![0; super::super::SMALL as usize + 1]).unwrap(),
            _ => unreachable!(),
        }
        let budget = Budget::default();
        let diagnostic = collect(&fixture.root, &selected, "failure", &budget);
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
        assert!(!diagnostic.receipt_available);
        assert_eq!(diagnostic.last_observed_case, None);
        assert!(
            !serde_json::to_string(&diagnostic)
                .unwrap()
                .contains("private-token")
        );
    }
}

#[test]
fn missing_receipt_retains_fixed_unavailable_context_without_an_alternate_source() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let diagnostic = collect(&fixture.root, &expected(), "failure", &budget);
    let result = retain(&fixture.root, &diagnostic, &budget);
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
    result.unwrap();
    assert!(!diagnostic.receipt_available);
    assert_eq!(diagnostic.runtime_step_outcome, "failure");
    assert_eq!(diagnostic.observed_case_count, 0);
    let names: Vec<_> = fs::read_dir(fixture.root.join("ops-runtime-diagnostics"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from(LEAF)]);
}

#[test]
fn public_selector_numbers_are_exact_and_marker_is_only_a_fixed_literal() {
    assert!(Expected::new(&"a".repeat(40), "0123", "1", "456").is_err());
    assert!(Expected::new(&"a".repeat(40), "123", "1", "9007199254740992").is_err());
    let fixture = Fixture::new();
    let path = fixture.root.join("github-output");
    fs::write(&path, []).unwrap();
    let result = marker(&path);
    result.unwrap();
    assert_eq!(fs::read(path).unwrap(), b"retained=true\n");
}

#[test]
fn signal_clamped_zero_preserves_original_exit_mismatch_for_expected_zero() {
    let selected = expected();
    let mut value = receipt(&selected, 6);
    value["cases"][5]["passed"] = Value::from(false);
    // Runner.case clamps a raw negative signal status to zero after recording
    // that the raw status did not match the expected zero.
    let diagnostic =
        Diagnostic::from_bytes(&serde_json::to_vec(&value).unwrap(), &selected, "failure").unwrap();
    assert!(diagnostic.receipt_available);
    assert_eq!(diagnostic.last_observed_case, Some("source-help"));
    assert_eq!(diagnostic.last_exit_status, Some(0));
    assert_eq!(diagnostic.last_exit_matched, Some(false));
    value["cases"][5]["passed"] = Value::from(true);
    value["cases"][5]["exitStatus"] = Value::from(1);
    assert!(
        Diagnostic::from_bytes(&serde_json::to_vec(&value).unwrap(), &selected, "failure").is_err()
    );
}

#[test]
fn borrowed_read_only_console_descriptor_preserves_the_actual_write_error() {
    let input = File::open("/dev/null").unwrap();
    let result = print_console(
        Descriptor(input.as_fd()),
        b"{\"kind\":\"ops-runtime-diagnostics\"}\n",
    );
    let closed = nix::unistd::close(input);
    assert_eq!(
        result.unwrap_err().raw_os_error(),
        Some(rustix::io::Errno::BADF.raw_os_error())
    );
    closed.unwrap();
}
