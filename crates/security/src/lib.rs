//! The repository's security gate. `tools` installs the checksum-pinned
//! scanners the gate runs and verifies an installation before each use;
//! `policy` reads the reviewed exceptions a finding may be matched against,
//! `findings` holds each scanner's report to them, `scope` says which
//! sources a diagnostic covers, `context` runs an invocation's bounded
//! commands and takes its private copy of the sources, `secrets` turns that
//! copy into what a secret scanner must read, `environment` builds the
//! hash-locked Python environment of the scanners that need one, `licenses`
//! reads the dependency declarations of the npm and Python locks, asks the
//! registries about them and reviews the licences of what a change declares
//! anew, `actions` reviews the actions a change pins anew, `spdx` judges a
//! licence expression against an allowlist, `openssl` holds the pinned
//! OpenSSL to the vendor's newest release, `semgrep` scans the maintained
//! sources with the reviewed local rules, `fast` holds the checks of the
//! fast tier, and `rulesets` reconciles the repository's rulesets on GitHub
//! with their policy.

pub mod actions;
pub mod context;
pub mod environment;
pub mod fast;
pub(crate) mod files;
pub mod findings;
pub mod licenses;
pub mod openssl;
pub mod policy;
pub(crate) mod record;
pub mod rulesets;
pub mod scope;
#[cfg(test)]
pub(crate) mod scratch;
pub mod secrets;
pub mod semgrep;
pub mod spdx;
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

/// Whether `name` is the full name of a commit: forty digits of lowercase
/// hexadecimal.
pub(crate) fn commit(name: &str) -> bool {
    let digit = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
    name.len() == 40 && name.bytes().all(digit)
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
