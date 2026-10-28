//! Pure fixed-workflow selectors. Parsing elapsed time belongs to the entry clock.

use super::{Error, Result, files};
use std::{collections::BTreeMap, path::PathBuf};

pub(super) const REPOSITORY: &str = "swiftraccoon/simplestChat";
pub(super) const WORKFLOW: &str = "Signed musl maintenance launcher prerequisite KVM runtime";
pub(super) const HOST_WORKFLOW: &str = "Signed musl prepared-host maintenance KVM runtime";
pub(super) const HOST_CASES: [&str; 2] = ["host-success", "host-launcher-refusal"];
pub(super) const CASES: [&str; 4] = [
    "positive",
    "dirty-source",
    "cancel-owned-launcher",
    "timeout-stopped-launcher",
];
pub(super) const USAGE: &str = "usage: xtask ops-artifact maintain-runtime --run-id N \
--run-attempt N --artifact-id N --scenario CASE --output ABS";
pub(super) const HOST_USAGE: &str = "usage: xtask ops-artifact maintain-host-runtime --run-id N \
--run-attempt N --artifact-id N --scenario CASE --output ABS";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Purpose {
    Prerequisite,
    Host,
}
impl Purpose {
    pub fn workflow(self) -> &'static str {
        match self {
            Self::Prerequisite => WORKFLOW,
            Self::Host => HOST_WORKFLOW,
        }
    }
    pub fn cases(self) -> &'static [&'static str] {
        match self {
            Self::Prerequisite => &CASES,
            Self::Host => &HOST_CASES,
        }
    }
    pub fn usage(self) -> &'static str {
        match self {
            Self::Prerequisite => USAGE,
            Self::Host => HOST_USAGE,
        }
    }
    pub fn summary_scope(self) -> &'static str {
        match self {
            Self::Prerequisite => "SignedMuslMaintenanceLauncherPrerequisiteKvm",
            Self::Host => "SignedMuslPreparedHostMaintenanceKvm",
        }
    }
    pub fn proof_key(self) -> &'static str {
        match self {
            Self::Prerequisite => "maintainProof",
            Self::Host => "hostProof",
        }
    }
}
pub(super) struct Options {
    pub purpose: Purpose,
    pub run: u64,
    pub attempt: u64,
    pub artifact: u64,
    pub scenario: String,
    pub output: PathBuf,
    pub revision: String,
}
pub(super) fn digest(text: &str, count: usize) -> bool {
    text.len() == count
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn number(text: &str) -> Result<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Input);
    }
    text.parse::<u64>()
        .ok()
        .filter(|value| *value > 0 && *value < (1 << 53))
        .ok_or(Error::Input)
}
impl Options {
    pub fn parse(args: &[String]) -> Result<Option<Self>> {
        Self::parse_for(args, Purpose::Prerequisite)
    }
    pub fn parse_host(args: &[String]) -> Result<Option<Self>> {
        Self::parse_for(args, Purpose::Host)
    }
    fn parse_for(args: &[String], purpose: Purpose) -> Result<Option<Self>> {
        if args == ["--help"] {
            return Ok(None);
        }
        let mut values = BTreeMap::new();
        let (chunks, remainder) = args.as_chunks::<2>();
        for pair in chunks {
            if ![
                "--run-id",
                "--run-attempt",
                "--artifact-id",
                "--scenario",
                "--output",
            ]
            .contains(&pair[0].as_str())
                || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
            {
                return Err(Error::Input);
            }
        }
        if !remainder.is_empty() || values.len() != 5 {
            return Err(Error::Input);
        }
        let scenario = values["--scenario"];
        let output = PathBuf::from(values["--output"]);
        if !purpose.cases().contains(&scenario) || !files::safe_path(&output) {
            return Err(Error::Input);
        }
        let revision = std::env::var("GITHUB_SHA").map_err(|_| Error::Input)?;
        if !digest(&revision, 40) {
            return Err(Error::Input);
        }
        Ok(Some(Self {
            purpose,
            run: number(values["--run-id"])?,
            attempt: number(values["--run-attempt"])?,
            artifact: number(values["--artifact-id"])?,
            scenario: scenario.into(),
            output,
            revision,
        }))
    }
    pub fn guard(&self) -> Result<()> {
        let expected = [
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("GITHUB_REPOSITORY", REPOSITORY),
            ("GITHUB_REF", "refs/heads/main"),
            ("GITHUB_WORKFLOW", self.purpose.workflow()),
        ];
        if !cfg!(target_os = "linux")
            || expected
                .iter()
                .any(|(key, value)| std::env::var(key).as_deref() != Ok(*value))
        {
            return Err(Error::Refused);
        }
        if self.output.try_exists().map_err(|_| Error::Io)?
            || std::fs::symlink_metadata(&self.output).is_ok()
            || self.output.parent().is_none_or(|parent| {
                !std::fs::canonicalize(parent).is_ok_and(|canonical| canonical == parent)
            })
        {
            return Err(Error::Refused);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_selector_domains_do_not_accept_aliases_or_public_paths() {
        assert!(number("0").is_err());
        assert!(number("9007199254740992").is_err());
        assert!(number("+1").is_err());
        assert_eq!(number("1"), Ok(1));
        assert!(!digest(&"A".repeat(40), 40));
        assert!(Options::parse(&["--timeout".into(), "4200".into()]).is_err());
        assert!(!files::safe_path(std::path::Path::new("/tmp/../foreign")));
    }
    #[test]
    fn host_workflow_and_cases_are_a_separate_closed_purpose() {
        assert_eq!(Purpose::Prerequisite.cases(), &CASES);
        assert_eq!(Purpose::Host.cases(), &HOST_CASES);
        for case in CASES {
            assert!(!Purpose::Host.cases().contains(&case));
        }
        for case in HOST_CASES {
            assert!(!Purpose::Prerequisite.cases().contains(&case));
        }
        assert_ne!(Purpose::Host.workflow(), Purpose::Prerequisite.workflow());
        assert_ne!(
            Purpose::Host.summary_scope(),
            Purpose::Prerequisite.summary_scope()
        );
        assert_ne!(Purpose::Host.proof_key(), Purpose::Prerequisite.proof_key());
    }
}
