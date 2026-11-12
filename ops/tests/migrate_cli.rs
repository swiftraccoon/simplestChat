//! The migration commands refuse malformed invocations and unprivileged
//! callers before touching anything. The certificate handoff and the data
//! steps never say why; the inspection prints one fixed code.
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
        &["migrate", "inspect"],
        &["migrate", "inspect", "source"],
        &[
            "migrate",
            "inspect",
            "source",
            "https://chat.example.test",
            "PRIVATE_VALUE",
        ],
        &["migrate", "review", "source", "https://chat.example.test"],
        &["migrate", "freeze-source"],
        &["migrate", "freeze-source", "--request"],
        &["migrate", "freeze-source", "--request=PRIVATE_VALUE"],
        &["migrate", "--request", "PRIVATE_VALUE", "freeze-source"],
        &["migrate", "freeze-source", "--req", "PRIVATE_VALUE"],
        &["migrate", "inspect-source", "--request", "PRIVATE_VALUE"],
        &["migrate", "thaw-source", "--request", "PRIVATE_VALUE"],
        &[
            "migrate",
            "restore-target",
            "--request",
            "PRIVATE_VALUE",
            "PRIVATE_VALUE",
        ],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"), "{arguments:?}");
        for family in [
            "simplestchat-ops migrate inspect source|target ORIGIN",
            "simplestchat-ops migrate freeze-source|recover-source --request PATH",
            "simplestchat-ops migrate seal-source|finalize-source --request PATH",
            "simplestchat-ops migrate restore-target|verify-target|abort-target --request PATH",
            "simplestchat-ops migrate tls export|import OPERATION",
        ] {
            assert!(text.contains(family), "{arguments:?}");
        }
    }
}

#[test]
fn an_unprivileged_inspection_prints_only_its_refusal() {
    if rustix::process::geteuid().is_root() {
        // As root this would read the machine's own configuration.
        return;
    }
    for arguments in [
        &["migrate", "inspect", "source", "https://chat.example.test"][..],
        &["migrate", "inspect", "target", "https://chat.example.test"],
        &["migrate", "inspect", "PRIVATE_VALUE", "PRIVATE_VALUE"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stderr.is_empty(), "{arguments:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "{\"passed\": false, \"failure\": \"root_required\"}\n",
            "{arguments:?}"
        );
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

#[test]
fn an_unprivileged_data_step_says_only_that_it_failed() {
    if rustix::process::geteuid().is_root() {
        // As root these would act on the machine's own deployment.
        return;
    }
    for step in [
        "freeze-source",
        "recover-source",
        "seal-source",
        "finalize-source",
        "restore-target",
        "verify-target",
        "abort-target",
    ] {
        let request = "/srv/simplestchat-public/migrations/PRIVATE_VALUE/request.json";
        let output = ops(&["migrate", step, "--request", request]);
        assert_eq!(output.status.code(), Some(1), "{step}");
        assert!(output.stdout.is_empty(), "{step}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "{\"passed\":false,\"error\":\"migration_action_failed\"}\n",
            "{step}"
        );
    }
}
