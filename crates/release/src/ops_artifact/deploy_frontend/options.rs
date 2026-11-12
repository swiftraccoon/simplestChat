//! Pure argument admission for the complete deployment frontend: the signed
//! route, and the explicitly forced one for an unsigned local export.
//! Paths select data and tools; the runtime owner must retain and validate their
//! actual filesystem identities. Parsing starts no I/O, process or network work.

use super::{failure, policy::REPOSITORY};
use simplestchat_toolkit::tooling::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

pub(super) const USAGE: &str = concat!(
    "usage: simplestchat-release deploy --inventory ABS --repository swiftraccoon/simplestChat ",
    "--origin https://HOST [OPTIONS]\n\n",
    "  --limit HOST                    exact inventory host alias\n",
    "  --wait-seconds N                CI wait, 0..7200; default 3600\n",
    "  --quiet-seconds N               room observation, 0..600; default 600\n",
    "  --ansible-playbook ABS          caller-trusted Ansible executable\n",
    "  --gh ABS                        caller-trusted GitHub CLI executable\n",
    "  --application-artifact-dir ABS  original application ZIP and ten members\n",
    "  --ops-artifact-zip ABS          original Ops ZIP candidate bytes\n",
    "  --force                         deploy an unsigned local export; no CI wait\n",
    "  --artifact-dir ABS              with --force: the export of this checkout\n\n",
    "Build this controller before supplying credentials. The complete operation ",
    "requires a clean published main revision and current signed artifacts.\n",
    "--force replaces both with one export of the clean checkout made by ",
    "simplestchat-ops release build; nothing attests it and the reports say so.\n",
);

/// The flag that asks for the unsigned route; it takes no value.
const FORCE: &str = "--force";
/// What selects signed inputs, and so has no place beside `--force`.
const SIGNED_ONLY: [&str; 3] = [
    "--wait-seconds",
    "--application-artifact-dir",
    "--ops-artifact-zip",
];
const VALUE_FLAGS: [&str; 11] = [
    "--inventory",
    "--repository",
    "--origin",
    "--limit",
    "--wait-seconds",
    "--quiet-seconds",
    "--ansible-playbook",
    "--gh",
    "--application-artifact-dir",
    "--ops-artifact-zip",
    "--artifact-dir",
];

pub(super) struct Options {
    pub inventory: PathBuf,
    pub repository: String,
    pub origin: String,
    pub origin_host: String,
    pub limit: Option<String>,
    pub wait_seconds: u64,
    pub quiet_seconds: u64,
    pub ansible_playbook: Option<PathBuf>,
    pub gh: Option<PathBuf>,
    pub application_artifact_dir: Option<PathBuf>,
    pub ops_artifact_zip: Option<PathBuf>,
    /// The unsigned export to deploy, which is what selects the forced route.
    pub artifact_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SignedDeploymentOptions")
    }
}

pub(super) fn path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.len() > 4096
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path.file_name().is_none()
        || !path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        || value
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        Err(failure(2))
    } else {
        Ok(path.to_owned())
    }
}

pub(super) fn number(value: &str, maximum: u64) -> Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(failure(2));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|number| *number <= maximum)
        .ok_or_else(|| failure(2))
}

pub(super) fn alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn hostname(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

pub(super) fn origin(value: &str) -> Result<(String, String)> {
    let authority = value.strip_prefix("https://").ok_or_else(|| failure(2))?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let host = match authority.split_once(':') {
        Some((host, "443")) => host,
        Some(_) => return Err(failure(2)),
        None => authority,
    };
    if !hostname(host) {
        return Err(failure(2));
    }
    // Admit the lexical authority before the URL parser can normalize encoded
    // hosts, backslashes, IPv4 aliases, case, dot segments or an explicit port.
    let parsed = url::Url::parse(value).map_err(|_| failure(2))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(host)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(failure(2));
    }
    Ok((format!("https://{host}"), host.to_owned()))
}

pub(super) fn parse(arguments: &[String]) -> Result<Option<Options>> {
    if arguments == ["--help"] {
        return Ok(None);
    }
    if arguments.len() > 20
        || arguments
            .iter()
            .any(|value| value.len() > 4096 || value.chars().any(char::is_control))
    {
        return Err(failure(2));
    }
    let mut seen = BTreeSet::new();
    let mut values = BTreeMap::new();
    let mut remaining = arguments.iter();
    while let Some(flag) = remaining.next() {
        if !seen.insert(flag.as_str()) {
            return Err(failure(2));
        }
        if flag == FORCE {
            continue;
        }
        if !VALUE_FLAGS.contains(&flag.as_str()) {
            return Err(failure(2));
        }
        let value = remaining.next().ok_or_else(|| failure(2))?;
        values.insert(flag.as_str(), value.as_str());
    }
    // The flag and the export select the unsigned route only together, and
    // nothing that selects signed inputs may stand beside them.
    let forced = seen.contains(FORCE);
    if forced != values.contains_key("--artifact-dir")
        || (forced && SIGNED_ONLY.iter().any(|flag| values.contains_key(flag)))
    {
        return Err(failure(2));
    }
    let required = |flag| values.get(flag).copied().ok_or_else(|| failure(2));
    let optional_path = |flag| values.get(flag).map(|value| path(value)).transpose();
    let repository = required("--repository")?;
    if repository != REPOSITORY {
        return Err(failure(2));
    }
    let limit = values.get("--limit").copied();
    if limit.is_some_and(|limit| !alias(limit)) {
        return Err(failure(2));
    }
    let (origin, origin_host) = origin(required("--origin")?)?;
    Ok(Some(Options {
        inventory: path(required("--inventory")?)?,
        repository: repository.into(),
        origin,
        origin_host,
        limit: limit.map(str::to_owned),
        wait_seconds: values
            .get("--wait-seconds")
            .map_or(Ok(if forced { 0 } else { 3600 }), |value| {
                number(value, 7200)
            })?,
        quiet_seconds: values
            .get("--quiet-seconds")
            .map_or(Ok(600), |value| number(value, 600))?,
        ansible_playbook: optional_path("--ansible-playbook")?,
        gh: optional_path("--gh")?,
        application_artifact_dir: optional_path("--application-artifact-dir")?,
        ops_artifact_zip: optional_path("--ops-artifact-zip")?,
        artifact_dir: optional_path("--artifact-dir")?,
    }))
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
