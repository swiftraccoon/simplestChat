//! CLI effects are refused before any host transaction for malformed selectors.
use std::process::Command;

fn cli(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(["release", "maintain"])
        .args(arguments)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn help_is_effect_free_and_names_the_complete_transaction() {
    let output = cli(&["--help"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let text = std::str::from_utf8(&output.stdout).unwrap();
    assert!(text.starts_with("Usage: simplestchat-ops release maintain --revision SHA40 "));
    assert!(text.contains("reacquire a fresh lock"));
    assert!(text.contains("Launcher failure retains the selected release"));
}

#[test]
fn malformed_cli_exposes_only_fixed_usage_before_root_or_runtime_effects() {
    for arguments in [
        vec![],
        vec!["--candidate-env", "PRIVATE_SECRET"],
        vec!["--revision", "PRIVATE_SECRET"],
        vec!["--help", "--host", "PRIVATE_SECRET"],
    ] {
        let output = cli(&arguments);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            output
                .stderr
                .starts_with(b"Usage: simplestchat-ops release maintain ")
        );
        assert!(
            !output
                .stderr
                .windows(b"PRIVATE_SECRET".len())
                .any(|w| w == b"PRIVATE_SECRET")
        );
    }
}
