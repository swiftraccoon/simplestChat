//! Fresh fixed API identities bind transport bytes, independently of signing.

use super::{failure, request::Request};
use crate::tooling::Result;
use serde::{Deserialize, Serialize};

const REPO: &str = "swiftraccoon/simplestChat";
const REPO_ID: u64 = 1_155_881_649;
const OWNER_ID: u64 = 47_705_353;
const PREFIX: &str = "https://api.github.com/repos/swiftraccoon/simplestChat";

#[derive(Deserialize)]
struct Owner {
    id: u64,
    login: String,
}
#[derive(Deserialize)]
struct Repository {
    id: u64,
    full_name: String,
    fork: bool,
    owner: Owner,
}
impl Repository {
    fn check(&self) -> Result<()> {
        if self.id != REPO_ID
            || self.full_name != REPO
            || self.fork
            || self.owner.id != OWNER_ID
            || self.owner.login != "swiftraccoon"
        {
            Err(failure(1))
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
struct Attempt {
    id: u64,
    run_attempt: u64,
    head_sha: String,
    head_branch: String,
    name: String,
    path: String,
    event: String,
    status: String,
    conclusion: String,
    repository: Repository,
    head_repository: Repository,
}
#[derive(Deserialize)]
struct Association {
    id: u64,
    head_sha: String,
    repository_id: u64,
    head_repository_id: u64,
}
#[derive(Deserialize)]
struct Artifact {
    id: u64,
    name: String,
    url: String,
    archive_download_url: String,
    expired: bool,
    size_in_bytes: u64,
    digest: String,
    workflow_run: Association,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Envelope {
    pub artifact_id: u64,
    pub zip_sha256: String,
    pub zip_bytes: u64,
}
fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    if bytes.is_empty() || bytes.len() as u64 > super::SMALL {
        return Err(failure(1));
    }
    // Typed selected fields reject duplicate selected keys and type coercions;
    // unrelated API fields are ignored and cannot grant authority.
    serde_json::from_slice(bytes).map_err(|_| failure(1))
}
pub(super) fn admit(
    repository: &[u8],
    attempt: &[u8],
    artifact: &[u8],
    request: &Request,
) -> Result<Envelope> {
    let repository: Repository = decode(repository)?;
    repository.check()?;
    let attempt: Attempt = decode(attempt)?;
    attempt.repository.check()?;
    attempt.head_repository.check()?;
    let selection = serde_json::to_value(request.selection.identity()).map_err(|_| failure(1))?;
    let revision = selection["revision"].as_str().ok_or_else(|| failure(1))?;
    let run = selection["buildRunId"].as_u64().ok_or_else(|| failure(1))?;
    let run_attempt = selection["runAttempt"].as_u64().ok_or_else(|| failure(1))?;
    if attempt.id != run
        || attempt.run_attempt != run_attempt
        || attempt.head_sha != revision
        || attempt.head_branch != "main"
        || attempt.name != "CI"
        || ![
            ".github/workflows/ci.yml",
            ".github/workflows/ci.yml@main",
            ".github/workflows/ci.yml@refs/heads/main",
        ]
        .contains(&attempt.path.as_str())
        || attempt.event != "push"
        || attempt.status != "completed"
        || attempt.conclusion != "success"
    {
        return Err(failure(1));
    }
    let artifact: Artifact = decode(artifact)?;
    let expected_url = format!("{PREFIX}/actions/artifacts/{}", request.artifact_id);
    if artifact.id != request.artifact_id
        || artifact.expired
        || artifact.name != format!("simplestchat-ops-{revision}-x86_64-unknown-linux-musl")
        || artifact.url != expected_url
        || artifact.archive_download_url != format!("{expected_url}/zip")
        || artifact.size_in_bytes == 0
        || artifact.size_in_bytes > super::ZIP
        || artifact.workflow_run.id != run
        || artifact.workflow_run.head_sha != revision
        || artifact.workflow_run.repository_id != REPO_ID
        || artifact.workflow_run.head_repository_id != REPO_ID
    {
        return Err(failure(1));
    }
    let sha = artifact
        .digest
        .strip_prefix("sha256:")
        .ok_or_else(|| failure(1))?;
    if !super::super::digest(sha) {
        return Err(failure(1));
    }
    Ok(Envelope {
        artifact_id: artifact.id,
        zip_sha256: sha.into(),
        zip_bytes: artifact.size_in_bytes,
    })
}
