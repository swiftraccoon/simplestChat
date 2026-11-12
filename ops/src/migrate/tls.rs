//! The handoff of one hostname's certificate between the two hosts of a
//! migration that keeps its origin. Only the selected issuer's certificate,
//! key and certificate metadata for that hostname travel, in one private
//! envelope; account keys, other hostnames and other storage do not. The
//! receiving side never replaces existing bytes.

#[path = "base64.rs"]
mod base64;
#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;

use crate::host::command::Local;
use crate::host::evidence::Evidence;
use crate::host::fault::{
    DIRECTORY, Fault, Numbered, Result, VALUE, decode, field, file, object, protected, require,
    string, text,
};
use crate::host::{Host, Paths, System, emit, hexadecimal, state};
use crate::json::{Json, python_dump};
use crate::release::assets::{self, Takeover, Workload};
use crate::turn::Disposable;
use crate::turn::store::{self, MAX_AUTHORITIES, MAX_CERTIFICATE, Site, relay_environment};
use rustix::fs::{Mode, OFlags, mkdirat, openat};
use rustix::io::Errno;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, fchown};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The three files the proxy keeps for one hostname, by suffix.
const SUFFIXES: [&str; 3] = ["crt", "key", "json"];
const MAX_BUNDLE: u64 = 4 * MAX_CERTIFICATE as u64 + 4096;
const UNFINISHED: &str = "Inspect and recover the unfinished release before starting another";
const FAILED: &str = "Migration TLS handoff failed; private certificate state was preserved.\n";

/// What the command was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Export,
    Import,
}

/// One issuer storage component, never a path.
fn issuer_name(value: &str) -> Result<&str> {
    let plain = |byte: &u8| byte.is_ascii_alphanumeric();
    let bytes = value.as_bytes();
    let ordinary = (1..=200).contains(&bytes.len())
        && bytes.first().is_some_and(plain)
        && bytes
            .iter()
            .all(|byte| plain(byte) || b"._-".contains(byte));
    require(ordinary, "Invalid certificate issuer directory")?;
    Ok(value)
}

/// The migration's private directory and the one hostname its protected
/// request admits: the origin both hosts share.
pub(super) fn operation(site: &Site<'_>, identity: &str) -> Result<(PathBuf, String)> {
    require(hexadecimal(identity, 32), "Invalid migration identity")?;
    let (root, owner) = (&site.host.paths.root, site.host.owner);
    let migrations = root.join("migrations");
    let directory = migrations.join(identity);
    for path in [root, &migrations, &directory] {
        protected(owner, path, DIRECTORY)?;
    }
    let request = directory.join("request.json");
    protected(owner, &request, file(0o600, 4096))?;
    let value = decode(&std::fs::read(&request)?)?;
    value.object().ok_or(VALUE)?;
    let source = text(field(&value, "sourceOrigin")?)?;
    let unchanged = field(&value, "operationId")?.string() == Some(identity)
        && field(&value, "destinationOrigin")?.string() == Some(source)
        && source.starts_with("https://");
    require(
        unchanged,
        "Certificate transfer requires the exact unchanged migration origin",
    )?;
    let hostname = source.strip_prefix("https://").unwrap_or(source);
    relay_environment(hostname, &"0".repeat(64))?;
    Ok((directory, hostname.to_owned()))
}

/// One hostname's three files, in the order they travel.
pub(super) type Files = Vec<(String, Vec<u8>)>;

/// The newest issuer's subtree for exactly `hostname`, each file read once
/// within its bound.
pub(super) fn snapshot(site: &Site<'_>, hostname: &str) -> Result<(String, Files)> {
    let certificates = site.certificates();
    let root = store::directory(site, &certificates)?;
    let mut newest: Option<(i128, String, Files)> = None;
    for issuer in store::names(&root, MAX_AUTHORITIES, "Too many issuers")? {
        let held = certificates.join(issuer_name(&issuer)?).join(hostname);
        let held = match store::directory(site, &held) {
            Ok(held) => held,
            Err(Fault::Kind("FileNotFoundError")) => continue,
            Err(fault) => return Err(fault),
        };
        let mut files = Files::new();
        let mut modified = 0;
        for suffix in SUFFIXES {
            let name = format!("{hostname}.{suffix}");
            let (data, changed) = store::read(site, &held, &name, suffix == "key")?;
            if suffix == "crt" {
                modified = changed;
            }
            files.push((suffix.to_owned(), data));
        }
        if newest.as_ref().is_none_or(|(seen, ..)| modified > *seen) {
            newest = Some((modified, issuer, files));
        }
    }
    let missing = "No certificate for the unchanged migration hostname";
    let (_, issuer, files) = newest.ok_or_else(|| Fault::Refused(missing.to_owned()))?;
    Ok((issuer, files))
}

