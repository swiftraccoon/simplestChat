//! The proxy's certificate storage, read without following anything the
//! proxy controls, and the checks a certificate must pass before its bytes
//! are published or transferred.

use crate::host::command::Request;
use crate::host::fault::{Fault, Numbered, Result, VALUE, require};
use crate::host::{Host, domain, hexadecimal};
use rustix::fs::{Mode, OFlags, openat};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The most one certificate, key or metadata file may hold.
pub(crate) const MAX_CERTIFICATE: usize = 65536;
/// The most entries the certificate storage may list.
pub(crate) const MAX_AUTHORITIES: usize = 64;
/// A key that does not belong to its certificate, or either unreadable.
pub(crate) const PAIR: Fault = Fault::Kind("SSLError");
const NO_FOLLOW: OFlags = OFlags::NOFOLLOW.union(OFlags::CLOEXEC);

/// One prepared host and the identities its relay files belong to.
pub(crate) struct Site<'a> {
    pub host: &'a Host<'a>,
    /// The proxy's own user, which owns what it stores.
    pub proxy: u32,
    /// The proxy's own group.
    pub proxy_group: u32,
    /// The group the relay reads its certificate and configuration by.
    pub relay_group: u32,
    /// Admit commands again after a termination request, so that one
    /// request still lets an interrupted replacement be rolled back.
    pub rearm: &'a dyn Fn(),
}

impl Site<'_> {
    /// Where the proxy keeps issued certificates, one directory per issuer.
    pub(crate) fn certificates(&self) -> PathBuf {
        let root = &self.host.paths.root;
        root.join("caddy-data/caddy/certificates")
    }

    fn sources(&self) -> [u32; 2] {
        [self.host.owner, self.proxy]
    }
}

/// Require the relay's hostname and shared secret to be ones the
/// application's own settings admit.
pub(crate) fn relay_environment(hostname: &str, secret: &str) -> Result<()> {
    let admitted = domain(hostname) && hexadecimal(secret, 64);
    require(admitted, "Invalid managed TURN configuration")
}

/// One directory entry of `parent`, opened as a directory and never through
/// a link.
pub(crate) fn child(
    parent: impl AsFd,
    name: impl rustix::path::Arg,
) -> rustix::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | NO_FOLLOW;
    openat(parent, name, flags, Mode::empty())
}

/// Whether an opened directory belongs to root or the proxy and no one else
/// may write to it.
pub(crate) fn kept(site: &Site<'_>, directory: &OwnedFd) -> Result<bool> {
    let metadata = File::from(directory.try_clone()?).metadata()?;
    Ok(site.sources().contains(&metadata.uid()) && metadata.mode() & 0o022 == 0)
}

/// Open `path` one component at a time from the machine's root, following
/// no link; every component must belong to root or the proxy and be
/// writable by no one else.
pub(crate) fn directory(site: &Site<'_>, path: &Path) -> Result<OwnedFd> {
    let anchor = &site.host.paths.anchor;
    let below = path.strip_prefix(anchor).map_err(|_| VALUE)?;
    let opened = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut descriptor = rustix::fs::open(anchor, opened, Mode::empty())?;
    for component in below.components() {
        let Component::Normal(name) = component else {
            return Err(VALUE);
        };
        descriptor = child(&descriptor, name)?;
        require(kept(site, &descriptor)?, "Unsafe certificate directory")?;
    }
    Ok(descriptor)
}

/// The names in an opened directory, at most `most` of them.
pub(crate) fn names(directory: &OwnedFd, most: usize, refusal: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        require(found.len() < most, refusal)?;
        found.push(String::from_utf8(name.to_vec()).map_err(|_| VALUE)?);
    }
    Ok(found)
}

/// One regular file of an opened directory, read whole within the bound even
/// if it grows, with the nanosecond it was last modified.
pub(crate) fn read(
    site: &Site<'_>,
    directory: &OwnedFd,
    name: &str,
    private_key: bool,
) -> Result<(Vec<u8>, i128)> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | NO_FOLLOW;
    let file = File::from(openat(directory, name, flags, Mode::empty())?);
    let metadata = file.metadata()?;
    let bound = MAX_CERTIFICATE as u64;
    let safe = metadata.is_file()
        && site.sources().contains(&metadata.uid())
        && metadata.mode() & 0o022 == 0
        && (1..=bound).contains(&metadata.len());
    require(safe, "Unsafe certificate source")?;
    let private = matches!(metadata.mode() & 0o7777, 0o400 | 0o600);
    require(!private_key || private, "Unsafe source key permissions")?;
    let mut data = Vec::new();
    file.take(bound + 1).read_to_end(&mut data)?;
    let whole = !data.is_empty() && data.len() <= MAX_CERTIFICATE;
    require(whole, "Oversized or empty certificate source")?;
    let modified = i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec());
    Ok((data, modified))
}

/// The public key a command prints, or the fault of a pair that cannot be
/// one.
fn public_key(host: &Host<'_>, argv: &[&str]) -> Result<Vec<u8>> {
    let argv = argv.iter().map(|word| (*word).to_owned()).collect();
    let oversized = "Oversized public key";
    let request = Request::fixed(argv, 10.0, MAX_CERTIFICATE, oversized);
    host.commands.run(&request).map_err(|_| PAIR)
}

/// Require a key that belongs to the certificate, at least one more day of
/// validity, and a chain the system trusts for `hostname`.
pub(crate) fn validate(
    runner: &mut Numbered<'_>,
    hostname: &str,
    certificate: &Path,
    key: &Path,
) -> Result<()> {
    let (certificate, key) = (
        certificate.to_str().ok_or(VALUE)?,
        key.to_str().ok_or(VALUE)?,
    );
    let host = runner.0.host;
    let issued = public_key(
        host,
        &[
            "/usr/bin/openssl",
            "x509",
            "-in",
            certificate,
            "-noout",
            "-pubkey",
        ],
    )?;
    let held = public_key(host, &["/usr/bin/openssl", "pkey", "-in", key, "-pubout"])?;
    if issued != held {
        return Err(PAIR);
    }
    let valid = [
        "/usr/bin/openssl",
        "x509",
        "-in",
        certificate,
        "-checkend",
        "86400",
        "-noout",
    ];
    runner.run(&valid, 30.0)?;
    let trusted = [
        "/usr/bin/openssl",
        "verify",
        "-purpose",
        "sslserver",
        "-verify_hostname",
        hostname,
        "-CAfile",
        "/etc/ssl/certs/ca-certificates.crt",
        "-untrusted",
        certificate,
        certificate,
    ];
    runner.run(&trusted, 30.0)?;
    Ok(())
}
