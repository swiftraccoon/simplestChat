//! The release runtime workflows build the controller their Python helpers
//! open. Each workflow compiles `simplestchat-release` in one step and runs a
//! `build/security_vm_*.py` helper in a later one; the helper opens the
//! controller at a fixed checkout path, so the two must name the same file.

use super::{checkout_file, steps, workflow};
use serde_json::Value;

/// Each workflow, its job and the helper that job runs.
const PAIRS: [(&str, &str, &str); 3] = [
    (
        "release-receive-runtime.yml",
        "release-receive-runtime",
        "build/security_vm_receive.py",
    ),
    (
        "release-backup-runtime.yml",
        "release-backup-runtime",
        "build/security_vm_restore.py",
    ),
    (
        "release-stage-runtime.yml",
        "release-stage-runtime",
        "build/security_vm_stage.py",
    ),
];
/// The controller build: no profile, target or target-directory option, so
/// Cargo writes the binary to the default debug directory.
const BUILD: &str = "cargo build --locked -p simplestchat-release --bin simplestchat-release";
/// Where that build leaves the controller, relative to the checkout.
const CONTROLLER: &str = "target/debug/simplestchat-release";

/// No environment at this level moves Cargo's output.
fn keeps_default_output(env: &Value) -> bool {
    env.get("CARGO_TARGET_DIR").is_none() && env.get("CARGO_BUILD_TARGET").is_none()
}

#[test]
fn each_release_runtime_builds_the_controller_its_helper_opens() {
    // No checkout carries a Cargo configuration that could move the output:
    // the repository ignores `.cargo/`, so a plain debug build lands in
    // target/debug on every runner.
    assert!(
        checkout_file(".gitignore")
            .lines()
            .any(|line| line == "/.cargo/")
    );
    for (name, job, helper) in PAIRS {
        let workflow = workflow(name);
        assert!(keeps_default_output(&workflow["env"]), "{name}");
        assert!(
            keeps_default_output(&workflow["jobs"][job]["env"]),
            "{name}"
        );
        let steps = steps(&workflow, job);
        // Exactly one step builds the controller, and that build is its only
        // mention of the controller crate.
        let builds = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("simplestchat-release"))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(builds.len(), 1, "{name}");
        let build = &steps[builds[0]];
        let lines = build["run"].as_str().unwrap().lines().collect::<Vec<_>>();
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("simplestchat-release"))
                .collect::<Vec<_>>(),
            [&BUILD],
            "{name}"
        );
        assert!(keeps_default_output(&build["env"]), "{name}");
        // A later step runs the helper.
        let invocation = format!("python3 -B {helper} ");
        let runs = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with(&invocation))
            })
            .unwrap_or_else(|| panic!("{name} runs {helper}"));
        assert!(builds[0] < runs, "{name}");
        // The helper opens exactly that controller and no other debug binary.
        let source = checkout_file(helper);
        assert_eq!(source.matches("\"target/debug/").count(), 1, "{helper}");
        assert_eq!(
            source.matches(&format!("ROOT / \"{CONTROLLER}\"")).count(),
            1,
            "{helper}"
        );
    }
}
