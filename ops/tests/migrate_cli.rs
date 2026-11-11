//! The certificate handoff refuses malformed invocations and unprivileged
//! callers before touching anything, and never says why.
use std::process::{Command, Output};

const IDENTITY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn ops(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn malformed_invocations_are_usage_errors_without_effects() {
    for arguments in [
        &["migrate"][..],
        &["migrate", "tls"],
        &["migrate", "tls", "export"],
        &["migrate", "export", IDENTITY],
        &["migrate", "tls", "transfer", IDENTITY],
        &["migrate", "tls", "export", IDENTITY, "PRIVATE_VALUE"],
        &["migrate", "tls", "import", "--operation", IDENTITY],
        &["migrate", "tls", "import", "--help"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"), "{arguments:?}");
        let family = "simplestchat-ops migrate tls export|import OPERATION";
        assert!(text.contains(family), "{arguments:?}");
    }
}

#[test]
fn an_unprivileged_caller_gets_one_fixed_line() {
    if rustix::process::geteuid().is_root() {
        // As root these would act on the machine's own fixed paths.
        return;
    }
    for arguments in [
        &["migrate", "tls", "export", IDENTITY][..],
        &["migrate", "tls", "import", IDENTITY],
        &["migrate", "tls", "import", "PRIVATE_VALUE"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "Migration TLS handoff failed; private certificate state was preserved.\n",
            "{arguments:?}"
        );
    }
}
