//! Why an operation stopped, for the commands that keep the diagnostics of
//! the helpers they replace. A retained outcome keeps a refusal in its own
//! words and every other fault by its kind, and the caller's one line names
//! at most the kind.

use super::evidence::{Evidence, Streams};
use super::failure::Failure;
use super::{Host, state};
use crate::json::Json;
use sha2::{Digest, Sha256};
use std::path::Path;

/// One stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// A refusal in its own words.
    Refused(String),
    /// A fault known only by its kind.
    Kind(&'static str),
}

pub(crate) type Result<T> = std::result::Result<T, Fault>;

impl Fault {
    /// The kind the caller's line names.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Refused(_) => "ReleaseError",
            Self::Kind(kind) => kind,
        }
    }

    /// What the retained outcome says.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Refused(text) => text.clone(),
            Self::Kind(kind) => (*kind).to_owned(),
        }
    }
}

/// Refuse with `text` unless `condition` holds.
pub(crate) fn require(condition: bool, text: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Fault::Refused(text.to_owned()))
    }
}

/// A value of the wrong shape.
pub(crate) const VALUE: Fault = Fault::Kind("ValueError");
/// A field that is not there.
pub(crate) const KEY: Fault = Fault::Kind("KeyError");
/// Text that is not JSON at all.
pub(crate) const SYNTAX: Fault = Fault::Kind("JSONDecodeError");

impl From<std::io::Error> for Fault {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        Self::Kind(match error.kind() {
            ErrorKind::NotFound => "FileNotFoundError",
            ErrorKind::AlreadyExists => "FileExistsError",
            ErrorKind::PermissionDenied => "PermissionError",
            ErrorKind::WouldBlock => "BlockingIOError",
            ErrorKind::NotADirectory => "NotADirectoryError",
            ErrorKind::IsADirectory => "IsADirectoryError",
            _ => "OSError",
        })
    }
}

impl From<rustix::io::Errno> for Fault {
    fn from(error: rustix::io::Errno) -> Self {
        Self::from(std::io::Error::from(error))
    }
}

impl From<Failure> for Fault {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Refused(text) => Self::Refused(text.to_owned()),
            Failure::Value => VALUE,
            Failure::Key => KEY,
            Failure::Timeout => Self::Kind("TimeoutExpired"),
            Failure::Os | Failure::Command => Self::Kind("OSError"),
        }
    }
}

/// Strict JSON, with text that is not JSON named apart from a value of the
/// wrong shape.
pub(crate) fn decode(bytes: &[u8]) -> Result<Json> {
    state::decode(bytes).map_err(|_| SYNTAX)
}

/// A required field of an object.
pub(crate) fn field<'a>(value: &'a Json, name: &str) -> Result<&'a Json> {
    value.object().ok_or(VALUE)?.get(name).ok_or(KEY)
}

/// A value that must be text.
pub(crate) fn text(value: &Json) -> Result<&str> {
    value.string().ok_or(VALUE)
}

/// Text that must be UTF-8.
pub(crate) fn utf8(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| VALUE)
}

/// An object whose fields keep the order they are given in.
pub(crate) fn object(fields: Vec<(&str, Json)>) -> Json {
    let order = fields.iter().map(|(name, _)| (*name).to_owned()).collect();
    let entries = fields.into_iter();
    let entries = entries.map(|(name, value)| (name.to_owned(), value));
    Json::Object(entries.collect(), order)
}

pub(crate) fn string(value: &str) -> Json {
    Json::String(value.to_owned())
}

/// Replace a field where it stands, or add it at the end.
pub(crate) fn set(target: &mut Json, name: &str, value: Json) {
    if let Json::Object(entries, order) = target
        && entries.insert(name.to_owned(), value).is_none()
    {
        order.push(name.to_owned());
    }
}

/// The SHA-256 of `bytes` in lowercase hexadecimal.
pub(crate) fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The present second, as retained records spell it.
pub(crate) fn timestamp(host: &Host<'_>) -> Json {
    let now = host.clock.now().format("%Y-%m-%dT%H:%M:%SZ");
    Json::String(now.to_string())
}

/// The commands of one evidence directory. A failed one is named by its
/// number, which is how its retained output is found.
pub(crate) struct Numbered<'a>(pub Evidence<'a>);

impl Numbered<'_> {
    fn fault(&self, failure: Failure) -> Fault {
        match failure {
            Failure::Command => {
                let number = self.0.number;
                Fault::Refused(format!(
                    "Command {number:03} failed; inspect private output"
                ))
            }
            other => Fault::from(other),
        }
    }

    pub(crate) fn run(&mut self, argv: &[&str], seconds: f64) -> Result<Vec<u8>> {
        let argv = argv.iter().map(|word| (*word).to_owned()).collect();
        let outcome = self.0.run(argv, seconds, Streams::default());
        outcome.map_err(|failure| self.fault(failure))
    }

    pub(crate) fn docker(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
        let outcome = self.0.docker(arguments, seconds, Streams::default());
        outcome.map_err(|failure| self.fault(failure))
    }
}

/// The limits one protected path is held to.
#[derive(Clone, Copy)]
pub(crate) struct Kind {
    pub directory: bool,
    pub mode: u32,
    pub limit: Option<u64>,
}

/// A private directory.
pub(crate) const DIRECTORY: Kind = directory(0o700);

/// A directory of one mode.
pub(crate) const fn directory(mode: u32) -> Kind {
    Kind {
        directory: true,
        mode,
        limit: None,
    }
}

/// A file of one mode and at most `limit` bytes.
pub(crate) const fn file(mode: u32, limit: u64) -> Kind {
    Kind {
        directory: false,
        mode,
        limit: Some(limit),
    }
}

/// Require an ordinary path of `owner`, naming it in a refusal.
pub(crate) fn protected(owner: u32, path: &Path, kind: Kind) -> Result<()> {
    // A path that cannot be read at all is reported as that, by its kind.
    std::fs::symlink_metadata(path)?;
    let modes = [kind.mode];
    let checked = state::protected(path, owner, kind.directory, &modes, kind.limit);
    checked.map_err(|failure| match failure {
        Failure::Refused(reason) => Fault::Refused(format!("{reason}: {}", path.display())),
        other => Fault::from(other),
    })
}
