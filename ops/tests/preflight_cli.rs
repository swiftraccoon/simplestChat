//! Parser/privacy checks never admit a native host inspection. Full success is
//! covered by the private owned-files adapter and actual Python oracle tests.

use serde_json::Value;
use std::process::Command;

#[test]
fn malformed_or_unselected_input_fails_before_native_host_access() {
    for (raw, expected) in [
        ("PRIVATE_NOT_JSON".to_owned(), "preflight_unavailable"),
        ("[]".to_owned(), "invalid_helper_set"),
        (
            "{\"PRIVATE_KEY\":\"PRIVATE_VALUE\"}".to_owned(),
            "invalid_helper_set",
        ),
        (" ".repeat(4097), "invalid_request"),
        ("{\"x\":1,\"x\":2}".to_owned(), "preflight_unavailable"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(["release", "preflight", "--helpers-json", &raw])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "schemaVersion":1,"passed":false,"failure":expected,
            })
        );
        assert!(
            !String::from_utf8(output.stdout)
                .unwrap()
                .contains("PRIVATE")
        );
    }
}

#[test]
fn fixed_route_has_no_path_or_fixture_override() {
    for args in [
        vec!["release", "preflight", "--path", "PRIVATE_PATH"],
        vec!["release", "preflight", "--root", "PRIVATE_PATH"],
        vec!["release", "preflight", "--helpers-json"],
        vec!["release", "preflight", "{}"],
        vec!["release", "preflight", "--helpers-json", "{}", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(text.contains("release preflight [--helpers-json JSON]"));
        assert!(!text.contains("PRIVATE"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("release preflight")
    );
}
