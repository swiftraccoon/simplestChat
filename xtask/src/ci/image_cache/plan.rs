//! Fixed reference commands and scanner-pin/trust/generation framing.

use super::{Selection, failure};
use crate::{
    ci::{
        command::{Command, Program},
        runner,
    },
    tooling::{Environment, Failure, Result},
};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Hint,
    Preparation,
    Production,
}
impl Operation {
    pub(super) fn parse(arguments: &[String]) -> Result<Self> {
        match arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["hint"] => Ok(Self::Hint),
            ["status", "preparation"] => Ok(Self::Preparation),
            ["status", "production"] => Ok(Self::Production),
            _ => Err(Failure::new(2, "Invalid image cache operation.")),
        }
    }
}
pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    temporary: PathBuf,
    trusted: bool,
}
impl Context {
    pub(super) fn new(root: &Path, environment: Environment, system: &str) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Image cache requires explicit hosted or disposable Linux CI.",
            ));
        }
        let temporary = PathBuf::from(runner::text(&environment, "RUNNER_TEMP"));
        if !temporary.is_absolute()
            || temporary.is_symlink()
            || !temporary.is_dir()
            || temporary
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(Failure::new(
                2,
                "Image cache requires an existing absolute runner directory.",
            ));
        }
        let trusted = runner::text(&environment, "GITHUB_REF") == "refs/heads/main"
            && runner::text(&environment, "GITHUB_EVENT_NAME") != "pull_request";
        Ok(Self {
            root: root.into(),
            environment,
            temporary,
            trusted,
        })
    }
    pub(super) fn prefix(&self, bytes: &[u8]) -> Result<String> {
        let text = line(bytes)?;
        let trust = if self.trusted { "main" } else { "untrusted" };
        let head = format!("grype-db-v2-{trust}-");
        if !text
            .strip_prefix(&head)
            .and_then(|rest| rest.strip_suffix('-'))
            .is_some_and(hash)
        {
            return Err(failure("Image cache prefix invalid."));
        }
        Ok(text.into())
    }
    pub(super) fn selection(&self, prefix: &str, bytes: &[u8], hint: bool) -> Result<Selection> {
        // Revalidate the prefix even when construction did not involve a child.
        self.prefix(prefix.as_bytes())?;
        let key = line(bytes)?;
        if !key.strip_prefix(prefix).is_some_and(hash) && !(hint && key == prefix) {
            return Err(failure("Image cache key invalid."));
        }
        Ok(Selection(if hint {
            format!("key={key}\nprefix={prefix}\n").into_bytes()
        } else {
            format!("key={key}\n").into_bytes()
        }))
    }
    pub(super) fn reference(&self, operation: Option<Operation>) -> Command {
        let mut command = Command::capture(Program::Python, &["build/security_image_cache.py"]);
        command.capture = Some(4096);
        match operation {
            None => command.arguments.push("--prefix".into()),
            Some(Operation::Hint) => {}
            Some(operation) => {
                command.arguments.push("--status".into());
                let path = if operation == Operation::Preparation {
                    "image-database-preparation/database-status.json"
                } else {
                    "production-security/image/database-status.json"
                };
                command
                    .arguments
                    .push(self.temporary.join(path).into_os_string());
            }
        }
        command
    }
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn line(bytes: &[u8]) -> Result<&str> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| failure("Image cache output invalid."))?
        .trim_end_matches('\n');
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(failure("Image cache output invalid."));
    }
    Ok(text)
}
