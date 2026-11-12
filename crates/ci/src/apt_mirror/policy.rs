//! Closed image/profile admission; repository definitions remain read-only.
//!
//! GitHub hosts Ubuntu runners in two clouds. An image prefers its cloud's
//! own mirror, Azure's or the regional one of an AWS region, before the two
//! official archives; both lists are admitted and reordered the same way.

use crate::runner;
use simplestchat_toolkit::tooling::{Environment, Failure, Result};
use std::ffi::OsStr;

/// The list of an Azure-hosted image as it is delivered.
pub(super) const BEFORE: &[u8] = concat!(
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n",
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n",
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n",
)
.as_bytes();
/// The Azure list as this command leaves it.
#[cfg(test)]
pub(super) const AFTER: &[u8] = concat!(
    "https://archive.ubuntu.com/ubuntu/\tpriority:1\n",
    "https://security.ubuntu.com/ubuntu/\tpriority:2\n",
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:3\n",
)
.as_bytes();
const OFFICIAL: [&str; 2] = [
    "https://archive.ubuntu.com/ubuntu/",
    "https://security.ubuntu.com/ubuntu/",
];
const AZURE: &str = "http://azure.archive.ubuntu.com/ubuntu/";
/// What follows an AWS region's name in the address of its mirror.
const REGIONAL: &str = ".ec2.archive.ubuntu.com/ubuntu/";
/// The longest name of an AWS region a list may carry.
const REGION_MAX: usize = 16;
/// The most bytes an admitted list takes: the Azure list with the longest
/// regional mirror in place of Azure's.
pub(super) const LIST_MAX: u64 =
    (BEFORE.len() - AZURE.len() + "http://".len() + REGION_MAX + REGIONAL.len()) as u64;
/// The official security archive, which an AWS image names directly.
const SECURITY: [&str; 2] = [
    "http://security.ubuntu.com/ubuntu/",
    "http://security.ubuntu.com/ubuntu",
];

/// A refusal that names only the check that refused.
fn failure(check: &str) -> Failure {
    Failure::new(
        2,
        format!("Unsupported hosted Ubuntu archive profile: {check}."),
    )
}

pub(super) fn host(
    environment: &Environment,
    system: &str,
    machine: &str,
    effective_uid: u32,
) -> Result<()> {
    if system != "Linux"
        || machine != "x86_64"
        || effective_uid != 0
        || environment.contains_key(OsStr::new("ACT"))
        || [
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
            ("ImageOS", "ubuntu24"),
        ]
        .iter()
        .any(|(name, value)| runner::text(environment, name) != *value)
    {
        return Err(failure("host"));
    }
    Ok(())
}

/// Whether `name` has the shape of an AWS region: an area of two letters,
/// one or two words and one digit, as `us-east-2` or `us-gov-west-1`.
fn region(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    let Some((number, words)) = parts.split_last() else {
        return false;
    };
    let lower =
        |word: &&str| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_lowercase());
    name.len() <= REGION_MAX
        && (2..=3).contains(&words.len())
        && words[0].len() == 2
        && words.iter().all(lower)
        && number.len() == 1
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `address` is the mirror a hosted image's own cloud provides.
fn cloud(address: &str) -> bool {
    let regional = address.strip_prefix("http://");
    let regional = regional.and_then(|rest| rest.strip_suffix(REGIONAL));
    address == AZURE || regional.is_some_and(region)
}

/// A list of three mirrors in the order they are preferred.
fn list(order: [&str; 3]) -> Vec<u8> {
    let lines = order.iter().zip(1..);
    let lines = lines.map(|(address, priority)| format!("{address}\tpriority:{priority}\n"));
    lines.collect::<String>().into_bytes()
}

/// The list to install for an original list, which prefers the cloud's own
/// mirror; nothing for a list that already prefers the official archives.
/// Any other bytes are refused: nothing is ever templated from them.
pub(super) fn admit(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let refused = || failure("mirror list");
    let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
    let addresses = text.lines().filter_map(|line| line.split('\t').next());
    let mirror = addresses.into_iter().find(|address| cloud(address));
    let mirror = mirror.ok_or_else(refused)?;
    let [archive, security] = OFFICIAL;
    let preferred = list([archive, security, mirror]);
    if bytes == list([mirror, archive, security]) {
        Ok(Some(preferred))
    } else if bytes == preferred {
        Ok(None)
    } else {
        Err(refused())
    }
}

fn text<'a>(bytes: &'a [u8], check: &str) -> Result<&'a str> {
    if bytes.is_empty()
        || bytes.len() > 16 * 1024
        || bytes
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\t'))
    {
        return Err(failure(check));
    }
    std::str::from_utf8(bytes).map_err(|_| failure(check))
}

pub(super) fn os_release(bytes: &[u8]) -> Result<()> {
    let failure = || failure("os-release");
    let mut found = [false; 3];
    for line in text(bytes, "os-release")?.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line.split_once('=').ok_or_else(failure)?;
        for (index, (key, expected)) in [
            ("ID", "ubuntu"),
            ("VERSION_ID", "24.04"),
            ("VERSION_CODENAME", "noble"),
        ]
        .iter()
        .enumerate()
        {
            if name == *key {
                if found[index] || (value != *expected && value != format!("\"{expected}\"")) {
                    return Err(failure());
                }
                found[index] = true;
            }
        }
    }
    if !found.into_iter().all(|value| value) {
        return Err(failure());
    }
    Ok(())
}

/// Check that this list is selected without rewriting pockets, keys or
/// options. Every stanza names the list, or the official security archive
/// itself, and at least one names the list.
pub(super) fn sources(bytes: &[u8]) -> Result<()> {
    let failure = || failure("sources");
    let mut found = false;
    let mut in_stanza = false;
    for line in text(bytes, "sources")?.lines() {
        if line.is_empty() {
            in_stanza = false;
            continue;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err(failure());
        }
        let (key, value) = line.split_once(':').ok_or_else(failure)?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(failure());
        }
        if key.eq_ignore_ascii_case("URIs") {
            let listed = value.trim() == "mirror+file:/etc/apt/apt-mirrors.txt";
            if in_stanza || !(listed || SECURITY.contains(&value.trim())) {
                return Err(failure());
            }
            found |= listed;
            in_stanza = true;
        }
    }
    if !found {
        return Err(failure());
    }
    Ok(())
}
