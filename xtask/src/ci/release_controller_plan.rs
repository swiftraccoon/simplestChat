//! Fixed controller commands and origin declarations. No received command text.

use crate::{
    ci::runner,
    tooling::{self, Environment, Failure, Result},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) const PREFIX: &str = "release-container-controller";
pub(super) const LOG: &str = "release-container-controller.log";
pub(super) const STATUS: &str = "release-container-controller.status";
pub(super) const SPOOL: &str = "release-controller-control";
pub(super) const MAX_LOG: u64 = 64 * 1024 * 1024;
pub(super) const STARTUP: Duration = Duration::from_secs(10);
pub(super) const WORKER: Duration = Duration::from_secs(90 * 60);
pub(super) const WAIT: Duration = Duration::from_secs(300);
pub(super) const RESERVE: Duration = Duration::from_secs(20);
pub(super) const GRACE: Duration = Duration::from_secs(5);

pub(super) fn failure() -> Failure {
    Failure::new(1, "Release controller preparation failed.")
}
pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect()
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Origin {
    pub schema_version: u8,
    pub workspace_sha256: String,
    pub revision: String,
    pub run: String,
    pub attempt: String,
    pub requirements_sha256: String,
    pub requirements_stamp: super::source::Stamp,
}
pub(super) struct Context {
    pub root: PathBuf,
    pub temporary: PathBuf,
    pub environment: Environment,
    pub origin: Origin,
    requirements: std::sync::Mutex<Option<super::source::Source>>,
}
impl Context {
    pub fn new(root: &Path, environment: Environment, system: &str) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Release controller requires disposable Linux CI.",
            ));
        }
        let temporary = PathBuf::from(runner::text(&environment, "RUNNER_TEMP"));
        let revision = runner::text(&environment, "GITHUB_SHA");
        let local = runner::text(&environment, "ACT") == "true";
        let run = runner::text(
            &environment,
            if local {
                "LOCAL_CI_RUN_ID"
            } else {
                "GITHUB_RUN_ID"
            },
        );
        let attempt = if local {
            "1"
        } else {
            runner::text(&environment, "GITHUB_RUN_ATTEMPT")
        };
        let positive = |value: &str| {
            value.len() <= 20
                && value
                    .as_bytes()
                    .first()
                    .is_some_and(|v| (b'1'..=b'9').contains(v))
                && value.bytes().all(|v| v.is_ascii_digit())
        };
        if !root.is_absolute()
            || !temporary.is_absolute()
            || temporary.as_os_str().as_encoded_bytes().len() > 4096
            || revision.len() != 40
            || !revision.bytes().all(|v| v.is_ascii_hexdigit())
            || if local {
                run.len() != 32 || !run.bytes().all(|v| v.is_ascii_hexdigit())
            } else {
                !positive(run) || !positive(attempt)
            }
        {
            return Err(Failure::new(2, "Release controller context invalid."));
        }
        let requirements =
            super::source::Source::open(&root.join("ops/ansible/requirements.txt"), 1024 * 1024)?;
        let origin = Origin {
            schema_version: 1,
            workspace_sha256: digest(root.as_os_str().as_encoded_bytes()),
            revision: revision.into(),
            run: run.into(),
            attempt: attempt.into(),
            requirements_sha256: requirements.sha256.clone(),
            requirements_stamp: requirements.stamp.clone(),
        };
        Ok(Self {
            root: root.into(),
            temporary,
            environment,
            origin,
            requirements: std::sync::Mutex::new(Some(requirements)),
        })
    }
    pub fn validate(&self) -> Result<()> {
        self.requirements
            .lock()
            .map_err(|_| failure())?
            .as_ref()
            .ok_or_else(failure)?
            .validate()
    }
    pub fn finish(&mut self) -> Result<()> {
        self.close_requirements()
    }
    pub fn close_requirements(&self) -> Result<()> {
        let source = self.requirements.lock().map_err(|_| failure())?.take();
        source
            .map(super::source::Source::finish)
            .transpose()
            .map(|_| ())
    }
    pub fn arguments(&self, stage: Stage) -> (&'static str, Vec<OsString>) {
        let prefix = self.temporary.join(PREFIX);
        let (program, mut arguments): (&str, Vec<OsString>) = match stage {
            Stage::Update => ("sudo", vec!["apt-get".into(), "update".into()]),
            Stage::Install => (
                "sudo",
                [
                    "apt-get",
                    "install",
                    "-y",
                    "python3-venv",
                    "openssl",
                    "curl",
                    "ca-certificates",
                    "util-linux",
                ]
                .map(OsString::from)
                .into(),
            ),
            Stage::Venv => ("python3", vec!["-m".into(), "venv".into(), prefix.into()]),
            Stage::Pip => (
                "",
                vec![
                    "install".into(),
                    "-r".into(),
                    "ops/ansible/requirements.txt".into(),
                ],
            ),
        };
        if stage == Stage::Pip {
            arguments.insert(0, self.temporary.join(PREFIX).join("bin/pip").into());
        }
        (program, arguments)
    }
    pub fn command(&self, stage: Stage) -> std::process::Command {
        let (program, mut arguments) = self.arguments(stage);
        let program = if stage == Stage::Pip {
            arguments.remove(0)
        } else {
            program.into()
        };
        tooling::command(&self.root, &self.environment, program, arguments)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Stage {
    Update,
    Install,
    Venv,
    Pip,
}
pub(super) const STAGES: [Stage; 4] = [Stage::Update, Stage::Install, Stage::Venv, Stage::Pip];
