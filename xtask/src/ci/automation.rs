//! The workflow's installed controller checks, in the original failure order.

use crate::tooling::{self, Environment, Failure};
use anyhow::Result;
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

const PLAYBOOKS: [&str; 10] = [
    "site.yml",
    "public.yml",
    "release.yml",
    "host-hardening.yml",
    "proxy-config.yml",
    "reboot.yml",
    "turn.yml",
    "monitoring.yml",
    "migration-prepare.yml",
    "migration-data.yml",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferenceGroup {
    Foundation,
    Fetch,
    Host,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Checks,
    References(ReferenceGroup),
}

#[derive(Clone, Copy)]
enum Selection {
    All,
    Checks,
    References,
    Reference(ReferenceGroup),
}

impl Selection {
    fn includes(self, group: Group) -> bool {
        match self {
            Self::All => true,
            Self::Checks => group == Group::Checks,
            Self::References => matches!(group, Group::References(_)),
            Self::Reference(selected) => group == Group::References(selected),
        }
    }
}

struct Step {
    group: Group,
    program: OsString,
    arguments: Vec<OsString>,
}

impl Step {
    fn new(program: impl Into<OsString>, arguments: &[&str]) -> Self {
        Self {
            group: Group::Checks,
            program: program.into(),
            arguments: arguments.iter().map(OsString::from).collect(),
        }
    }

    fn reference(group: ReferenceGroup, program: impl Into<OsString>, arguments: &[&str]) -> Self {
        Self {
            group: Group::References(group),
            ..Self::new(program, arguments)
        }
    }
}

/// Resolve the shell glob to argument boundaries before launching any checks.
/// No script name, playbook or command is evaluated as shell text.
fn steps(root: &Path, executable: &Path) -> std::io::Result<Vec<Step>> {
    let mut steps = vec![
        Step::new(executable.as_os_str(), &["check", "--python"]),
        Step::new("ansible-lint", &["--offline", "--strict", "ops/ansible"]),
        Step::new(
            "ansible-lint",
            &["--offline", "--strict", "security/vm/stage.yml"],
        ),
    ];
    for name in PLAYBOOKS {
        steps.push(Step::new(
            "ansible-playbook",
            &[
                "-i",
                "ops/ansible/inventory.example.yml",
                &format!("ops/ansible/{name}"),
                "--syntax-check",
            ],
        ));
    }
    let mut scripts = std::fs::read_dir(root.join("ops/ansible/files"))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    scripts.retain(|name| {
        Path::new(name)
            .extension()
            .is_some_and(|extension| extension == "sh")
    });
    scripts.sort();
    if scripts.is_empty() {
        return Err(std::io::Error::other(
            "No controller shell scripts were found.",
        ));
    }
    steps.push(Step {
        group: Group::Checks,
        program: "shellcheck".into(),
        arguments: scripts
            .into_iter()
            .map(|name| Path::new("ops/ansible/files").join(name).into_os_string())
            .collect(),
    });
    // Exercise the distributed Ops dependency profile before the server build.
    // Its ignored helpers and Python comparisons have separate owning callers.
    steps.push(Step::new(
        "cargo",
        &[
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
            "release_receive_cli",
            "--test",
            "release_stage_cli",
            "--test",
            "archive_cli",
            "--test",
            "manifest_cli",
            "--",
            "--test-threads=1",
        ],
    ));
    // The unchanged Python reference needs the installed controller environment.
    // Keep this explicit gate in both hosted automation and its local-CI route.
    steps.push(Step::reference(
        ReferenceGroup::Foundation,
        "cargo",
        &[
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
            "--exact",
        ],
    ));
    #[cfg(target_os = "linux")]
    steps.push(Step::reference(
        ReferenceGroup::Foundation,
        "cargo",
        &[
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            "capacity_vps::host_tests::whole_host_policy_matches_original_python_oracle",
            "--",
            "--ignored",
            "--exact",
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Foundation,
        "cargo",
        &[
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            "release::build::tests::whole_reference_scenarios_match_unchanged_python",
            "--",
            "--ignored",
            "--exact",
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Foundation,
        "cargo",
        &[
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
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Foundation,
        "cargo",
        &[
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
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Fetch,
        "cargo",
        &[
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
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Fetch,
        "cargo",
        &[
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
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Host,
        "cargo",
        &[
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            concat!(
                "release::attestation::receive::oracle_tests::",
                "whole_host_receiver_matches_unchanged_python",
            ),
            "--",
            "--ignored",
            "--exact",
        ],
    ));
    steps.push(Step::reference(
        ReferenceGroup::Host,
        "cargo",
        &[
            "test",
            "--locked",
            "-p",
            "simplestchat-ops",
            "--lib",
            concat!(
                "release::attestation::stage::oracle_tests::",
                "whole_stage_matches_unchanged_python",
            ),
            "--",
            "--ignored",
            "--exact",
        ],
    ));
    steps.push(Step::new(
        "python",
        &[
            "-m",
            "unittest",
            "discover",
            "-s",
            "ops/ansible/tests",
            "-v",
        ],
    ));
    Ok(steps)
}

fn execute(
    steps: &[Step],
    mut child: impl FnMut(&Step) -> tooling::Result<()>,
) -> tooling::Result<()> {
    for step in steps {
        child(step)?;
    }
    Ok(())
}

fn selected(steps: Vec<Step>, selection: Selection) -> Vec<Step> {
    steps
        .into_iter()
        .filter(|step| selection.includes(step.group))
        .collect()
}

fn checks(root: &Path, environment: &Environment, selection: Selection) -> tooling::Result<()> {
    #[cfg(not(target_os = "linux"))]
    eprintln!("The VPS host oracle runs in Linux automation; use ci-local for that gate.");
    let executable = std::env::current_exe().map_err(|error| Failure::new(1, error.to_string()))?;
    let steps = selected(
        steps(root, &executable).map_err(|error| Failure::new(1, error.to_string()))?,
        selection,
    );
    execute(&steps, |step| {
        tooling::run(root, environment, &step.program, &step.arguments)
    })
}

pub(super) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    let selection = match arguments {
        [] => Selection::All,
        [flag] if flag == "--checks" => Selection::Checks,
        [flag] if flag == "--references" => Selection::References,
        [flag, group] if flag == "--references" && group == "foundation" => {
            Selection::Reference(ReferenceGroup::Foundation)
        }
        [flag, group] if flag == "--references" && group == "fetch" => {
            Selection::Reference(ReferenceGroup::Fetch)
        }
        [flag, group] if flag == "--references" && group == "host" => {
            Selection::Reference(ReferenceGroup::Host)
        }
        _ => {
            eprintln!(concat!(
                "usage: cargo run --locked -q -p xtask -- ci automation ",
                "[--checks|--references [foundation|fetch|host]]",
            ));
            return Ok(ExitCode::from(2));
        }
    };
    match checks(root, &tooling::inherited_env(), selection) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(failure) => {
            eprintln!("{}", failure.message);
            Ok(ExitCode::from(failure.code))
        }
    }
}

#[cfg(test)]
#[path = "automation_tests.rs"]
mod tests;
