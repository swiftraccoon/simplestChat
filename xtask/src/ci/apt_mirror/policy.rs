//! Closed image/profile admission; repository definitions remain read-only.

use crate::{
    ci::runner,
    tooling::{Environment, Failure, Result},
};
use std::ffi::OsStr;

pub(super) const BEFORE: &[u8] = concat!(
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n",
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n",
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n",
)
.as_bytes();
pub(super) const AFTER: &[u8] = concat!(
    "https://archive.ubuntu.com/ubuntu/\tpriority:1\n",
    "https://security.ubuntu.com/ubuntu/\tpriority:2\n",
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:3\n",
)
.as_bytes();

fn failure() -> Failure {
    Failure::new(2, "Unsupported hosted Ubuntu archive profile.")
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
        return Err(failure());
    }
    Ok(())
}

/// True only for the original list; the preferred list needs no write.
pub(super) fn admit(bytes: &[u8]) -> Result<bool> {
    if bytes == BEFORE {
        Ok(true)
    } else if bytes == AFTER {
        Ok(false)
    } else {
        Err(failure())
    }
}

fn text(bytes: &[u8]) -> Result<&str> {
    if bytes.is_empty()
        || bytes.len() > 16 * 1024
        || bytes
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\t'))
    {
        return Err(failure());
    }
    std::str::from_utf8(bytes).map_err(|_| failure())
}

pub(super) fn os_release(bytes: &[u8]) -> Result<()> {
    let mut found = [false; 3];
    for line in text(bytes)?.lines() {
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

/// Check that this list is selected without rewriting pockets, keys or options.
pub(super) fn sources(bytes: &[u8]) -> Result<()> {
    let mut found = false;
    let mut in_stanza = false;
    for line in text(bytes)?.lines() {
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
            if in_stanza || value.trim() != "mirror+file:/etc/apt/apt-mirrors.txt" {
                return Err(failure());
            }
            found = true;
            in_stanza = true;
        }
    }
    if !found {
        return Err(failure());
    }
    Ok(())
}
