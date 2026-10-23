//! Fixed hosted identity. Candidate files and caller-selected requests cannot
//! select a repository, workflow, ref, event, target or signer authority.

use super::failure;
use crate::tooling::{Environment, Result};
use serde::Serialize;
use simplestchat_ops::distribution::manifest::{Selection, verification};
use std::path::{Path, PathBuf};

pub(super) const TARGET: &str = "x86_64-unknown-linux-musl";
pub(super) const HOST: &str = "x86_64-unknown-linux-gnu";
pub(super) const VERSION: &str = "1.99.0";

#[derive(Clone, Copy)]
pub(super) enum ArtifactKind {
    Unsigned,
    Signed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Context {
    pub repository: &'static str,
    pub revision: String,
    pub package_version: &'static str,
    pub protocol_version: u8,
    pub target: &'static str,
    pub build_run_id: u64,
    pub run_attempt: u64,
    #[serde(skip)]
    pub workspace: PathBuf,
}

fn number(value: &str) -> Result<u64> {
    if value.is_empty()
        || value.len() > 16
        || value.starts_with('0')
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(failure(1));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= verification::MAX_SAFE_INTEGER)
        .ok_or_else(|| failure(1))
}

impl Context {
    pub fn capture(environment: &Environment) -> Result<Self> {
        let get = |name: &str| {
            environment
                .get(std::ffi::OsStr::new(name))
                .and_then(|s| s.to_str())
                .ok_or_else(|| failure(1))
        };
        if get("GITHUB_ACTIONS")? != "true"
            || get("RUNNER_OS")? != "Linux"
            || get("RUNNER_ARCH")? != "X64"
            || get("GITHUB_REPOSITORY")? != verification::REPOSITORY
            || get("GITHUB_REPOSITORY_ID")? != verification::REPOSITORY_ID.to_string()
            || get("GITHUB_REPOSITORY_OWNER_ID")? != verification::OWNER_ID.to_string()
            || get("GITHUB_REF")? != verification::SOURCE_REF
            || get("GITHUB_EVENT_NAME")? != "push"
            || get("GITHUB_WORKFLOW")? != "CI"
            || get("RUNNER_ENVIRONMENT")? != "github-hosted"
            || get("GITHUB_SERVER_URL")? != "https://github.com"
            || get("GITHUB_API_URL")? != "https://api.github.com"
        {
            return Err(failure(1));
        }
        let revision = get("GITHUB_SHA")?;
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failure(1));
        }
        let workflow_ref = format!(
            "{}/{}@{}",
            verification::REPOSITORY,
            verification::WORKFLOW,
            verification::SOURCE_REF
        );
        if get("GITHUB_WORKFLOW_REF")? != workflow_ref {
            return Err(failure(1));
        }
        if get("GITHUB_WORKFLOW_SHA")? != revision {
            return Err(failure(1));
        }
        let workspace = Path::new(get("GITHUB_WORKSPACE")?);
        if !super::super::namespace::path(workspace) {
            return Err(failure(1));
        }
        Ok(Self {
            repository: verification::REPOSITORY,
            revision: revision.to_owned(),
            package_version: env!("CARGO_PKG_VERSION"),
            protocol_version: 1,
            target: TARGET,
            build_run_id: number(get("GITHUB_RUN_ID")?)?,
            run_attempt: number(get("GITHUB_RUN_ATTEMPT")?)?,
            workspace: workspace.to_path_buf(),
        })
    }

    pub fn selection(&self) -> Result<Selection> {
        let bytes = serde_json::to_vec(self).map_err(|_| failure(1))?;
        Selection::decode(&bytes).map_err(|_| failure(1))
    }

    pub fn artifact_name(&self, kind: ArtifactKind) -> String {
        format!(
            "simplestchat-ops-{}{}-{}",
            match kind {
                ArtifactKind::Signed => "",
                ArtifactKind::Unsigned => "unsigned-",
            },
            self.revision,
            TARGET
        )
    }
}
