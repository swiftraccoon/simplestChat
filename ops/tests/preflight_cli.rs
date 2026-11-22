//! Parser/privacy checks never admit a native host inspection. Full success is
//! covered by the private owned-files adapter.

use std::process::Command;

#[test]
fn fixed_route_takes_no_argument_path_request_or_fixture_override() {
    for args in [
        vec!["release", "preflight", "--path", "PRIVATE_PATH"],
        vec!["release", "preflight", "--root", "PRIVATE_PATH"],
        vec!["release", "preflight", "--request"],
        vec![
            "release",
            "preflight",
            "--request",
            "{\"PRIVATE\":\"VALUE\"}",
        ],
        vec!["release", "preflight", "{}"],
        vec!["release", "preflight", "--request", "{}", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(text.contains("       simplestchat-ops release preflight\n"));
        assert!(!text.contains("release preflight ["));
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
