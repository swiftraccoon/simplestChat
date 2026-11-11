//! The manual release-artifact workflow and the tested image's export in CI,
//! carried over from the Python workflow tests. Each pin inspects workflow
//! structure offline; none invokes GitHub or a container engine.

use super::{checkout_file, document, located, script, steps, workflow};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const WORKFLOW: &str = ".github/workflows/release-artifact.yml";
const TOOLCHAIN_ACTION: &str = "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d";
const PREPARATION_COMMAND: &str = concat!(
    "cargo build --locked --release -p simplestchat-ops --bin simplestchat-ops \\\n",
    "  --target-dir \"$RUNNER_TEMP/release-builder-build\"\n"
);
const BUILDER_COMMAND: &str = concat!(
    "\"$RUNNER_TEMP/release-builder-build/release/simplestchat-ops\" release build \\\n",
    "  --output \"${RUNNER_TEMP}/simplestchat-release\"\n"
);

/// The keys of a mapping.
fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

/// A step's `uses`, which must be text when present.
fn uses(step: &Value) -> Option<&str> {
    step.get("uses").map(|uses| uses.as_str().unwrap())
}

/// From `test_release_build_is_manual_only_with_read_only_repository_permissions`.
#[test]
fn release_build_is_manual_only_with_read_only_repository_permissions() {
    let release = document(WORKFLOW);
    assert_eq!(keys(&release["on"]), ["workflow_dispatch"].into());
    assert_eq!(release["permissions"], json!({"contents": "read"}));
    assert_eq!(keys(&release["jobs"]), ["artifact"].into());
    let job = release["jobs"]["artifact"].as_object().unwrap();
    assert!(
        !job.contains_key("permissions"),
        "The job must not escalate the workflow token"
    );
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 75);
}

/// From `test_only_checkout_toolchain_and_retention_actions_are_used_and_pinned`:
/// the selected compiler bootstrap is pinned alongside checkout and retention.
#[test]
fn only_checkout_toolchain_and_retention_actions_are_used_and_pinned() {
    let release = document(WORKFLOW);
    let actions = steps(&release, "artifact")
        .iter()
        .filter(|step| step.get("uses").is_some())
        .collect::<Vec<_>>();
    assert_eq!(actions.len(), 3);
    let names = [
        "actions/checkout",
        "actions/upload-artifact",
        "dtolnay/rust-toolchain",
    ];
    assert_eq!(
        actions
            .iter()
            .map(|step| uses(step).unwrap().split('@').next().unwrap())
            .collect::<BTreeSet<_>>(),
        names.into()
    );
    for step in &actions {
        // Exactly a permitted action at a full lower-case commit.
        let (name, commit) = uses(step).unwrap().split_once('@').unwrap();
        assert!(names.contains(&name), "{name}");
        assert_eq!(commit.len(), 40, "{name}");
        assert!(
            commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "{name}"
        );
    }
    let checkout = actions
        .iter()
        .find(|step| uses(step).unwrap().starts_with("actions/checkout@"))
        .unwrap();
    assert_eq!(checkout["with"], json!({"persist-credentials": false}));
    let toolchain = actions
        .iter()
        .find(|step| uses(step) == Some(TOOLCHAIN_ACTION))
        .unwrap();
    assert_eq!(toolchain["with"], json!({"toolchain": "1.99.0"}));
    assert_eq!(toolchain["timeout-minutes"], 2);
}

/// From `test_native_preparation_precedes_the_exact_production_builder`: cold
/// source compilation stays outside the operation's original entry clock.
#[test]
fn native_preparation_precedes_the_exact_production_builder() {
    let release = document(WORKFLOW);
    let steps = steps(&release, "artifact");
    let commands = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.get("run").is_some())
        .collect::<Vec<_>>();
    let [(prepared, preparation), (built, builder)] = commands[..] else {
        panic!("exactly two commands");
    };
    assert_eq!(preparation["run"], PREPARATION_COMMAND);
    assert_eq!(preparation["timeout-minutes"], 6);
    assert_eq!(builder["run"], BUILDER_COMMAND);
    assert_eq!(builder["timeout-minutes"], 65);
    for step in [preparation, builder] {
        // An unconditional selected-commit build must not be silently skipped.
        let step = step.as_object().unwrap();
        assert!(!step.contains_key("if"));
        assert!(!step.contains_key("continue-on-error"));
    }
    let job = release["jobs"]["artifact"].as_object().unwrap();
    assert!(!job.contains_key("continue-on-error"));
    assert_eq!(steps.len(), 5);
    assert!(0 < built);
    assert!(prepared < built);
    let (toolchain, _) = located(steps, |step| uses(step) == Some(TOOLCHAIN_ACTION));
    assert!(toolchain < prepared);
    assert!(uses(&steps[0]).unwrap().starts_with("actions/checkout@"));
}

