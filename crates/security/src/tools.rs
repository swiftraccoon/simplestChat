//! Install and verify checksum-pinned security tools without global executables.
//!
//! The lock (`build/security-tools.lock.json`) names, for each tool and
//! platform, one HTTPS release archive by its SHA-256 and the regular members
//! to keep, each with the destination it is written to. An archive is inspected
//! in memory and never extracted by its own paths. An installation is one
//! private directory published by a rename; reuse verifies the lock, the
//! receipt and every installed byte before it returns a tool's path. An
//! existing directory is only verified, never overlaid: one holding another
//! selection is replaced by its owner.

mod archive;
mod cli;
mod files;
mod install;
mod lock;
mod transport;

#[cfg(test)]
mod tests;

pub use cli::run;
pub use install::{Site, install, path};
pub use transport::{Answer, Curl, Transport};

/// What a selection without names installs: the fast gate's scanners.
pub const FAST_TOOLS: [&str; 7] = [
    "actionlint",
    "zizmor",
    "gitleaks",
    "cargo-deny",
    "cargo-audit",
    "squawk",
    "semgrep-core",
];

/// A fixed failure code. It never carries scanner output or downloaded bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault(pub &'static str);

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Fault {}

/// The installer's verdicts.
pub type Result<T> = std::result::Result<T, Fault>;

/// The bounds one installation works within.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The most bytes one download may have.
    pub archive: u64,
    /// The most bytes one installed file, or one archive member, may have.
    pub executable: u64,
    /// The most members one archive may list.
    pub members: usize,
    /// The most bytes one archive may unpack to, selected or not.
    pub unpacked: u64,
}

impl Limits {
    /// The bounds every caller outside a test uses.
    pub const REVIEWED: Self = Self {
        archive: 128 * 1024 * 1024,
        executable: 256 * 1024 * 1024,
        members: 10_000,
        unpacked: 512 * 1024 * 1024,
    };
}

fn require(held: bool, code: &'static str) -> Result<()> {
    if held { Ok(()) } else { Err(Fault(code)) }
}

/// The lowercase SHA-256 of `bytes`.
fn digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in sha2::Sha256::digest(bytes) {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The four platforms a lock may name, from the kernel's own two names.
fn platform(system: &str, machine: &str) -> Result<&'static str> {
    let (system, machine) = (system.to_lowercase(), machine.to_lowercase());
    match (system.as_str(), machine.as_str()) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64" | "arm64") => Ok("linux-aarch64"),
        ("darwin", "arm64" | "aarch64") => Ok("darwin-arm64"),
        ("darwin", "x86_64") => Ok("darwin-x86_64"),
        _ => Err(Fault("unsupported_tool_platform")),
    }
}
