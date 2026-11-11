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
