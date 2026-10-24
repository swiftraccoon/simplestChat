//! Explicit fresh source selectors, never a deserialized authentication receipt.

use super::super::{
    control::{Cause, Result},
    files,
};
use serde::Serialize;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release verify --repository REPO --revision SHA ",
    "--artifact-id N --ci-run N --output-parent ABS [--artifact-dir ABS] ",
    "--timeout-seconds N --finalization-seconds M\n",
    "Fresh API identity and signed local bytes; 0 < M < N. No SSH or deployment-host operation."
);
pub(super) const SAFE: u64 = (1_u64 << 53) - 1;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(transparent)]
pub(super) struct ArtifactId(u64);
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(transparent)]
pub(super) struct RunId(u64);
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Selected {
    pub repository: String,
    pub revision: String,
    pub artifact_id: ArtifactId,
    pub ci_run: RunId,
}
impl Selected {
    pub fn artifact(&self) -> u64 {
        self.artifact_id.0
    }
    pub fn run(&self) -> u64 {
        self.ci_run.0
    }
}
pub(super) struct Options {
    pub selected: Selected,
    pub output_parent: PathBuf,
    pub artifact: Option<PathBuf>,
    pub whole: Duration,
    pub reserve: Duration,
    #[cfg(test)]
    pub environment: Option<BTreeMap<OsString, OsString>>,
    #[cfg(test)]
    pub free_bytes: Option<u64>,
    #[cfg(test)]
    pub expire_after: Option<u64>,
    #[cfg(test)]
    pub output_spelling: Option<PathBuf>,
}
pub(super) fn number(value: &str) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Cause::Input);
    }
    value.parse().map_err(|_| Cause::Input)
}
fn identity(value: &str) -> Result<u64> {
    number(value).and_then(|value| {
        (1..=SAFE)
            .contains(&value)
            .then_some(value)
            .ok_or(Cause::Input)
    })
}
pub(super) fn repository(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    let part = |value: &str, maximum: usize, repository: bool| {
        !value.is_empty()
            && value.len() <= maximum
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || byte == b'-'
                    || (repository && matches!(byte, b'_' | b'.'))
            })
    };
    part(owner, 39, false) && part(name, 100, true)
}
pub(super) fn digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
                "--repository",
                "--revision",
                "--artifact-id",
                "--ci-run",
                "--output-parent",
                "--artifact-dir",
                "--timeout-seconds",
                "--finalization-seconds",
            ]
            .contains(&name)
                || fields.insert(name, value).is_some()
            {
                return Err(Cause::Input);
            }
            at += 1;
        }
        let text = |name| -> Result<&str> {
            fields
                .get(name)
                .and_then(|value| value.to_str())
                .ok_or(Cause::Input)
        };
        let selected = Selected {
            repository: text("--repository")?.into(),
            revision: text("--revision")?.into(),
            artifact_id: ArtifactId(identity(text("--artifact-id")?)?),
            ci_run: RunId(identity(text("--ci-run")?)?),
        };
        if !repository(&selected.repository) || !digest(&selected.revision, 40) {
            return Err(Cause::Input);
        }
        let output_parent = PathBuf::from(fields.get("--output-parent").ok_or(Cause::Input)?);
        let artifact = fields
            .get("--artifact-dir")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        if artifact
            .as_ref()
            .is_some_and(|path| !files::canonical(path))
        {
            return Err(Cause::Input);
        }
        let whole = number(text("--timeout-seconds")?)?;
        let reserve = number(text("--finalization-seconds")?)?;
        if reserve == 0 || reserve >= whole {
            return Err(Cause::Input);
        }
        Ok(Some(Self {
            selected,
            output_parent,
            artifact,
            whole: Duration::from_secs(whole),
            reserve: Duration::from_secs(reserve),
            #[cfg(test)]
            environment: None,
            #[cfg(test)]
            free_bytes: None,
            #[cfg(test)]
            expire_after: None,
            #[cfg(test)]
            output_spelling: None,
        }))
    }
}
