//! Explicit controller identities and a single original operation budget.

use super::super::{
    acquire::options::{self, Selected},
    control::{Cause, Result},
    files,
};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release fetch --repository REPO --revision SHA ",
    "--artifact-id N --ci-run N --host HOST --user USER --identity ABS ",
    "[--port N] --output-parent ABS [--verified-directory ABS] ",
    "--timeout-seconds N --finalization-seconds M\n",
    "Verify explicit server bytes, then fetch through the prepared Python SSH receiver. ",
    "0 < M < N; no automatic retry or installed helper switch."
);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ParseError {
    Usage,
    Invalid,
}
pub(super) struct Options {
    pub local: options::Options,
    pub host: String,
    pub user: String,
    pub identity: PathBuf,
    pub port: u16,
    #[cfg(test)]
    pub environment: Option<super::super::verify::tool::Environment>,
}
fn positive(value: &str, maximum: u64) -> Result<u64> {
    if value.is_empty()
        || value.len() > 16
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Cause::Input);
    }
    let value: u64 = value.parse().map_err(|_| Cause::Input)?;
    (value <= maximum).then_some(value).ok_or(Cause::Input)
}
fn host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn user(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && (value.as_bytes()[0].is_ascii_lowercase() || value.starts_with('_'))
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}
impl Options {
    pub fn parse(arguments: &[OsString]) -> std::result::Result<Option<Self>, ParseError> {
        if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("-h" | "--help")) {
            return Ok(None);
        }
        let mut fields = BTreeMap::new();
        let mut at = 0;
        while at < arguments.len() {
            let name = arguments[at].to_str().ok_or(ParseError::Usage)?;
            let (name, value) = if let Some((name, value)) = name.split_once('=') {
                (name, OsString::from(value))
            } else {
                at += 1;
                (name, arguments.get(at).ok_or(ParseError::Usage)?.clone())
            };
            if ![
                "--repository",
                "--revision",
                "--artifact-id",
                "--ci-run",
                "--host",
                "--user",
                "--identity",
                "--port",
                "--output-parent",
                "--verified-directory",
                "--timeout-seconds",
                "--finalization-seconds",
            ]
            .contains(&name)
                || fields.insert(name, value).is_some()
            {
                return Err(ParseError::Usage);
            }
            at += 1;
        }
        for required in [
            "--repository",
            "--revision",
            "--artifact-id",
            "--ci-run",
            "--host",
            "--user",
            "--identity",
            "--output-parent",
            "--timeout-seconds",
            "--finalization-seconds",
        ] {
            if !fields.contains_key(required) {
                return Err(ParseError::Usage);
            }
        }
        // These new explicit native budgets have no Python argparse counterpart.
        // Their syntax/relationship remains a usage error, without side effects.
        let budget = |name| {
            fields
                .get(name)
                .and_then(|v| v.to_str())
                .ok_or(ParseError::Usage)
                .and_then(|value| options::number(value).map_err(|_| ParseError::Usage))
        };
        let whole = budget("--timeout-seconds")?;
        let reserve = budget("--finalization-seconds")?;
        if reserve == 0 || reserve >= whole {
            return Err(ParseError::Usage);
        }
        let validated = (|| -> Result<Self> {
            let text = |name| {
                fields
                    .get(name)
                    .and_then(|v| v.to_str())
                    .ok_or(Cause::Input)
            };
            let selected = Selected::new(
                text("--repository")?.into(),
                text("--revision")?.into(),
                positive(text("--artifact-id")?, options::SAFE)?,
                positive(text("--ci-run")?, options::SAFE)?,
            )?;
            let host_value = text("--host")?.to_owned();
            let user_value = text("--user")?.to_owned();
            if !host(&host_value) || !user(&user_value) {
                return Err(Cause::Input);
            }
            let path = |name| fields.get(name).map(PathBuf::from).ok_or(Cause::Input);
            let identity = path("--identity")?;
            let parent = path("--output-parent")?;
            // Explicit empty is present and refused as a native canonical input;
            // it never changes the reference's cached branch into a fresh fetch.
            let artifact = fields.get("--verified-directory").map(PathBuf::from);
            if !files::canonical(&identity)
                || !files::canonical(&parent)
                || artifact.as_ref().is_some_and(|v| !files::canonical(v))
                || identity.to_str().is_none()
                || parent.to_str().is_none()
                || artifact.as_ref().is_some_and(|v| v.to_str().is_none())
            {
                return Err(Cause::Input);
            }
            let port = match fields.get("--port") {
                Some(value) => positive(value.to_str().ok_or(Cause::Input)?, u16::MAX.into())?,
                None => 22,
            };
            Ok(Self {
                local: options::Options::internal(
                    selected,
                    parent,
                    artifact,
                    Duration::from_secs(whole),
                    Duration::from_secs(reserve),
                ),
                host: host_value,
                user: user_value,
                identity,
                port: u16::try_from(port).map_err(|_| Cause::Input)?,
                #[cfg(test)]
                environment: None,
            })
        })()
        .map_err(|_| ParseError::Invalid)?;
        Ok(Some(validated))
    }
    pub fn ssh_arguments(&self) -> Vec<OsString> {
        let mut values = Vec::new();
        for setting in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "ConnectTimeout=15",
            "ControlMaster=no",
            "ControlPath=none",
            "ForwardAgent=no",
            "ForwardX11=no",
            "ClearAllForwardings=yes",
            "PermitLocalCommand=no",
            "ProxyCommand=none",
            "ProxyJump=none",
            "IdentitiesOnly=yes",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "SendEnv=-*",
            "ServerAliveInterval=10",
            "ServerAliveCountMax=3",
        ] {
            values.extend(["-o".into(), setting.into()]);
        }
        values.extend([
            "-i".into(),
            self.identity.as_os_str().to_owned(),
            "-p".into(),
            self.port.to_string().into(),
            "-l".into(),
            self.user.clone().into(),
            self.host.clone().into(),
            if self.user == "root" {
                "/usr/bin/python3 -B /usr/local/libexec/simplestchat-public/fetch-release.py"
            } else {
                concat!(
                    "sudo -n -- /usr/bin/python3 -B ",
                    "/usr/local/libexec/simplestchat-public/fetch-release.py"
                )
            }
            .into(),
        ]);
        values
    }
}
