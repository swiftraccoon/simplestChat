//! The monitoring and database-logging commands refuse malformed invocations
//! and unprivileged callers before touching anything.
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
        &["monitor"][..],
        &["monitor", "collect", "now"],
        &["monitor", "credential", "PRIVATE_PATH"],
        &["monitor", "alerts"],
        &["monitor", "--state", "PRIVATE_PATH"],
        &["database"],
        &["database", "logging"],
        &["database", "logging", "apply"],
        &["database", "logging", "disable", "PRIVATE_VALUE"],
    ] {
        let output = ops(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("PRIVATE"), "{arguments:?}");
        let family = "simplestchat-ops monitor collect|credential|external|prepare|report\n";
        assert!(text.contains(family));
        assert!(text.contains("simplestchat-ops database logging disable"));
    }
}

#[test]
fn an_unprivileged_caller_gets_each_commands_fixed_refusal() {
    if rustix::process::geteuid().is_root() {
        // As root these would act on the machine's own fixed paths.
        return;
    }
    for (arguments, refusal) in [
        (
            &["monitor", "collect"][..],
            "Monitoring collection failed; inspect the collector's private state.\n",
        ),
        (
            &["monitor", "credential"],
            "Scrape credential preparation failed; inspect the prepared environment privately.\n",
        ),
        (
            &["monitor", "external"],
            "External check import failed; previous evidence remains private.\n",
        ),
        (
            &["monitor", "prepare"],
            "Operational schema preparation failed; inspect the database privately.\n",
        ),
        (
            &["monitor", "report"],
            "Operational report unavailable; inspect private monitoring health.\n",
        ),
        (
            &["database", "logging", "disable"],
            "Database logging remediation failed (ReleaseError); inspect private evidence.\n",
        ),
    ] {
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
