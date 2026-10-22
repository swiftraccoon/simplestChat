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

struct Step {
    program: OsString,
    arguments: Vec<OsString>,
}

impl Step {
    fn new(program: impl Into<OsString>, arguments: &[&str]) -> Self {
        Self {
            program: program.into(),
            arguments: arguments.iter().map(OsString::from).collect(),
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
        program: "shellcheck".into(),
        arguments: scripts
            .into_iter()
            .map(|name| Path::new("ops/ansible/files").join(name).into_os_string())
            .collect(),
    });
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

fn checks(root: &Path, environment: &Environment) -> tooling::Result<()> {
    let executable = std::env::current_exe().map_err(|error| Failure::new(1, error.to_string()))?;
    let steps = steps(root, &executable).map_err(|error| Failure::new(1, error.to_string()))?;
    execute(&steps, |step| {
        tooling::run(root, environment, &step.program, &step.arguments)
    })
}

pub(super) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    if !arguments.is_empty() {
        eprintln!("usage: cargo run --locked -q -p xtask -- ci automation");
        return Ok(ExitCode::from(2));
    }
    match checks(root, &tooling::inherited_env()) {
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
