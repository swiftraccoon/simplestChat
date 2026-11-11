//! One failure type for the monitoring commands. A refusal carries a fixed
//! message; everything else is a class of fault without private detail.

/// Why a monitoring step did not produce its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A named policy refusal with fixed, non-sensitive text.
    Refused(&'static str),
    /// Data that is not the expected shape.
    Value,
    /// A required field that is absent.
    Key,
    /// A file or socket operation that failed.
    Os,
    /// A fixed command that failed or could not start.
    Command,
    /// A fixed command that outlived its deadline.
    Timeout,
}

pub(crate) type Result<T> = std::result::Result<T, Failure>;

/// Refuse with `message` unless `condition` holds.
pub(crate) fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Failure::Refused(message))
    }
}

impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Os
    }
}