/// Validate private snapshots of the pair before anything is exported or
/// written to the proxy's storage.
pub(super) fn validate(
    site: &Site<'_>,
    directory: &Path,
    hostname: &str,
    files: &Files,
) -> Result<()> {
    let mut named: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    named.sort_unstable();
    require(
        named == ["crt", "json", "key"],
        "Unexpected certificate files",
    )?;
    let sized = |(_, data): &(String, Vec<u8>)| (1..=MAX_CERTIFICATE).contains(&data.len());
    require(files.iter().all(sized), "Invalid certificate file size")?;
    let held = |suffix: &str| {
        files
            .iter()
            .find(|(name, _)| name == suffix)
            .map(|(_, data)| data)
    };
    let (certificate, key, metadata) = (held("crt"), held("key"), held("json"));
    decode(metadata.ok_or(VALUE)?)?.object().ok_or(VALUE)?;
    let private = Disposable::new(directory, ".tls-validation-")?;
    let (leaf, secret) = (private.0.join("certificate.pem"), private.0.join("key.pem"));
    state::atomic(&leaf, certificate.ok_or(VALUE)?)?;
    state::atomic(&secret, key.ok_or(VALUE)?)?;
    let mut runner = Numbered(Evidence::new(site.host, private.0.clone()));
    store::validate(&mut runner, hostname, &leaf, &secret)
}

/// Create one durable private file in an opened directory, neither following
/// nor replacing anything that is already there.
fn exclusive(
    site: &Site<'_>,
    directory: &OwnedFd,
    name: &str,
    data: &[u8],
    proxy: bool,
) -> Result<()> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut output = File::from(openat(directory, name, flags, Mode::from_raw_mode(0o600))?);
    if proxy {
        fchown(&output, Some(site.proxy), Some(site.proxy_group))?;
    }
    output.write_all(data)?;
    output.sync_all()?;
    rustix::fs::fsync(directory)?;
    Ok(())
}

/// Export the validated subtree of the hostname as one private envelope,
/// which is never printed.
fn export(site: &Site<'_>, directory: &Path, hostname: &str) -> Result<()> {
    let (issuer, files) = snapshot(site, hostname)?;
    validate(site, directory, hostname, &files)?;
    let encoded = files
        .iter()
        .map(|(name, data)| (name.as_str(), Json::String(base64::encode(data))));
    let bundle = object(vec![
        ("domain", string(hostname)),
        ("issuer", string(&issuer)),
        ("files", object(encoded.collect())),
    ]);
    let descriptor = store::directory(site, directory)?;
    let envelope = python_dump(&bundle, false);
    exclusive(site, &descriptor, "tls.json", envelope.as_bytes(), false)
}

/// The envelope's issuer and files: exact fields, a bounded size, and no
/// name that is a path.
fn bundle(site: &Site<'_>, directory: &Path, hostname: &str) -> Result<(String, Files)> {
    let path = directory.join("tls.json");
    protected(site.host.owner, &path, file(0o600, MAX_BUNDLE))?;
    let encoded = std::fs::read(&path)?;
    let bounded = encoded.len() as u64 <= MAX_BUNDLE;
    require(bounded, "Oversized certificate bundle")?;
    let bundle = decode(&encoded)?;
    let fields = bundle.object().ok_or(VALUE)?;
    let mut named: Vec<&str> = fields.keys().map(String::as_str).collect();
    named.sort_unstable();
    let ours = named == ["domain", "files", "issuer"]
        && fields.get("domain").and_then(Json::string) == Some(hostname);
    require(ours, "Certificate bundle identity differs")?;
    let issuer = issuer_name(text(field(&bundle, "issuer")?)?)?.to_owned();
    let Json::Object(records, order) = field(&bundle, "files")? else {
        return Err(VALUE);
    };
    let mut named: Vec<&str> = order.iter().map(String::as_str).collect();
    named.sort_unstable();
    require(
        named == ["crt", "json", "key"],
        "Unexpected certificate files",
    )?;
    let mut files = Files::new();
    for name in order {
        let value = text(records.get(name).ok_or(VALUE)?)?;
        files.push((name.clone(), base64::decode(value)?));
    }
    validate(site, directory, hostname, &files)?;
    Ok((issuer, files))
}

