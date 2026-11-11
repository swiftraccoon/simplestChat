//! Publication of the proxy's newest certificate for the relay: validated
//! private snapshots, an immutable generation of exactly those bytes, and
//! one atomic switch of the selection.

use super::store::{self, MAX_AUTHORITIES, Site, relay_environment};
use crate::host::fault::{Fault, Numbered, Result, digest, directory, file, protected, require};
use crate::host::state::{self, Access};
use rustix::io::Errno;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A private directory that does not outlive its use.
pub(crate) struct Disposable(pub PathBuf);

impl Disposable {
    pub(crate) fn new(parent: &Path, prefix: &str) -> Result<Self> {
        Ok(Self(state::private_directory(parent, prefix)?))
    }
}

impl Drop for Disposable {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The newest certificate any issuer holds for exactly `hostname`, with its
/// key, each read once within its bound.
fn source(site: &Site<'_>, hostname: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    relay_environment(hostname, &"0".repeat(64))?;
    let root = store::directory(site, &site.certificates())?;
    let mut newest: Option<(i128, Vec<u8>, Vec<u8>)> = None;
    let issuers = store::names(&root, MAX_AUTHORITIES, "Too many certificate authorities")?;
    for issuer in issuers {
        let issuer = store::child(&root, issuer.as_str())?;
        let held = match store::child(&issuer, hostname) {
            Ok(held) => held,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(Fault::from(error)),
        };
        let (certificate, modified) = store::read(site, &held, &format!("{hostname}.crt"), false)?;
        let (key, _) = store::read(site, &held, &format!("{hostname}.key"), true)?;
        if newest.as_ref().is_none_or(|(seen, ..)| modified > *seen) {
            newest = Some((modified, certificate, key));
        }
    }
    let missing = "Caddy has not issued the relay hostname certificate";
    let (_, certificate, key) = newest.ok_or_else(|| Fault::Refused(missing.to_owned()))?;
    Ok((certificate, key))
}

/// Publish the newest valid pair and select it. Returns whether the
/// selection changed.
pub(super) fn publish(site: &Site<'_>, runner: &mut Numbered<'_>, hostname: &str) -> Result<bool> {
    let (certificate, key) = source(site, hostname)?;
    let config = &site.host.paths.relay;
    {
        // The proxy cannot change these root-owned copies while they are
        // checked; a concurrent renewal can only yield a pair that fails.
        let snapshot = Disposable::new(config, ".certificate-")?;
        let (leaf, private) = (
            snapshot.0.join("certificate.pem"),
            snapshot.0.join("key.pem"),
        );
        state::atomic(&leaf, &certificate)?;
        state::atomic(&private, &key)?;
        store::validate(runner, hostname, &leaf, &private)?;
    }
    let owner = site.host.owner;
    let generation = digest(&certificate);
    let tls = config.join("tls");
    protected(owner, &tls, directory(0o750))?;
    let destination = tls.join(&generation);
    let pair = [("certificate.pem", &certificate), ("key.pem", &key)];
    if destination.exists() {
        protected(owner, &destination, directory(0o750))?;
        for (name, data) in pair {
            let path = destination.join(name);
            protected(owner, &path, file(0o440, store::MAX_CERTIFICATE as u64))?;
            require(std::fs::read(&path)? == *data, "Certificate copy differs")?;
        }
    } else {
        std::fs::DirBuilder::new()
            .mode(0o750)
            .create(&destination)?;
        std::os::unix::fs::chown(&destination, Some(owner), Some(site.relay_group))?;
        std::fs::set_permissions(&destination, PermissionsExt::from_mode(0o750))?;
        let access = Access {
            owner,
            group: site.relay_group,
            mode: 0o440,
        };
        for (name, data) in pair {
            state::atomic_for(&destination.join(name), data, &access)?;
        }
    }
    let current = tls.join("current");
    let selected = std::fs::symlink_metadata(&current).ok();
    let linked = selected.as_ref().is_some_and(std::fs::Metadata::is_symlink);
    if linked && std::fs::read_link(&current)? == Path::new(&generation) {
        return Ok(false);
    }
    require(selected.is_none() || linked, "Unexpected TLS selection")?;
    let next = tls.join(".next");
    let unfinished = std::fs::symlink_metadata(&next).is_ok();
    require(!unfinished, "Unfinished TLS selection")?;
    std::os::unix::fs::symlink(&generation, &next)?;
    std::fs::rename(&next, &current)?;
    Ok(true)
}
