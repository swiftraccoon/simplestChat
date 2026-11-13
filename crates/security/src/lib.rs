//! The repository's security gate. `tools` installs the checksum-pinned
//! scanners the gate runs and verifies an installation before each use;
//! `policy` reads the reviewed exceptions a finding may be matched against,
//! `findings` holds each scanner's report to them, `scope` says which
//! sources a diagnostic covers, `context` runs an invocation's bounded
//! commands and takes its private copy of the sources, and `secrets` turns
//! that copy into what a secret scanner must read.

pub mod context;
pub(crate) mod files;
pub mod findings;
pub mod policy;
pub(crate) mod record;
pub mod scope;
#[cfg(test)]
pub(crate) mod scratch;
pub mod secrets;
pub mod tools;

/// A fixed failure code. It never carries scanner output, report contents or
/// downloaded bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault(pub &'static str);

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Fault {}

/// A verdict of the gate: an answer, or the code of what was refused.
pub type Result<T> = std::result::Result<T, Fault>;

pub(crate) fn require(held: bool, code: &'static str) -> Result<()> {
    if held { Ok(()) } else { Err(Fault(code)) }
}

/// `bytes` in lowercase hexadecimal.
pub(crate) fn hexadecimal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The lowercase SHA-256 of `bytes`.
pub(crate) fn digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hexadecimal(&sha2::Sha256::digest(bytes))
}
