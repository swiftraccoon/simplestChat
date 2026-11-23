//! The reboot commands refuse malformed invocations and unprivileged callers
//! before touching anything.
use std::process::{Command, Output};

fn ops(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn malformed_invocations_are_usage_errors_without_effects() {
    for arguments in [
        &["reboot"][..],
        &["reboot", "now"],
        &["reboot", ""],
        &["reboot", "PREPARE"],
        &["reboot", "prepare", "PRIVATE_VALUE"],
        &["reboot", "prepare", "resume"],
        &["reboot", "--force", "prepare"],
        &["reboot", "cancel", "--boot", "PRIVATE_VALUE"],
        &["reboot", "compare-compose"],
        &["reboot", "compare-compose", "/PRIVATE_PATH"],
        &["reboot", "compare-compose", "PRIVATE_PATH", "5.6.0"],
        &["reboot", "compare-compose", "/PRIVATE_PATH", ""],
        &[
            "reboot",
            "compare-compose",
            "/PRIVATE_PATH",
            "5.6.0",
            "PRIVATE_VALUE",
        ],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"), "{arguments:?}");
        let family = "simplestchat-ops reboot prepare|resume|cancel";
        assert!(text.contains(family), "{arguments:?}");
    }
}

#[test]
fn an_unprivileged_caller_gets_one_fixed_line_and_no_outcome() {
    if rustix::process::geteuid().is_root() {
        // As root these would act on the machine's own fixed paths.
        return;
    }
    for action in ["prepare", "resume", "cancel"] {
        let output = ops(&["reboot", action]);
        assert_eq!(output.status.code(), Some(1), "{action}");
        assert!(output.stdout.is_empty(), "{action}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "Reboot operation failed (ReleaseError); inspect retained private evidence.\n",
            "{action}"
        );
    }
}

/// A candidate that cannot run fails the comparison in its fixed words,
/// before any other command and without output.
#[test]
fn a_candidate_that_cannot_run_fails_the_compose_comparison() {
    let output = ops(&["reboot", "compare-compose", "/nonexistent/compose", "5.6.0"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Compose compatibility command failed; no services were stopped\n"
    );
}
