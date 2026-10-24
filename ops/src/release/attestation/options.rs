//! Explicit selectors and caller-admitted complete-operation budgets.

use super::control::{Cause, Result};
use serde::Serialize;
use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release attestation prepare --artifact-dir ABS ",
    "--security-dir ABS --repository OWNER/REPO --revision SHA --run-id N ",
    "--run-attempt N --timeout-seconds N --finalization-seconds M\n",
    "Prepare unsigned server metadata; 0 < M < N. No signing, Git, Docker or network."
);

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub(super) struct RunId(u64);
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub(super) struct Attempt(u32);

#[derive(Debug)]
pub(super) struct Selection {
    pub repository: String,
    pub revision: String,
    pub run: RunId,
    pub attempt: Attempt,
}
impl Selection {
    fn new(repository: String, revision: String, run: u64, attempt: u64) -> Result<Self> {
        let (owner, name) = repository.split_once('/').ok_or(Cause::Input)?;
        fn part(value: &str, max: usize, extra: &[u8]) -> bool {
            !value.is_empty()
                && value.len() <= max
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
        }
        if !part(owner, 39, b"-")
            || !part(name, 100, b"_.-")
            || revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || run == 0
            || run >= 1_u64 << 53
            || attempt == 0
            || attempt >= 1_u64 << 31
        {
            return Err(Cause::Input);
        }
        Ok(Self {
            repository,
            revision,
            run: RunId(run),
            attempt: Attempt(attempt.try_into().map_err(|_| Cause::Input)?),
        })
    }
    pub fn claim_selection(&self) -> Result<crate::release::trust::Selection> {
        crate::release::trust::Selection::new(
            &self.repository,
            &self.revision,
            self.run.0.try_into().map_err(|_| Cause::Input)?,
            self.run.0.try_into().map_err(|_| Cause::Input)?,
            i64::from(self.attempt.0),
        )
        .map_err(|_| Cause::Policy)
    }
}

pub(super) struct Options {
    pub artifact: PathBuf,
    pub security: PathBuf,
    pub selection: Selection,
    pub whole: Duration,
    pub reserve: Duration,
}
fn number(value: &str) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Cause::Input);
    }
    value.parse().map_err(|_| Cause::Input)
}
pub(super) fn canonical(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|p| matches!(p, Component::RootDir | Component::Normal(_)))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}
impl Options {
    pub fn parse(args: &[OsString]) -> Result<Option<Self>> {
        if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
            return Ok(None);
        }
        let mut fields = std::collections::BTreeMap::new();
        let mut at = 0;
        while at < args.len() {
            let name = args[at].to_str().ok_or(Cause::Input)?;
            let (name, value) = if let Some((name, value)) = name.split_once('=') {
                (name, OsString::from(value))
            } else {
                at += 1;
                (name, args.get(at).ok_or(Cause::Input)?.clone())
            };
            if ![
                "--artifact-dir",
                "--security-dir",
                "--repository",
                "--revision",
                "--run-id",
                "--run-attempt",
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
        if fields.len() != 8 {
            return Err(Cause::Input);
        }
        let take_text = |name: &str| fields[name].to_str().map(str::to_owned).ok_or(Cause::Input);
        let artifact = PathBuf::from(&fields["--artifact-dir"]);
        let security = PathBuf::from(&fields["--security-dir"]);
        if !canonical(&artifact) || !canonical(&security) {
            return Err(Cause::Input);
        }
        let selection = Selection::new(
            take_text("--repository")?,
            take_text("--revision")?,
            number(&take_text("--run-id")?)?,
            number(&take_text("--run-attempt")?)?,
        )?;
        let whole = number(&take_text("--timeout-seconds")?)?;
        let reserve = number(&take_text("--finalization-seconds")?)?;
        if reserve == 0 || reserve >= whole {
            return Err(Cause::Input);
        }
        Ok(Some(Self {
            artifact,
            security,
            selection,
            whole: Duration::from_secs(whole),
            reserve: Duration::from_secs(reserve),
        }))
    }
}
