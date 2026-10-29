//! Fixed explicit old/current signed selections for the manual deploy fixture.
use super::{
    Error, Result, files,
    options::{Options, Purpose, digest},
};
use std::{collections::BTreeMap, path::PathBuf};

pub(super) const SCENARIO: &str = "deploy-rollback";
pub(super) const USAGE: &str = "usage: xtask ops-artifact deploy-runtime --run-id N \
--run-attempt N --artifact-id N --server-artifact-id N --previous-revision SHA \
--previous-run-id N --previous-run-attempt N --previous-artifact-id N --output ABS";
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ServerSelection {
    pub revision: String,
    pub run: u64,
    pub attempt: u64,
    pub artifact: u64,
}
pub(super) struct DeployOptions {
    pub common: Options,
    pub current_server: u64,
    pub previous: ServerSelection,
}
fn number(value: &str) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Input);
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0 && *value < (1 << 53))
        .ok_or(Error::Input)
}
impl DeployOptions {
    pub fn parse(args: &[String]) -> Result<Option<Self>> {
        if args == ["--help"] {
            return Ok(None);
        }
        let revision = std::env::var("GITHUB_SHA").map_err(|_| Error::Input)?;
        Self::parse_for(args, &revision).map(Some)
    }
    fn parse_for(args: &[String], revision: &str) -> Result<Self> {
        const FLAGS: [&str; 9] = [
            "--run-id",
            "--run-attempt",
            "--artifact-id",
            "--server-artifact-id",
            "--previous-revision",
            "--previous-run-id",
            "--previous-run-attempt",
            "--previous-artifact-id",
            "--output",
        ];
        let (pairs, extra) = args.as_chunks::<2>();
        let mut values = BTreeMap::new();
        for pair in pairs {
            if !FLAGS.contains(&pair[0].as_str())
                || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
            {
                return Err(Error::Input);
            }
        }
        if !extra.is_empty() || values.len() != FLAGS.len() || !digest(revision, 40) {
            return Err(Error::Input);
        }
        let previous = values["--previous-revision"];
        let output = PathBuf::from(values["--output"]);
        if !digest(previous, 40) || previous == revision || !files::safe_path(&output) {
            return Err(Error::Input);
        }
        Ok(Self {
            common: Options {
                purpose: Purpose::Deploy,
                run: number(values["--run-id"])?,
                attempt: number(values["--run-attempt"])?,
                artifact: number(values["--artifact-id"])?,
                scenario: SCENARIO.into(),
                output,
                revision: revision.into(),
            },
            current_server: number(values["--server-artifact-id"])?,
            previous: ServerSelection {
                revision: previous.into(),
                run: number(values["--previous-run-id"])?,
                attempt: number(values["--previous-run-attempt"])?,
                artifact: number(values["--previous-artifact-id"])?,
            },
        })
    }
    pub fn previous_options(&self) -> Options {
        Options {
            purpose: Purpose::Deploy,
            run: self.previous.run,
            attempt: self.previous.attempt,
            artifact: self.common.artifact,
            scenario: SCENARIO.into(),
            output: self.common.output.clone(),
            revision: self.previous.revision.clone(),
        }
    }
}
#[cfg(test)]
#[path = "deploy_options_tests.rs"]
mod tests;
