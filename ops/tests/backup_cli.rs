//! The backup commands refuse malformed invocations, unbounded retention and
//! unprivileged callers before touching anything.
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
        &["backup"][..],
        &["backup", "restore"],
        &["backup", "nightly", "PRIVATE_VALUE"],
        &["backup", "nightly", "--keep-days"],
        &["backup", "nightly", "--keep-days", "many"],
        &["backup", "nightly", "--keep-days", "1", "--keep-days", "2"],
        &["backup", "nightly", "--keep-days", "1e3"],
        &["backup", "cleanup", "--target", "PRIVATE_PATH"],
        &["backup", "--keep-days", "3", "nightly"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"), "{arguments:?}");
        let family =
            "simplestchat-ops backup nightly|cleanup [--keep-days N] [--keep-at-least N]\n";
        assert!(text.contains(family), "{arguments:?}");
    }
}

#[test]
fn unbounded_retention_and_unprivileged_callers_get_the_fixed_refusal() {
    let refusal = "Nightly backup failed; inspect the private backup logs and journals.\n";
    let mut cases = vec![
        &["backup", "nightly", "--keep-days", "0"][..],
        &["backup", "nightly", "--keep-days=3651"],
        &["backup", "cleanup", "--keep-at-least", "-1"],
        &["backup", "cleanup", "--keep-at-least=366"],
    ];
    if !rustix::process::geteuid().is_root() {
        // As root these two would act on the machine's own fixed paths.
        cases.extend([&["backup", "nightly"][..], &["backup", "cleanup"]]);
    }
    for arguments in cases {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            refusal,
            "{arguments:?}"
        );
    }
}
