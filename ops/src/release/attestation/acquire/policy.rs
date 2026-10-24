//! Fresh API relationships. JSON objects are data, never live acquisition proof.
use super::{
    super::{
        control::{self, Cause, Owner, Result},
        verify,
    },
    options::{SAFE, Selected},
};
use crate::json::Json;

fn require(owner: &mut Owner, accepted: bool) -> Result<()> {
    if accepted {
        Ok(())
    } else {
        control::failed(owner, Cause::Policy)
    }
}
fn positive(value: Option<&Json>) -> Option<u64> {
    value
        .and_then(Json::integer)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| (1..=SAFE).contains(value))
}
fn text<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Json::string)
}
pub(super) struct Artifact {
    pub run: u64,
    pub bytes: u64,
    pub digest: String,
}
pub(super) fn artifact(value: &Json, selected: &Selected, owner: &mut Owner) -> Result<Artifact> {
    require(
        owner,
        positive(value.get("id")) == Some(selected.artifact())
            && matches!(value.get("expired"), Some(Json::Bool(false)))
            && text(value, "name")
                == Some(format!("simplestchat-production-{}", selected.revision).as_str()),
    )?;
    let bytes = positive(value.get("size_in_bytes")).ok_or(Cause::Policy)?;
    let digest = text(value, "digest")
        .and_then(|value| value.strip_prefix("sha256:"))
        .ok_or(Cause::Policy)?;
    require(
        owner,
        bytes <= verify::zip::MAX_ZIP && super::options::digest(digest, 64),
    )?;
    let association = value.get("workflow_run").ok_or(Cause::Policy)?;
    let run = positive(association.get("id")).ok_or(Cause::Policy)?;
    let repository = positive(association.get("repository_id"));
    require(
        owner,
        text(association, "head_sha") == Some(selected.revision.as_str())
            && repository.is_some()
            && repository == positive(association.get("head_repository_id")),
    )?;
    Ok(Artifact {
        run,
        bytes,
        digest: digest.into(),
    })
}
pub(super) fn run(
    value: &Json,
    artifact: &Artifact,
    selected: &Selected,
    owner: &mut Owner,
) -> Result<verify::options::Metadata> {
    let attempt = positive(value.get("run_attempt")).ok_or(Cause::Policy)?;
    require(
        owner,
        artifact.run == selected.run() && text(value, "head_branch") == Some("main"),
    )?;
    let repository = |key| {
        value
            .get(key)
            .and_then(|value| text(value, "full_name"))
            .map(|value| value.to_lowercase())
    };
    require(
        owner,
        positive(value.get("id")) == Some(artifact.run)
            && text(value, "name") == Some("CI")
            && text(value, "path") == Some(".github/workflows/ci.yml")
            && text(value, "event") == Some("push")
            && text(value, "head_sha") == Some(selected.revision.as_str())
            && text(value, "status") == Some("completed")
            && text(value, "conclusion") == Some("success")
            && repository("repository") == Some(selected.repository.to_lowercase())
            && repository("head_repository") == Some(selected.repository.to_lowercase()),
    )?;
    let encoded = serde_json::to_vec(&serde_json::json!({
        "schemaVersion":1,"repository":selected.repository,"artifactId":selected.artifact(),
        "buildRunId":artifact.run,"ciRunId":selected.run(),"runAttempt":attempt,
        "revision":selected.revision,"artifactZipBytes":artifact.bytes,"zipSha256":artifact.digest
    }))
    .map_err(|_| Cause::Policy)?;
    verify::options::Metadata::decode(&encoded)
}
