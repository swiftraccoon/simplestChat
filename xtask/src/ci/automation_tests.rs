//! Fixed command-policy checks; no host or installed controller is contacted.

use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new(names: &[&str]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "simplestchat-controller-gate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(root.join("ops/ansible/files")).unwrap();
        for name in names {
            fs::write(root.join("ops/ansible/files").join(name), "fixture").unwrap();
        }
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn text(values: &[OsString]) -> Vec<&str> {
    values.iter().map(|value| value.to_str().unwrap()).collect()
}

#[test]
fn controller_checks_preserve_the_python_gate_offline_flags_and_all_syntax_only_playbooks() {
    let fixture = Fixture::new(&["a.sh"]);
    let steps = steps(&fixture.0, Path::new("/owned tool/xtask")).unwrap();
    assert_eq!(steps.len(), if cfg!(target_os = "linux") { 23 } else { 22 });
    assert_eq!(steps[0].program, "/owned tool/xtask");
    assert_eq!(text(&steps[0].arguments), ["check", "--python"]);
    assert_eq!(
        text(&steps[1].arguments),
        ["--offline", "--strict", "ops/ansible"]
    );
    assert_eq!(
        text(&steps[2].arguments),
        ["--offline", "--strict", "security/vm/stage.yml"]
    );
    for step in &steps[3..13] {
        assert_eq!(step.program, "ansible-playbook");
        assert_eq!(
            text(&step.arguments)[..2],
            ["-i", "ops/ansible/inventory.example.yml"]
        );
        assert_eq!(step.arguments.last().unwrap(), "--syntax-check");
    }
    let playbooks = steps[3..13]
        .iter()
        .map(|step| step.arguments[2].to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        playbooks,
        [
            "ops/ansible/site.yml",
            "ops/ansible/public.yml",
            "ops/ansible/release.yml",
            "ops/ansible/host-hardening.yml",
            "ops/ansible/proxy-config.yml",
            "ops/ansible/reboot.yml",
            "ops/ansible/turn.yml",
            "ops/ansible/monitoring.yml",
            "ops/ansible/migration-prepare.yml",
            "ops/ansible/migration-data.yml"
        ]
    );
    assert_eq!(steps.last().unwrap().program, "python");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(steps[16].program, "cargo");
        assert_eq!(
            text(&steps[16].arguments),
            [
                "test",
                "--locked",
                "-p",
                "simplestchat-ops",
                "--lib",
                "capacity_vps::host_tests::whole_host_policy_matches_original_python_oracle",
                "--",
                "--ignored",
                "--exact"
            ]
        );
    }
    assert_eq!(steps[14].program, "cargo");
    assert_eq!(
        text(&steps[14].arguments),
        [
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            "--test",
            "release_build_cli",
            "--test",
            "release_attestation_cli",
            "--test",
            "release_verification_cli",
            "--test",
            "release_fresh_verify_cli",
            "--test",
            "release_fetch_cli",
            "--test",
            "archive_cli",
            "--test",
            "manifest_cli",
            "--",
            "--test-threads=1"
        ]
    );
    assert_eq!(steps[15].program, "cargo");
    assert_eq!(
        text(&steps[15].arguments),
        [
            "test",
            "--locked",
            "-p",
            "xtask",
            "--bin",
            "xtask",
            concat!(
                "ci::ops_artifact::capacity_vps::controller_tests::",
                "all_eight_whole_reference_scenarios_preserve_local_policy_",
                "and_explicit_transport_deltas",
            ),
            "--",
            "--ignored",
            "--exact"
        ]
    );
    let builder = &steps[steps.len() - 6];
    assert_eq!(builder.program, "cargo");
    assert_eq!(
        text(&builder.arguments),
        [
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            "release::build::tests::whole_reference_scenarios_match_unchanged_python",
            "--",
            "--ignored",
            "--exact",
        ]
    );
    let prepare = &steps[steps.len() - 5];
    assert_eq!(prepare.program, "cargo");
    assert_eq!(
        text(&prepare.arguments),
        [
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            concat!(
                "release::attestation::prepare::oracle_tests::",
                "whole_prepare_scenarios_match_unchanged_python",
            ),
            "--",
            "--ignored",
            "--exact",
        ]
    );
    let verify = &steps[steps.len() - 4];
    assert_eq!(verify.program, "cargo");
    assert_eq!(
        text(&verify.arguments),
        [
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            concat!(
                "release::attestation::verify::oracle_tests::",
                "whole_verify_scenarios_match_unchanged_python",
            ),
            "--",
            "--ignored",
            "--exact",
        ]
    );
    let fresh = &steps[steps.len() - 3];
    assert_eq!(fresh.program, "cargo");
    assert_eq!(
        text(&fresh.arguments),
        [
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            concat!(
                "release::attestation::acquire::oracle_tests::",
                "whole_fresh_release_matches_unchanged_python",
            ),
            "--",
            "--ignored",
            "--exact",
        ]
    );
    let fetch = &steps[steps.len() - 2];
    assert_eq!(fetch.program, "cargo");
    assert_eq!(
        text(&fetch.arguments),
        [
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            concat!(
                "release::attestation::fetch::oracle_tests::",
                "whole_fetch_controller_matches_unchanged_python",
            ),
            "--",
            "--ignored",
            "--exact",
        ]
    );
    assert_eq!(
        text(&steps.last().unwrap().arguments),
        [
            "-m",
            "unittest",
            "discover",
            "-s",
            "ops/ansible/tests",
            "-v"
        ]
    );
}

#[test]
fn shell_script_glob_preserves_sorted_literal_arguments_and_rejects_an_empty_inventory() {
    let fixture = Fixture::new(&["z.sh", "a script.sh", "private.env", "dollar$(command).sh"]);
    let steps = steps(&fixture.0, Path::new("/owned/xtask")).unwrap();
    let shellcheck = &steps[13];
    assert_eq!(shellcheck.program, "shellcheck");
    assert_eq!(
        text(&shellcheck.arguments),
        [
            "ops/ansible/files/a script.sh",
            "ops/ansible/files/dollar$(command).sh",
            "ops/ansible/files/z.sh"
        ]
    );
    let empty = Fixture::new(&["private.env"]);
    assert!(super::steps(&empty.0, Path::new("/owned/xtask")).is_err());
}

#[test]
fn a_failed_quality_or_syntax_check_preserves_status_and_cannot_run_later_steps() {
    let fixture = Fixture::new(&["a.sh"]);
    let steps = steps(&fixture.0, Path::new("/owned/xtask")).unwrap();
    for failing in 0..steps.len() {
        let mut called = 0;
        let result = execute(&steps, |_| {
            let index = called;
            called += 1;
            if index == failing {
                Err(Failure::new(37, "fixture failure"))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err().code, 37);
        assert_eq!(called, failing + 1);
    }
    assert_eq!(
        run(&fixture.0, &["unexpected".into()]).unwrap(),
        ExitCode::from(2)
    );
}
