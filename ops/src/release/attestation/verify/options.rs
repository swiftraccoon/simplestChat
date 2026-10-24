//! A passive nine-field selection and one explicit original caller budget.

use super::super::{
    control::{Cause, Result},
    files,
};
use crate::{json::Json, release::trust};
use serde::Serialize;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release attestation verify --directory ABS ",
    "--selection-json JSON --timeout-seconds N --finalization-seconds M\n",
    "Verify cached server bytes; 0 < M < N. Selection data does not prove fresh API identity."
);
const MAX_ID: i64 = (1_i64 << 53) - 1;

#[derive(Debug, Serialize)]
#[serde(transparent)]
struct ArtifactId(i64);
#[derive(Debug, Serialize)]
#[serde(transparent)]
struct RunId(i64);
#[derive(Debug, Serialize)]
#[serde(transparent)]
struct Attempt(i64);
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Metadata {
    schema_version: u8,
    repository: String,
    artifact_id: ArtifactId,
    build_run_id: RunId,
    ci_run_id: RunId,
    run_attempt: Attempt,
    revision: String,
    artifact_zip_bytes: u64,
    zip_sha256: String,
}
fn integer(value: &Json, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Json::integer)
        .filter(|value| (1..=MAX_ID).contains(value))
        .ok_or(Cause::Input)
}
fn text(value: &Json, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Json::string)
        .map(str::to_owned)
        .ok_or(Cause::Input)
}
pub(super) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl Metadata {
    fn decode(bytes: &[u8]) -> Result<Self> {
        let value =
            crate::json::decode_limited(bytes, trust::MAX_LINE).map_err(|_| Cause::Input)?;
        let object = value.object().ok_or(Cause::Input)?;
        let keys = [
            "schemaVersion",
            "repository",
            "artifactId",
            "buildRunId",
            "ciRunId",
            "runAttempt",
            "revision",
            "artifactZipBytes",
            "zipSha256",
        ];
        if object.len() != keys.len()
            || !keys.iter().all(|key| object.contains_key(*key))
            || value.get("schemaVersion").and_then(Json::integer) != Some(1)
        {
            return Err(Cause::Input);
        }
        let result = Self {
            schema_version: 1,
            repository: text(&value, "repository")?,
            artifact_id: ArtifactId(integer(&value, "artifactId")?),
            build_run_id: RunId(integer(&value, "buildRunId")?),
            ci_run_id: RunId(integer(&value, "ciRunId")?),
            run_attempt: Attempt(integer(&value, "runAttempt")?),
            revision: text(&value, "revision")?,
            artifact_zip_bytes: integer(&value, "artifactZipBytes")?
                .try_into()
                .map_err(|_| Cause::Input)?,
            zip_sha256: text(&value, "zipSha256")?,
        };
        if result.build_run_id.0 != result.ci_run_id.0
            || result.artifact_zip_bytes > super::zip::MAX_ZIP
            || !hex(&result.zip_sha256, 64)
            || result.selection().is_err()
        {
            return Err(Cause::Input);
        }
        Ok(result)
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn run(&self) -> i64 {
        self.ci_run_id.0
    }
    pub fn attempt(&self) -> i64 {
        self.run_attempt.0
    }
    pub fn zip_bytes(&self) -> u64 {
        self.artifact_zip_bytes
    }
    pub fn zip_sha256(&self) -> &str {
        &self.zip_sha256
    }
    pub fn selection(&self) -> Result<trust::Selection> {
        trust::Selection::new(
            &self.repository,
            &self.revision,
            self.build_run_id.0,
            self.ci_run_id.0,
            self.run_attempt.0,
        )
        .map_err(|_| Cause::Input)
    }
}
pub(super) struct Options {
    pub directory: PathBuf,
    pub metadata: Metadata,
    pub whole: Duration,
    pub reserve: Duration,
    #[cfg(test)]
    pub environment: Option<BTreeMap<OsString, OsString>>,
}
fn number(value: &str) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Cause::Input);
    }
    value.parse().map_err(|_| Cause::Input)
}
impl Options {
    pub fn parse(arguments: &[OsString]) -> Result<Option<Self>> {
        if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
            return Ok(None);
        }
        let mut fields = BTreeMap::new();
        let mut at = 0;
        while at < arguments.len() {
            let name = arguments[at].to_str().ok_or(Cause::Input)?;
            let (name, value) = if let Some((name, value)) = name.split_once('=') {
                (name, OsString::from(value))
            } else {
                at += 1;
                (name, arguments.get(at).ok_or(Cause::Input)?.clone())
            };
            if ![
                "--directory",
                "--selection-json",
                "--timeout-seconds",
                "--finalization-seconds",
            ]
            .contains(&name)
                || value.is_empty()
                || fields.insert(name, value).is_some()
            {
                return Err(Cause::Input);
            }
            at += 1;
        }
        if fields.len() != 4 {
            return Err(Cause::Input);
        }
        let directory = PathBuf::from(&fields["--directory"]);
        if !files::canonical(&directory) {
            return Err(Cause::Input);
        }
        let selected = fields["--selection-json"].to_str().ok_or(Cause::Input)?;
        if selected.len() > trust::MAX_LINE {
            return Err(Cause::Input);
        }
        let metadata = Metadata::decode(selected.as_bytes())?;
        let whole = number(fields["--timeout-seconds"].to_str().ok_or(Cause::Input)?)?;
        let reserve = number(
            fields["--finalization-seconds"]
                .to_str()
                .ok_or(Cause::Input)?,
        )?;
        if reserve == 0 || reserve >= whole {
            return Err(Cause::Input);
        }
        Ok(Some(Self {
            directory,
            metadata,
            whole: Duration::from_secs(whole),
            reserve: Duration::from_secs(reserve),
            #[cfg(test)]
            environment: None,
        }))
    }
}
