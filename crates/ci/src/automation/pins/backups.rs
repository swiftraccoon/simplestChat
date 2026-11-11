//! The nightly backup playbook, its wrapper script and units, and the
//! source-built Ops executable the infrastructure proof's guest runs them
//! with. Nothing connects to a host.

use super::ansible::{asserted, render};
use super::monitoring::{LOOK, OPS, REQUIRE, named, names, play, requirement, section};
use super::{checkout_file, document, run, steps, unique, workflow};
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

const GROUP_VARS: &str = "ops/ansible/group_vars/benchmark_hosts.yml";
const BUILT: &str = "target/x86_64-unknown-linux-gnu/release/simplestchat-ops";

/// The playbook copies no helper to the host: it requires the executable a
/// signed release delivered, then installs only the wrapper and the units.
#[test]
fn nightly_backup_installs_only_its_wrapper_and_units() {
    let play = play("ops/ansible/backup.yml");
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        [
            "Require the prepared public host and an explicit opt-in",
            LOOK,
            REQUIRE,
            "Require the public database container",
        ]
    );
    requirement(pre_tasks, "scpub_backup_ops");
    let tasks = section(&play, "tasks");
    assert_eq!(
        names(tasks),
        [
            "Create the private backup directory",
            "Install the backup script",
            "Install the backup units",
            "Enable the nightly timer",
        ]
    );
    let script = named(tasks, "Install the backup script");
    assert_eq!(
        script["ansible.builtin.template"],
        json!({
            "src": "backup-nightly.sh.j2",
            "dest": "/usr/local/libexec/simplestchat-public/backup-nightly.sh",
            "owner": "root", "group": "root", "mode": "0700",
            "validate": "/usr/bin/bash -n %s",
        })
    );
    let text = checkout_file("ops/ansible/backup.yml");
    for absent in ["python", ".py", "ansible.builtin.copy"] {
        assert!(!text.contains(absent), "{absent}");
    }
}

