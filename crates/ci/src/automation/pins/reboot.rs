//! The reboot playbook: explicit authorization, the signed Ops executable,
//! and one prepared stop, one reboot and one recovery of the same
//! containers. Nothing connects to a host.

use super::checkout_file;
use super::monitoring::{
    LOOK, OPS, REQUIRE, environment, named, names, play, requirement, section,
};
use serde_json::{Value, json};

const REBOOT: &str = "ops/ansible/reboot.yml";
const WORKFLOW: &str = "Prepare, reboot once and recover the same public containers";
const UPGRADE: &str = "scpub_upgrade_runtime | default(false) | bool";

/// The bounded transient unit one reboot action runs in.
fn unit(action: &str, stop_seconds: u32) -> Value {
    json!({
        "ansible.builtin.command": {"argv": [
            "systemd-run",
            format!("--unit=simplestchat-reboot-{action}-{{{{ ansible_facts.date_time.epoch }}}}"),
            "--property=Type=exec",
            "--property=RuntimeMaxSec=900",
            format!("--property=TimeoutStopSec={stop_seconds}"),
            "--wait",
            OPS,
            "reboot",
            action,
        ]},
        "environment": environment(),
    })
}

/// Whether `task` runs exactly the unit of `action`, whatever else it sets.
fn runs(task: &Value, action: &str, stop_seconds: u32) -> bool {
    let unit = unit(action, stop_seconds);
    let fields = unit.as_object().unwrap();
    fields.iter().all(|(key, value)| task[key] == *value)
}

/// From `test_explicit_authorization_and_existing_host_guards_precede_reboot`:
/// one host at a time, an explicit opt-in that no tag can skip, an existing
/// protected selection, and the executable a signed release delivered. The
/// playbook copies nothing to the host and installs or restarts no package
/// of its own.
#[test]
fn reboot_needs_explicit_authorization_and_the_signed_ops_executable() {
    let play = play(REBOOT);
    assert_eq!(play["serial"], 1);
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        [
            "Require explicit reboot authorization and an existing public deployment",
            "Require existing protected public configuration",
            "Refuse to use reboot as initial provisioning",
            LOOK,
            REQUIRE,
        ]
    );
    let guard = &pre_tasks[0];
    assert_eq!(guard["tags"], json!(["always"]));
    let rules = guard["ansible.builtin.assert"]["that"].as_array().unwrap();
    for rule in [
        "scpub_enabled | bool",
        "scpub_reboot | default(false) | bool",
    ] {
        assert!(rules.iter().any(|listed| listed == rule), "{rule}");
    }
    requirement(pre_tasks, "scpub_reboot_ops");
    let tasks = section(&play, "tasks");
    assert_eq!(
        names(tasks),
        [
            "Verify runtime upgrade compatibility before stopping any services",
            WORKFLOW,
        ]
    );
    let text = checkout_file(REBOOT);
    for absent in [
        "python",
        ".py",
        "ansible.builtin.copy",
        "ansible.builtin.apt",
        "ansible.builtin.service",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
}

/// Preparation, the deferred runtime upgrade, the reboot, its verification
/// and the recovery run in that order and never in check mode. Each action
/// is the installed executable in a bounded unit with a fixed environment;
/// a preparation that is told to stop keeps eight minutes to start its
/// services again.
#[test]
fn reboot_prepares_reboots_and_recovers_through_the_ops_executable() {
    let play = play(REBOOT);
    let workflow = named(section(&play, "tasks"), WORKFLOW);
    assert_eq!(workflow["when"], "not ansible_check_mode");
    let block = section(workflow, "block");
    assert_eq!(
        names(block),
        [
            "Prepare the reboot with private evidence and ordered graceful service stops",
            "Install the reviewed runtime packages only after ordered service stops",
            "Reboot the explicitly authorized host and wait for Docker",
            "Verify all selected runtime component versions after the reboot",
            "Restore the prepared services only after the boot identity changed",
        ]
    );
    assert!(runs(&block[0], "prepare", 480));
    assert_eq!(block[0]["changed_when"], true);
    assert_eq!(
        block[1]["ansible.builtin.include_tasks"],
        "tasks/docker.yml"
    );
    assert_eq!(
        block[1]["vars"],
        json!({"scbench_docker_defer_restart": true})
    );
    assert_eq!(block[1]["when"], UPGRADE);
    let reboot = &block[2]["ansible.builtin.reboot"];
    assert_eq!(
        reboot["test_command"],
        "/usr/bin/systemctl is-active docker.service"
    );
    assert_eq!(reboot["reboot_timeout"], 600);
    assert_eq!(
        block[3]["ansible.builtin.include_tasks"],
        "tasks/runtime-verify.yml"
    );
    assert_eq!(block[3]["when"], UPGRADE);
    assert!(runs(&block[4], "resume", 60));
    assert_eq!(block[4]["changed_when"], true);
    for task in [&block[0], &block[4]] {
        for key in ["when", "failed_when", "ignore_errors", "ignore_unreachable"] {
            assert!(task.get(key).is_none(), "{key}");
        }
    }
}

/// A failed step is answered by one bounded cancellation on the original
/// boot, whose own result never decides the play: the failure that started
/// the rescue is reported whatever the cancellation did.
#[test]
fn a_failed_reboot_is_cancelled_once_and_stays_a_failure() {
    let play = play(REBOOT);
    let workflow = named(section(&play, "tasks"), WORKFLOW);
    let rescue = section(workflow, "rescue");
    assert_eq!(
        names(rescue),
        [
            "Attempt bounded service recovery only if the original boot remains active",
            "Preserve the failed reboot result even when original-boot recovery succeeds",
        ]
    );
    assert!(runs(&rescue[0], "cancel", 60));
    assert_eq!(rescue[0]["failed_when"], false);
    assert_eq!(rescue[0]["ignore_unreachable"], true);
    assert_eq!(rescue[0]["register"], "scpub_reboot_cancellation");
    assert_eq!(
        rescue[0]["changed_when"],
        "scpub_reboot_cancellation.rc | default(1) == 0"
    );
    let failure = rescue[1]["ansible.builtin.fail"]["msg"].as_str().unwrap();
    assert!(failure.contains("recovery never converts this failure into success"));
    assert!(rescue[1].get("when").is_none());
}
