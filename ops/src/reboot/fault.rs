//! Why a reboot operation stopped. The retained outcome keeps a refusal in
//! its own words and every other fault by its kind, and the caller's one
//! line names only the kind.

use crate::host::failure::Failure;

/// One stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// A refusal in its own words.
    Refused(String),
    /// A fault known only by its kind.
    Kind(&'static str),
}

pub(super) type Result<T> = std::result::Result<T, Fault>;

impl Fault {
    /// The kind the caller's line names.
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::Refused(_) => "ReleaseError",
            Self::Kind(kind) => kind,
        }
    }

    /// What the retained outcome says.
    pub(super) fn text(&self) -> String {
        match self {
            Self::Refused(text) => text.clone(),
            Self::Kind(kind) => (*kind).to_owned(),
        }
    }
}

/// Refuse with `text` unless `condition` holds.
pub(super) fn require(condition: bool, text: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Fault::Refused(text.to_owned()))
    }
}

/// A value of the wrong shape.
pub(super) const VALUE: Fault = Fault::Kind("ValueError");
/// A field that is not there.
pub(super) const KEY: Fault = Fault::Kind("KeyError");
/// An execution profile that is not the managed one.
pub(super) const PROFILE: Fault = Fault::Kind("RuntimeProfileError");

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
