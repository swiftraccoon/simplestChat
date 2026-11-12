//! The relay commands refuse malformed invocations, misplaced address
//! overrides and unprivileged callers before touching anything.
use std::process::{Command, Output};

const FAMILY: &str = concat!(
    "simplestchat-ops turn prepare-certificate|refresh-certificate|enable-metrics|activate ",
    "[--verify-address IP]",
);

fn ops(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn malformed_invocations_are_usage_errors_without_effects() {
    for arguments in [
        &["turn"][..],
        &["turn", "ACTIVATE"],
        &["turn", "activate", "now"],
        &["turn", "activate", "--revision", "PRIVATE_VALUE"],
        &["turn", "certificate"],
        &["turn", "PREPARE-CERTIFICATE"],
        &["turn", "prepare-certificate", "PRIVATE_VALUE"],
        &["turn", "refresh-certificate", "--verify-address"],
        &["turn", "refresh-certificate", "--verify"],
        &["turn", "refresh-certificate", "--verify", "192.0.2.45"],
        &[
            "turn",
            "refresh-certificate",
            "--verify-address",
            "192.0.2.45",
            "--verify-address",
            "192.0.2.46",
        ],
        &["turn", "--config", "PRIVATE_PATH", "enable-metrics"],
        &["turn", "--verify-address", "192.0.2.45"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"), "{arguments:?}");
        assert!(text.contains(FAMILY), "{arguments:?}");
    }
}

#[test]
fn refused_requests_get_one_fixed_line_that_names_only_the_kind() {
    let refused =
        |kind: &str| format!("Relay operation failed ({kind}); inspect private evidence.\n");
    let mut cases = vec![
        (
            &[
                "turn",
                "prepare-certificate",
                "--verify-address",
                "192.0.2.45",
            ][..],
            "ReleaseError",
        ),
        (
            &[
                "turn",
                "refresh-certificate",
                "--verify-address",
                "PRIVATE.example",
            ],
            "ValueError",
        ),
        (
            &["turn", "--verify-address=192.0.2.300", "enable-metrics"],
            "ValueError",
        ),
        (
            &["turn", "activate", "--verify-address", "PRIVATE.example"],
            "ValueError",
        ),
    ];
    if !rustix::process::geteuid().is_root() {
        // As root these would act on the machine's own fixed paths.
        cases.extend([
            (&["turn", "prepare-certificate"][..], "ReleaseError"),
            (&["turn", "refresh-certificate"], "ReleaseError"),
            (
                &["turn", "enable-metrics", "--verify-address=2001:db8::45"],
                "ReleaseError",
            ),
            (&["turn", "activate"], "ReleaseError"),
            (
                &["turn", "activate", "--verify-address=192.0.2.45"],
                "ReleaseError",
            ),
        ]);
    }
    for (arguments, kind) in cases {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(text, refused(kind), "{arguments:?}");
    }
}
