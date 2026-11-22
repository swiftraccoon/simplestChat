//! The scheduled infrastructure job's workflow: the daily full scope through
//! the Rust controller, and the manual boot-only diagnosis, which receives
//! no credential and cannot stand in for the scheduled signed validation.

use super::{run, steps, unique, workflow};

/// A step's script as one line: continuations joined, whitespace collapsed.
fn joined(script: &str) -> String {
    let script = script.replace("\\\n", " ");
    script.split_whitespace().collect::<Vec<_>>().join(" ")
}

const FULL: &str =
    "Acquire the signed main release, apply twice and verify host policy and isolated restore";
const BOOT: &str = "Diagnose only the owned Debian guest boot";

/// Scheduled and default runs take the full scope with the job's token; only
/// an explicit dispatch takes the boot scope, and it gets no token.
#[test]
fn workflow_manual_mode_cannot_replace_scheduled_signed_validation() {
    let security = workflow("infrastructure-security.yml");
    let trigger = security["on"].as_object().unwrap();
    let option = &trigger["workflow_dispatch"]["inputs"]["boot_only"];
    assert_eq!(option["type"], "boolean");
    assert_eq!(option["default"], false);
    assert!(trigger.contains_key("schedule"));
    // The token reads the repository and its runs' artifacts, nothing more.
    assert_eq!(
        security["permissions"],
        serde_json::json!({"contents": "read", "actions": "read"})
    );
    let job = &security["jobs"]["infrastructure"];
    assert!(job.get("permissions").is_none());
    let name = job["name"].as_str().unwrap();
    assert!(name.contains("boot diagnostics"));
    assert!(name.contains("Debian VM deployment and restore policy"));
    assert_eq!(job["timeout-minutes"], 60);
    let steps = steps(&security, "infrastructure");
    let full = unique(steps, FULL);
    assert_eq!(full["if"], "${{ !inputs.boot_only }}");
    assert_eq!(full["timeout-minutes"], 55);
    assert_eq!(full["env"]["GH_TOKEN"], "${{ github.token }}");
    let command = joined(run(full));
    assert_eq!(
        command,
        "target/debug/simplestchat-release ops-artifact infrastructure-runtime \
         --output \"$RUNNER_TEMP/security-vm\""
    );
    let boot = unique(steps, BOOT);
    assert_eq!(
        boot["if"],
        "${{ github.event_name == 'workflow_dispatch' && inputs.boot_only }}"
    );
    assert_eq!(boot["timeout-minutes"], 35);
    assert!(boot.get("env").is_none());
    let command = joined(run(boot));
    assert_eq!(
        command,
        "target/debug/simplestchat-release ops-artifact infrastructure-runtime --boot-only \
         --output \"$RUNNER_TEMP/security-vm\""
    );
    // No step runs repository Python; the controller environment holds Ansible only.
    for step in steps {
        let script = step["run"].as_str().unwrap_or_default();
        assert!(
            !script.contains(".py") && !script.contains("python -m"),
            "{script}"
        );
    }
    let summary = unique(steps, "Preserve only the sanitized VM summary");
    assert_eq!(summary["if"], "${{ always() }}");
    assert_eq!(
        summary["with"]["path"],
        "${{ runner.temp }}/security-vm/summary.json"
    );
}

/// The controller, its runner-side verifier, the pinned Ansible and the
/// pinned `gh` exist before either scope's step, and before any credential.
#[test]
fn every_tool_the_controller_retains_is_prepared_before_it_runs() {
    let security = workflow("infrastructure-security.yml");
    let steps = steps(&security, "infrastructure");
    let position = |name: &str| steps.iter().position(|step| step["name"] == name).unwrap();
    let build = unique(
        steps,
        "Compile the controller and its runner-side verifier before credentials",
    );
    let script = joined(run(build));
    assert_eq!(
        script,
        "cargo build --locked -p simplestchat-release --bin simplestchat-release \
         cargo build --locked --release --target x86_64-unknown-linux-gnu \
         -p simplestchat-ops --bin simplestchat-ops"
    );
    let prepare = unique(steps, "Install owned-guest prerequisites");
    let script = run(prepare);
    for text in [
        "-r ops/ansible/requirements.txt",
        "tools install --tools gh",
        "\"$RUNNER_TEMP/vm-controller/bin\" \"${vm_gh%/gh}\" >> \"$GITHUB_PATH\"",
        "test -c /dev/kvm",
    ] {
        assert!(script.contains(text), "{text}");
    }
    for step in [build, prepare] {
        assert!(step.get("if").is_none() && step.get("env").is_none());
    }
    for scope in [FULL, BOOT] {
        assert!(position(prepare["name"].as_str().unwrap()) < position(scope));
        assert!(position(build["name"].as_str().unwrap()) < position(scope));
    }
}