/// From `test_ci_export_uses_the_tested_image_with_separate_native_preparation`:
/// the six-minute export bound and the immutable image selection stay.
#[test]
fn ci_export_uses_the_tested_image_with_separate_native_preparation() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "deployment");
    let (prepared, preparation) = located(steps, |step| step["run"] == PREPARATION_COMMAND);
    assert_eq!(preparation["timeout-minutes"], 6);
    let (exported, export) = located(steps, |step| {
        step["name"] == "Export the tested immutable production image"
    });
    assert_eq!(export["timeout-minutes"], 6);
    assert_eq!(
        export["run"],
        concat!(
            "\"$RUNNER_TEMP/release-builder-build/release/simplestchat-ops\" ",
            "release build \\\n  --image-id \"${PRODUCTION_IMAGE}\" \\\n",
            "  --timeout-seconds 300 --output \"${RUNNER_TEMP}/simplestchat-release\"\n"
        )
    );
    let (smoked, _) = located(steps, |step| {
        step["name"] == "Production startup, migration and database-backed API smoke"
    });
    assert!(prepared < smoked);
    assert!(smoked < exported);
}

/// From `test_original_failed_outcome_is_retained_without_turning_build_failure_into_success`.
#[test]
fn original_failed_outcome_is_retained_without_turning_build_failure_into_success() {
    let release = document(WORKFLOW);
    let steps = steps(&release, "artifact");
    let (retained, retention) = located(steps, |step| {
        uses(step).is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
    });
    // Failed-build evidence is retained, without retrying cancelled jobs.
    assert_eq!(retention["if"], "${{ !cancelled() }}");
    assert_eq!(
        retention["with"],
        json!({
            "name": "simplestchat-development-${{ github.sha }}",
            "path": "${{ runner.temp }}/simplestchat-release",
            "if-no-files-found": "error",
            "compression-level": 0,
            "retention-days": 14,
        })
    );
    assert_eq!(retained, steps.len() - 1);
    let retention = retention.as_object().unwrap();
    assert!(!retention.contains_key("continue-on-error"));
    // Retention must not rewrite the builder's original outcome.
    assert!(!retention.contains_key("run"));
}

/// From `test_no_deployment_publication_credentials_or_service_side_effects_are_configured`.
#[test]
fn no_deployment_publication_credentials_or_service_side_effects_are_configured() {
    let release = document(WORKFLOW);
    let steps = steps(&release, "artifact");
    let scopes = [&release, &release["jobs"]["artifact"]]
        .into_iter()
        .chain(steps);
    for scope in scopes {
        let scope = scope.as_object().unwrap();
        for key in ["environment", "env", "services", "secrets"] {
            assert!(!scope.contains_key(key), "{key}");
        }
    }
    let secrets = regex::Regex::new(r"\$\{\{\s*secrets\.").unwrap();
    assert!(!secrets.is_match(&checkout_file(WORKFLOW)));
    let commands = steps.iter().map(script).collect::<Vec<_>>().join("\n");
    let effects =
        regex::Regex::new(r"\b(?:ssh|scp|ansible-playbook|push|publish|deploy|login|reboot)\b")
            .unwrap();
    assert!(!effects.is_match(&commands));
    // The sole publication is GitHub's retained build artifact, not a
    // registry push, release creation, VPS command or deploy environment.
    assert_eq!(
        commands,
        format!("\n\n{PREPARATION_COMMAND}\n{BUILDER_COMMAND}\n")
    );
}
