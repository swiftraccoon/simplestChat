//! Read-only release-host admission. No installed helper is imported or run.
//! The receipt describes one optimistic filesystem observation, not a lock,
//! authenticated host identity, installed Ops proof or authorization to release.

#[path = "preflight_files.rs"]
mod files;
#[path = "preflight_platform.rs"]
mod platform;

/// The distribution and version an os-release file names.
pub(crate) use platform::fields as os_release;
use serde::Serialize;
use std::fmt;

/// The fixed public failure vocabulary. Operating-system errors and file names
/// never enter these messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Root,
    Platform,
    Distribution,
    Path,
    Size,
    Unavailable,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Root => "root_required",
            Self::Platform => "unsupported_platform",
            Self::Distribution => "unsupported_distribution",
            Self::Path => "unsafe_release_path",
            Self::Size => "invalid_release_file_size",
            Self::Unavailable => "preflight_unavailable",
        }
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;

/// The public receipt: its schema and a fresh operation identifier, which does
/// not establish durability, later namespace stability or workload readiness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    schema_version: u8,
    run_id: String,
}

trait Host {
    type Access: files::Access;
    fn identity(&mut self) -> Result<platform::Identity>;
    fn filesystem(&mut self) -> Result<files::Tree<Self::Access>>;
    fn random(&mut self) -> Result<[u8; 16]>;
}

struct Native;
impl Host for Native {
    type Access = files::NativeAccess;
    fn identity(&mut self) -> Result<platform::Identity> {
        let identity = rustix::system::uname();
        Ok(platform::Identity {
            euid: rustix::process::geteuid().as_raw(),
            system: identity.sysname().to_bytes().to_vec(),
            machine: identity.machine().to_bytes().to_vec(),
        })
    }
    fn filesystem(&mut self) -> Result<files::Tree<Self::Access>> {
        files::Tree::native()
    }
    #[cfg(target_os = "linux")]
    fn random(&mut self) -> Result<[u8; 16]> {
        let mut bytes = [0; 16];
        let mut filled = 0;
        while filled < bytes.len() {
            let count = rustix::rand::getrandom(
                &mut bytes[filled..],
                rustix::rand::GetRandomFlags::empty(),
            )
            .map_err(|_| Failure::Unavailable)?;
            if count == 0 {
                return Err(Failure::Unavailable);
            }
            filled += count;
        }
        Ok(bytes)
    }
    #[cfg(not(target_os = "linux"))]
    fn random(&mut self) -> Result<[u8; 16]> {
        // Native admission rejects this platform before entropy is requested.
        Err(Failure::Platform)
    }
}

/// Check the fixed release paths on the current root Linux/amd64 Debian 13 host:
/// the configuration and release roots are root-owned 0700 directories and
/// `images.json` is a root-owned 0600 regular file of at most 64 KiB. Namespace
/// bindings are checked and all owned File closes are attempted before success.
///
/// # Errors
/// Returns the first fixed [`Failure`]: not root, another platform or
/// distribution, an unsafe path, an unsupported size, or an unavailable check.
pub fn check() -> Result<Receipt> {
    check_with(&mut Native)
}

/// Admit only the existing root/Linux/amd64/Debian13 platform profile. Reuse the
/// original OS snapshot, namespace verification and all-attempt closes; do not
/// inspect application configuration, read images.json or request entropy.
pub(crate) fn platform_only() -> Result<()> {
    platform_with(&mut Native)
}

fn platform_with(host: &mut impl Host) -> Result<()> {
    host.identity()?.validate()?;
    let mut tree = host.filesystem()?;
    let observed = tree
        .os_release()
        .and_then(|bytes| platform::distribution(&bytes));
    let closed = tree.finish();
    observed.and(closed)
}

#[cfg(test)]
#[test]
fn installer_platform_facade_uses_only_original_os_snapshot_and_closes() {
    let mut host = tests::FixtureHost::new();
    std::fs::remove_file(host.root.join("etc/simplestchat-public/images.json")).unwrap();
    std::fs::remove_dir(host.root.join("srv/simplestchat-public/releases")).unwrap();
    std::fs::remove_dir(host.root.join("srv/simplestchat-public")).unwrap();
    assert!(platform_with(&mut host).is_ok());
    assert_eq!(host.random_calls, 0);
    assert!(host.access.0.borrow().live.is_empty());
    assert!(
        host.access
            .0
            .borrow()
            .reads
            .iter()
            .all(|path| path.ends_with("os-release"))
    );

    let mut failed = tests::FixtureHost::new();
    failed.access.0.borrow_mut().fail_close = Some(1);
    assert!(platform_with(&mut failed).is_err());
    assert_eq!(failed.random_calls, 0);
    assert!(failed.access.0.borrow().live.is_empty());
}

fn check_with(host: &mut impl Host) -> Result<Receipt> {
    host.identity()?.validate()?;
    let mut tree = host.filesystem()?;
    let observed = (|| {
        platform::distribution(&tree.os_release()?)?;
        tree.protected_directory("etc/simplestchat-public", 0o700)?;
        tree.protected_directory("srv/simplestchat-public", 0o700)?;
        tree.protected_file("etc/simplestchat-public/images.json", 0o600, 65536)?;
        tree.verify()
    })();
    let closed = tree.finish();
    observed?;
    closed?;
    let mut bytes = host.random()?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    Ok(Receipt {
        schema_version: 1,
        run_id: bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
    })
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
