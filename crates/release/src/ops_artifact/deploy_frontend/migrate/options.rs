//! Pure argument admission for a migration between two explicitly selected
//! inventories. Paths select data and tools; nothing is opened here, and
//! parsing starts no I/O, process or network work.

use super::super::options::{alias, number, origin, path};
use super::super::{failure, policy::REPOSITORY};
use simplestchat_toolkit::tooling::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub(in super::super) const USAGE: &str = concat!(
    "usage: simplestchat-release migrate --source-inventory ABS --source-origin https://HOST ",
    "--inventory ABS --origin https://HOST --repository swiftraccoon/simplestChat [OPTIONS]\n\n",
    "  --source-limit HOST             exact source inventory host alias\n",
    "  --limit HOST                    exact destination inventory host alias\n",
    "  --wait-seconds N                CI wait, 0..7200; default 3600\n",
    "  --revision SHA                  exact ancestor application revision; default HEAD\n",
    "  --dns-wait-seconds N            kept-hostname DNS cutover wait, 0..1800; default 600\n",
    "  --ansible-playbook ABS          caller-trusted Ansible executable\n",
    "  --gh ABS                        caller-trusted GitHub CLI executable\n",
    "  --activate-inventory ABS        replace this private source inventory after cutover\n",
    "  --update-canary                 move the existing CANARY_ORIGIN after cutover\n",
    "  --application-artifact-dir ABS  original application ZIP and ten members\n",
    "  --ops-artifact-zip ABS          original Ops ZIP candidate bytes\n\n",
    "Build this controller before supplying credentials. The source stays live until ",
    "the destination holds a verified copy of its data, and is never restarted once ",
    "destination startup was attempted.\n",
);

const VALUE_FLAGS: [&str; 14] = [
    "--source-inventory",
    "--source-origin",
    "--inventory",
    "--origin",
    "--repository",
    "--source-limit",
    "--limit",
    "--wait-seconds",
    "--revision",
    "--dns-wait-seconds",
    "--ansible-playbook",
    "--gh",
    "--activate-inventory",
    "--application-artifact-dir",
];
const LAST_VALUE_FLAG: &str = "--ops-artifact-zip";
const UPDATE_CANARY: &str = "--update-canary";

/// One of the two hosts: its inventory, the origin it serves and an exact
/// host alias when the inventory has more than one.
pub(in super::super) struct Side {
    pub inventory: PathBuf,
    pub origin: String,
    pub origin_host: String,
    pub limit: Option<String>,
}

pub(in super::super) struct Options {
    pub source: Side,
    pub target: Side,
    pub repository: String,
    pub wait_seconds: u64,
    /// The application revision to move, an ancestor of the checkout.
    pub revision: Option<String>,
    pub dns_wait_seconds: u64,
    pub ansible_playbook: Option<PathBuf>,
    pub gh: Option<PathBuf>,
    pub activate_inventory: Option<PathBuf>,
    pub update_canary: bool,
    pub application_artifact_dir: Option<PathBuf>,
    pub ops_artifact_zip: Option<PathBuf>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SignedMigrationOptions")
    }
}

impl Options {
    /// Whether the public hostname stays and only its addresses change.
    pub(in super::super) fn keeps_hostname(&self) -> bool {
        self.source.origin == self.target.origin
    }
}

fn revision(value: &str) -> Result<String> {
    let exact = value.len() == 40
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if exact {
        Ok(value.to_owned())
    } else {
        Err(failure(2))
    }
}

pub(in super::super) fn parse(arguments: &[String]) -> Result<Option<Options>> {
    if arguments == ["--help"] {
        return Ok(None);
    }
    let mut seen = BTreeSet::new();
    let mut values = BTreeMap::new();
    let mut remaining = arguments.iter();
    while let Some(flag) = remaining.next() {
        let flag = flag.as_str();
        if !seen.insert(flag) {
            return Err(failure(2));
        }
        if flag == UPDATE_CANARY {
            continue;
        }
        if !VALUE_FLAGS.contains(&flag) && flag != LAST_VALUE_FLAG {
            return Err(failure(2));
        }
        let value = remaining.next().ok_or_else(|| failure(2))?;
        values.insert(flag, value.as_str());
    }
    let required = |flag| values.get(flag).copied().ok_or_else(|| failure(2));
    let optional_path = |flag| values.get(flag).map(|value| path(value)).transpose();
    let repository = required("--repository")?;
    if repository != REPOSITORY {
        return Err(failure(2));
    }
    let side = |inventory, selected, limit| -> Result<Side> {
        let limit = values.get(limit).copied();
        if limit.is_some_and(|limit| !alias(limit)) {
            return Err(failure(2));
        }
        let (origin, origin_host) = origin(required(selected)?)?;
        Ok(Side {
            inventory: path(required(inventory)?)?,
            origin,
            origin_host,
            limit: limit.map(str::to_owned),
        })
    };
    Ok(Some(Options {
        source: side("--source-inventory", "--source-origin", "--source-limit")?,
        target: side("--inventory", "--origin", "--limit")?,
        repository: repository.into(),
        wait_seconds: values
            .get("--wait-seconds")
            .map_or(Ok(3600), |value| number(value, 7200))?,
        revision: values
            .get("--revision")
            .map(|value| revision(value))
            .transpose()?,
        dns_wait_seconds: values
            .get("--dns-wait-seconds")
            .map_or(Ok(600), |value| number(value, 1800))?,
        ansible_playbook: optional_path("--ansible-playbook")?,
        gh: optional_path("--gh")?,
        activate_inventory: optional_path("--activate-inventory")?,
        update_canary: seen.contains(UPDATE_CANARY),
        application_artifact_dir: optional_path("--application-artifact-dir")?,
        ops_artifact_zip: optional_path(LAST_VALUE_FLAG)?,
    }))
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
