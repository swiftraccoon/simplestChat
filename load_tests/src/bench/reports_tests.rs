//! Exercise actual owned reader children, not a mocked JSON parser. The helper
//! prints fixture reports only; it starts no workload, socket or native media.

use super::{
    artifacts,
    process::NativeHost,
    provenance,
    reports::{NativeReports, Reports, Request},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn selected_reader_protocol_refuses_private_fields_divergence_and_process_failures() {
    let directory = std::env::temp_dir().join(format!("benchmark-report-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("absent-results.json");
    let samples = json!([]);
    let reference = crate::report::benchmark_report("media", &path, &samples, false, 2).unwrap();
    let mut fixtures = Vec::new();
    // Different field order and Number spellings still describe the same
    // report; returned values come from the linked fixed projection.
    let mut reversed = reference
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(key, value)| format!("{}:{}", serde_json::to_string(key).unwrap(), value))
        .collect::<Vec<_>>()
        .join(",");
    reversed = format!("{{{reversed}}}").replace("\"schemaVersion\":1", "\"schemaVersion\":1.0");
    fixtures.push((reversed, 1, true));
    for pointer in ["", "/coverage"] {
        let mut value = reference.clone();
        value.pointer_mut(pointer).unwrap()["privatePayload"] = json!("secret-sentinel");
        fixtures.push((value.to_string(), 1, false));
    }
    let mut changed = reference.clone();
    changed["coverage"]["expectedConsumers"] = json!(1);
    fixtures.push((changed.to_string(), 1, false));
    fixtures.push((reference.to_string(), 0, false));
    fixtures.push((reference.to_string(), 2, false));
    fixtures.push(("{\"privatePayload\":\"secret-sentinel\"".into(), 1, false));
    for (index, (body, code, accepted)) in fixtures.into_iter().enumerate() {
        let executable = directory.join(format!("reader-{index}"));
        let script =
            format!("#!/bin/sh\ncat <<'FIXED_REPORT'\n{body}\nFIXED_REPORT\nexit {code}\n");
        artifacts::new_file(&executable, script.as_bytes()).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hash = provenance::hash_file(&executable).unwrap().0;
        let host = NativeHost::default();
        let reader = NativeReports {
            host: &host,
            generator: executable,
            generator_sha256: hash,
            directory: directory.clone(),
            environment: BTreeMap::new(),
        };
        let result = reader
            .read(Request {
                mode: "media",
                path: &path,
                input: &samples,
                admitted: false,
                expected_participants: 2,
            })
            .await;
        assert_eq!(result.is_ok(), accepted, "fixture{index}");
        if let Err(error) = result {
            assert_eq!(error.to_string(), "benchmark_report_command_failed");
            assert!(!format!("{error:#}").contains("secret-sentinel"));
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn output_number_domain_is_explicit_and_unknown_nested_arrays_never_escape() {
    let reference = json!({"coverage":{"complete":false},"consumers":[{"ordinal":1}],
        "schemaVersion":1});
    let equivalent: Value = serde_json::from_str(
        "{\"schemaVersion\":1.0,\"consumers\":[{\"ordinal\":1.0}],\"coverage\":{\"complete\":false}}",
    ).unwrap();
    assert!(super::reports::checked(equivalent, reference.clone(), 1).is_ok());
    let mut private = reference.clone();
    private["consumers"][0]["privatePayload"] = json!("secret-sentinel");
    assert!(super::reports::checked(private, reference, 1).is_err());
}