/// From `test_playbook_bounds_retention_before_installing_anything`: the
/// opt-in and both retention bounds are decided by Ansible before Jinja's
/// integer conversion could lose information.
#[test]
fn retention_is_bounded_before_anything_is_installed() {
    let play = play("ops/ansible/backup.yml");
    let opt_in = &section(&play, "pre_tasks")[0];
    let rules = &opt_in["ansible.builtin.assert"]["that"];
    let selected = json!({"scpub_enabled": true, "scpub_backup_enabled": true});
    let with = |pairs: &[(&str, Value)]| {
        let mut case = selected.clone();
        for (name, value) in pairs {
            case[*name] = value.clone();
        }
        case
    };
    let days = "scpub_backup_keep_days";
    let count = "scpub_backup_keep_at_least";
    let cases = [
        (selected.clone(), true),
        (with(&[(days, json!(1)), (count, json!(1))]), true),
        (with(&[(days, json!(3650)), (count, json!(365))]), true),
        (with(&[("scpub_backup_enabled", json!(false))]), false),
        (with(&[("scpub_enabled", json!(false))]), false),
        (with(&[(days, json!(0))]), false),
        (with(&[(days, json!(3651))]), false),
        (with(&[(days, json!("14"))]), false),
        (with(&[(days, json!(14.5))]), false),
        (with(&[(count, json!(0))]), false),
        (with(&[(count, json!(366))]), false),
        (with(&[(count, json!("3"))]), false),
        (json!({"scpub_enabled": true}), false),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(rules, &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// From `test_wrapper_preserves_failure_and_runs_guarded_exit_cleanup`: the
/// wrapper runs the installed executable with the bounded retention, keeps
/// its exit status and always retries the guarded cleanup on exit.
#[test]
fn wrapper_runs_the_ops_executable_and_always_retries_guarded_cleanup() {
    let wrapper = "backup-nightly.sh.j2";
    let defaults = render(&[wrapper], GROUP_VARS, &json!({}));
    let selected = json!({"scpub_backup_keep_days": 30, "scpub_backup_keep_at_least": 7});
    let chosen = render(&[wrapper], GROUP_VARS, &selected);
    for (script, days, count) in [(&defaults[wrapper], 14, 3), (&chosen[wrapper], 30, 7)] {
        let mut bash = Command::new("bash");
        bash.arg("-n").stdin(Stdio::piped());
        let mut child = bash
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let checked = child.wait_with_output().unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let lines: Vec<&str> = script.lines().collect();
        for expected in [
            "set -euo pipefail",
            "umask 077",
            "export PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            &format!("ops={OPS}"),
            &format!("keep_days={days}"),
            &format!("keep_at_least={count}"),
            "  \"$ops\" backup cleanup >/dev/null 2>&1 || true",
            "  exit \"$status\"",
            "trap cleanup EXIT",
            "trap 'exit 143' TERM",
            "trap 'exit 130' INT",
            "\"$ops\" backup nightly --keep-days \"$keep_days\" --keep-at-least \"$keep_at_least\"",
        ] {
            assert_eq!(
                lines.iter().filter(|line| **line == expected).count(),
                1,
                "{expected}"
            );
        }
        assert!(!script.contains("python") && !script.contains(".py"));
        assert_eq!(script.matches("\"$ops\"").count(), 2);
    }
}

/// From `test_units_share_the_canonical_runtime_lock_directory`: the timer
/// survives a reboot, and the service can write only the archive directory
/// and the lock's runtime directory, which it keeps.
#[test]
fn units_share_the_canonical_runtime_lock_directory() {
    let timer = checkout_file("ops/ansible/templates/backup-nightly.timer.j2");
    for kept in ["OnCalendar=*-*-* 04:00:00 UTC", "Persistent=true"] {
        assert!(timer.lines().any(|line| line == kept), "{kept}");
    }
    let service = checkout_file("ops/ansible/templates/backup-nightly.service.j2");
    let lines: Vec<&str> = service.lines().collect();
    for kept in [
        "Type=oneshot",
        "RuntimeDirectory=simplestchat-bench",
        "RuntimeDirectoryMode=0700",
        "RuntimeDirectoryPreserve=yes",
        "ExecStart=/usr/bin/bash /usr/local/libexec/simplestchat-public/backup-nightly.sh",
        "UMask=0077",
        "ReadWritePaths=/srv/simplestchat-public/backups /run/simplestchat-bench",
        "ProtectSystem=strict",
        "NoNewPrivileges=true",
    ] {
        assert!(lines.contains(&kept), "{kept}");
    }
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("Exec")).count(),
        1
    );
    for unit in [&timer, &service] {
        assert!(!unit.contains("{{") && !unit.contains("{%") && !unit.contains("python"));
    }
}

/// The infrastructure proof's guest is provisioned from source and has no
/// signed release, so its stage playbook installs the executable the workflow
/// built from the same checkout, before the playbooks that require it.
#[test]
fn infrastructure_guest_receives_the_source_built_ops_executable() {
    let stage = document("security/vm/stage.yml");
    let tasks = stage[0]["tasks"].as_array().unwrap();
    let install = named(
        tasks,
        "Install the source-built Ops executable the backup units run",
    );
    assert_eq!(
        install["ansible.builtin.copy"],
        json!({
            "src": format!("{{{{ playbook_dir }}}}/../../{BUILT}"),
            "dest": OPS,
            "owner": "root", "group": "root", "mode": "0755",
        })
    );
    assert!(install.get("when").is_none());
    let security = workflow("infrastructure-security.yml");
    let steps = steps(&security, "infrastructure");
    let position = |name: &str| steps.iter().position(|step| step["name"] == name).unwrap();
    let toolchain = unique(
        steps,
        "Install the pinned Rust toolchain for the guest's Ops executable",
    );
    assert_eq!(
        run(toolchain).trim(),
        "rustup toolchain install --profile minimal"
    );
    let build = unique(steps, "Build the Ops executable the guest's units run");
    let command = "cargo build --locked --release --target x86_64-unknown-linux-gnu \
                   -p simplestchat-ops --bin simplestchat-ops";
    assert_eq!(
        run(build).split_whitespace().collect::<Vec<_>>().join(" "),
        command
    );
    assert!(BUILT.starts_with("target/x86_64-unknown-linux-gnu/release/"));
    for step in [toolchain, build] {
        assert_eq!(step["if"], "${{ !inputs.boot_only }}");
        assert!(step.get("env").is_none() && step.get("continue-on-error").is_none());
    }
    let applied = position("Apply twice and verify host policy and isolated restore");
    assert!(
        position(toolchain["name"].as_str().unwrap()) < position(build["name"].as_str().unwrap())
    );
    assert!(position(build["name"].as_str().unwrap()) < applied);
}
