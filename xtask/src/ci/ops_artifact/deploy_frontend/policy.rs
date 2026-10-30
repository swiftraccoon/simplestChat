//! Pure selection of the one published main revision and one CI attempt.
//! Listings supply coordinates only. Fresh downstream signature verification
//! must bind both payloads to the selected attempt before any host action.
//! API review: C-VALIDATE/C-CUSTOM-TYPE keep checked identities distinct;
//! C-SERDE/C-GOOD-ERR reject selected-field coercions without reporting inputs.

use crate::tooling::{Failure, Result};
use serde::Deserialize;
use serde_json::value::RawValue;

pub(super) const REPOSITORY: &str = "swiftraccoon/simplestChat";
const REPOSITORY_ID: u64 = 1_155_881_649;
const OWNER_ID: u64 = 47_705_353;
const SAFE: u64 = 9_007_199_254_740_991;
const RESPONSE: usize = 4 * 1024 * 1024;
const RECORD: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Revision(String);
impl Revision {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RunId(u64);
impl RunId {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Attempt(u64);
impl Attempt {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArtifactId(u64);
impl ArtifactId {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub id: RunId,
    pub attempt: Attempt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Artifact {
    pub id: ArtifactId,
    pub zip_bytes: u64,
    pub zip_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArtifactKind {
    Application,
    Ops,
}
impl ArtifactKind {
    pub fn name(self, revision: &Revision) -> String {
        match self {
            Self::Application => format!("simplestchat-production-{}", revision.as_str()),
            Self::Ops => format!(
                "simplestchat-ops-{}-x86_64-unknown-linux-musl",
                revision.as_str()
            ),
        }
    }
    fn maximum(self) -> u64 {
        match self {
            Self::Application => 2 * 1024 * 1024 * 1024,
            Self::Ops => 300 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunProgress {
    Pending,
    Success,
}

fn refusal(code: &'static str) -> Failure {
    Failure::new(1, code)
}
fn digest(value: &str, bytes: usize) -> bool {
    value.len() == bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn positive(value: u64) -> bool {
    (1..=SAFE).contains(&value)
}
fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8], maximum: usize) -> Result<T> {
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(refusal("ci_response_invalid"));
    }
    // Typed API records reject duplicate used fields; unrelated API fields are
    // ignored and cannot authorize a different request or selected identity.
    serde_json::from_slice(bytes).map_err(|_| refusal("ci_response_invalid"))
}
fn line(value: &str) -> &str {
    value.trim_end_matches(['\r', '\n'])
}

/// Validate original command observations; no Git or network action occurs.
/// The caller must re-observe these identities before dispatching the playbook.
pub(super) fn admit_revision(
    head: &str,
    branch: &str,
    status: &str,
    remote: &str,
) -> Result<Revision> {
    let head = line(head);
    let remote = line(remote);
    let repository = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .into_iter()
    .find_map(|prefix| remote.strip_prefix(prefix))
    .map(|value| value.strip_suffix(".git").unwrap_or(value));
    if !digest(head, 40)
        || line(branch) != "main"
        || !status.is_empty()
        || repository.is_none_or(|value| !value.eq_ignore_ascii_case(REPOSITORY))
    {
        return Err(refusal("checkout_identity_invalid"));
    }
    Ok(Revision(head.into()))
}

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
    fn matches(&self) -> bool {
        self.id == REPOSITORY_ID
            && self.full_name == REPOSITORY
            && !self.fork
            && self.owner.id == OWNER_ID
            && self.owner.login == "swiftraccoon"
    }
}
#[derive(Deserialize)]
struct RunRecord {
    id: u64,
    run_attempt: u64,
    head_sha: String,
    head_branch: String,
    name: String,
    path: String,
    event: String,
    status: String,
    conclusion: Option<String>,
    repository: Repository,
    head_repository: Repository,
}
impl RunRecord {
    fn identity(&self, revision: &Revision) -> Result<Run> {
        if !positive(self.id)
            || !positive(self.run_attempt)
            || self.head_sha != revision.as_str()
            || self.head_branch != "main"
            || self.name != "CI"
            || self.path != ".github/workflows/ci.yml"
            || self.event != "push"
            || !self.repository.matches()
            || !self.head_repository.matches()
        {
            return Err(refusal("ci_identity_mismatch"));
        }
        Ok(Run {
            id: RunId(self.id),
            attempt: Attempt(self.run_attempt),
        })
    }
    fn progress(&self) -> Result<RunProgress> {
        if self.status == "completed" {
            return if self.conclusion.as_deref() == Some("success") {
                Ok(RunProgress::Success)
            } else {
                Err(refusal("ci_failed"))
            };
        }
        if ["queued", "in_progress", "pending", "waiting", "requested"]
            .contains(&self.status.as_str())
            && self.conclusion.is_none()
        {
            Ok(RunProgress::Pending)
        } else {
            Err(refusal("ci_status_invalid"))
        }
    }
}
#[derive(Deserialize)]
struct Runs {
    total_count: u64,
    workflow_runs: Vec<Box<RawValue>>,
}

/// Reject missing, ambiguous or truncated listings; never choose an older success.
pub(super) fn runs(bytes: &[u8], revision: &Revision) -> Result<Run> {
    let listing: Runs = decode(bytes, RESPONSE)?;
    if listing.total_count != 1 || listing.workflow_runs.len() != 1 {
        return Err(refusal("ci_missing_or_ambiguous"));
    }
    let record: RunRecord = decode(listing.workflow_runs[0].get().as_bytes(), RECORD)?;
    let selected = record.identity(revision)?;
    record.progress()?;
    Ok(selected)
}

/// Every poll belongs to the original run AND attempt, including pending polls.
pub(super) fn run(bytes: &[u8], revision: &Revision, selected: &Run) -> Result<RunProgress> {
    let record: RunRecord = decode(bytes, RECORD)?;
    if record.identity(revision)? != *selected {
        return Err(refusal("ci_identity_mismatch"));
    }
    record.progress()
}

#[derive(Deserialize)]
struct Association {
    id: u64,
    head_sha: String,
    repository_id: u64,
    head_repository_id: u64,
}
#[derive(Deserialize)]
struct ArtifactRecord {
    id: u64,
    name: String,
    url: String,
    archive_download_url: String,
    expired: bool,
    size_in_bytes: u64,
    digest: String,
    workflow_run: Association,
}
#[derive(Deserialize)]
struct Artifacts {
    total_count: u64,
    artifacts: Vec<Box<RawValue>>,
}

/// API metadata bounds candidate transport bytes, not the payload's attempt.
/// The existing application/Ops authenticators must prove that latter join.
pub(super) fn artifact(
    bytes: &[u8],
    revision: &Revision,
    selected: &Run,
    kind: ArtifactKind,
) -> Result<Artifact> {
    let listing: Artifacts = decode(bytes, RESPONSE)?;
    if listing.total_count != 1 || listing.artifacts.len() != 1 {
        return Err(refusal("ci_artifact_missing_or_ambiguous"));
    }
    let record: ArtifactRecord = decode(listing.artifacts[0].get().as_bytes(), RECORD)?;
    let url = format!(
        "https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{}",
        record.id
    );
    let sha = record.digest.strip_prefix("sha256:");
    if !positive(record.id)
        || record.expired
        || record.name != kind.name(revision)
        || record.url != url
        || record.archive_download_url != format!("{url}/zip")
        || record.size_in_bytes == 0
        || record.size_in_bytes > kind.maximum()
        || sha.is_none_or(|value| !digest(value, 64))
        || record.workflow_run.id != selected.id.get()
        || record.workflow_run.head_sha != revision.as_str()
        || record.workflow_run.repository_id != REPOSITORY_ID
        || record.workflow_run.head_repository_id != REPOSITORY_ID
    {
        return Err(refusal("ci_artifact_identity_invalid"));
    }
    Ok(Artifact {
        id: ArtifactId(record.id),
        zip_bytes: record.size_in_bytes,
        zip_sha256: sha.unwrap().into(),
    })
}

#[derive(Deserialize)]
struct GitObject {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}
#[derive(Deserialize)]
struct GitReference {
    #[serde(rename = "ref")]
    name: String,
    object: GitObject,
}

/// Join a fresh response to the fixed GitHub main-ref request owned by the caller.
pub(super) fn main(bytes: &[u8], revision: &Revision) -> Result<()> {
    let reference: GitReference = decode(bytes, RECORD)?;
    if reference.name != "refs/heads/main"
        || reference.object.kind != "commit"
        || reference.object.sha != revision.as_str()
    {
        return Err(refusal("published_main_mismatch"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