/// Create `name` in an opened directory unless it exists, and give a new one
/// to the proxy. Returns the directory opened without following a link,
/// and whether it was created.
fn ensured(site: &Site<'_>, parent: &OwnedFd, name: &str) -> Result<(OwnedFd, bool)> {
    let created = match mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => true,
        Err(Errno::EXIST) => false,
        Err(error) => return Err(Fault::from(error)),
    };
    let child = store::child(parent, name)?;
    if created {
        fchown(&child, Some(site.proxy), Some(site.proxy_group))?;
    }
    Ok((child, created))
}

/// Install only new bytes, or accept a complete subtree for the hostname
/// that already holds exactly these.
fn import(site: &Site<'_>, directory: &Path, hostname: &str) -> Result<()> {
    let (issuer, files) = bundle(site, directory, hostname)?;
    let mut parent = store::directory(site, &site.host.paths.root)?;
    for name in ["caddy-data", "caddy", "certificates", issuer.as_str()] {
        let (child, _) = ensured(site, &parent, name)?;
        let safe = store::kept(site, &child)?;
        require(safe, "Unsafe destination certificate directory")?;
        parent = child;
    }
    let (held, created) = ensured(site, &parent, hostname)?;
    let metadata = File::from(held.try_clone()?).metadata()?;
    let ours = metadata.uid() == site.proxy && metadata.mode() & 0o022 == 0;
    require(ours, "Unsafe destination hostname directory")?;
    if created {
        for (suffix, data) in &files {
            exclusive(site, &held, &format!("{hostname}.{suffix}"), data, true)?;
        }
    } else {
        let mut present = store::names(&held, SUFFIXES.len(), "Unexpected hostname files")?;
        present.sort_unstable();
        let mut expected = SUFFIXES.map(|suffix| format!("{hostname}.{suffix}"));
        expected.sort_unstable();
        require(present == expected, "Unexpected existing hostname files")?;
        for (suffix, data) in &files {
            let name = format!("{hostname}.{suffix}");
            let (actual, _) = store::read(site, &held, &name, suffix == "key")?;
            let same = actual == *data;
            require(same, "Existing destination certificate differs")?;
        }
    }
    rustix::fs::fsync(&parent)?;
    Ok(())
}

/// Run one action: admit the caller, take the workload lock through `lock`
/// and release it whatever happens under it.
fn execute(
    site: &Site<'_>,
    action: Action,
    identity: &str,
    privileged: bool,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Result<()> {
    require(privileged, "Run as root on the selected migration host")?;
    let workload = match lock(Takeover::Nothing) {
        Ok(workload) => workload,
        Err(failure) if failure.busy() => return Err(Fault::Kind("BlockingIOError")),
        Err(_) => return Err(Fault::Refused(UNFINISHED.to_owned())),
    };
    let outcome = operation(site, identity).and_then(|(directory, hostname)| match action {
        Action::Export => export(site, &directory, &hostname),
        Action::Import => import(site, &directory, &hostname),
    });
    let released = workload.finish().map_err(|_| Fault::Kind("OSError"));
    outcome.and(released)
}

/// `tls export|import OPERATION`, or nothing for any other command line.
fn parse(arguments: &[OsString]) -> Option<(Action, &str)> {
    let [family, action, identity] = arguments else {
        return None;
    };
    let action = match (family.to_str()?, action.to_str()?) {
        ("tls", "export") => Action::Export,
        ("tls", "import") => Action::Import,
        _ => return None,
    };
    let identity = identity.to_str()?;
    (!identity.starts_with('-')).then_some((action, identity))
}

/// Run `migrate tls export|import OPERATION` on the selected migration host.
/// A syntax error returns 2 before any effect; any failure is one fixed line.
pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    let Some((action, identity)) = parse(arguments) else {
        return ExitCode::from(2);
    };
    rustix::process::umask(Mode::from_raw_mode(0o077));
    let clock = System::new();
    let host = Host {
        commands: &Local,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let site = Site {
        host: &host,
        proxy: 10001,
        proxy_group: 10001,
        relay_group: 10002,
        rearm: &|| {},
    };
    let privileged = rustix::process::geteuid().is_root();
    match execute(&site, action, identity, privileged, Workload::acquire_for) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            let _ = emit(std::io::stderr(), FAILED);
            ExitCode::FAILURE
        }
    }
}
