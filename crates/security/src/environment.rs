//! The hash-locked Python environment of the scanners that are Python
//! packages.
//!
//! The environment is named by the SHA-256 of its lock, built in a staging
//! directory beside that name and published by a link once every package is
//! installed with its hash, the packages agree with each other and a receipt
//! holds the lock's digest. Whatever is at the name without a receipt is the
//! remains of an interrupted build and is built again.

use crate::{
    Fault,
    context::{Context, Outcome, Run},
    digest, files,
    record::MAX_DOCUMENT,
    require,
};
use std::{
    ffi::OsStr,
    fs::DirBuilder,
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// The lock of the environment, below the root.
pub(crate) const LOCK: &str = "security/requirements.txt";
/// Where the environments are, below the root. Outside Cargo's target
/// directory, whose cache is saved and restored without the receipts.
const CACHE: &str = ".cache/security-environments";
/// How the name of a staging directory starts.
const STAGING: &str = ".install.";
/// The oldest interpreter the scanners support.
const OLDEST: (u64, u64) = (3, 12);
/// How long the interpreter has to say its version.
const ASKED: Duration = Duration::from_secs(30);
/// How long creating the environment or checking its packages may take.
const BUILT: Duration = Duration::from_secs(300);
/// How long installing the packages may take.
const INSTALLED: Duration = Duration::from_secs(900);
/// How the packages are installed: every one by its hash, none built here.
pub(crate) const INSTALL: [&str; 8] = [
    "-m",
    "pip",
    "--isolated",
    "install",
    "--disable-pip-version-check",
    "--require-hashes",
    "--only-binary=:all:",
    "--requirement",
];

/// A staging directory of this build, removed again unless it is published.
struct Staging(PathBuf, bool);

impl Staging {
    fn create(cache: &Path) -> Outcome<Self> {
        let process = std::process::id();
        let moment = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        for attempt in 0..64 {
            let path = cache.join(format!("{STAGING}{process:x}-{moment:08x}-{attempt}"));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path, false)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(files::unusable(&error).into()),
            }
        }
        Err(Fault("tool_entry_unavailable").into())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Require an interpreter of the oldest supported version or a newer one.
fn supported(context: &mut Context, python3: &Path) -> Outcome<()> {
    let how = Run {
        within: ASKED,
        ..Run::default()
    };
    let asked = [python3.as_os_str(), OsStr::new("--version")];
    let (_, said) = context.run("scanner-python-version", &asked, &how)?;
    let version = std::str::from_utf8(&said).ok().and_then(|said| {
        let mut numbers = said.strip_prefix("Python ")?.split('.');
        let major = numbers.next()?.parse::<u64>().ok()?;
        let minor = numbers.next()?;
        let digits = minor.find(|digit: char| !digit.is_ascii_digit());
        let minor = minor[..digits.unwrap_or(minor.len())].parse::<u64>().ok()?;
        Some((major, minor))
    });
    require(
        version.is_some_and(|version| version >= OLDEST),
        "scanner_environment_python",
    )?;
    Ok(())
}

/// Remove whatever is at the environment's name, and the staging directory a
/// link there names. Only an entry of the cache itself whose name starts as
/// a staging directory's does is removed for a link: the link's text is
/// compared and never resolved, so nothing it leads to through a parent
/// step or below another staging directory is touched.
fn discard(context: &mut Context, cache: &Path, environment: &Path) -> Outcome<()> {
    let Ok(metadata) = environment.symlink_metadata() else {
        return Ok(());
    };
    context.note(vec![
        ("name", "scanner-environment-rebuilt".into()),
        ("exitStatus", 0.into()),
    ]);
    let removed = |path: &Path| {
        let gone = match path.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(_) => Ok(()),
        };
        gone.map_err(|error| files::unusable(&error))
    };
    if !metadata.file_type().is_symlink() {
        return Ok(removed(environment)?);
    }
    let target = environment
        .read_link()
        .map_err(|error| files::unusable(&error))?;
    removed(environment)?;
    let text = target.as_os_str().as_bytes();
    let staged = text
        .iter()
        .rposition(|byte| *byte == b'/')
        .is_some_and(|cut| {
            &text[..cut] == cache.as_os_str().as_bytes()
                && text[cut + 1..].starts_with(STAGING.as_bytes())
        });
    if staged {
        removed(&target)?;
    }
    Ok(())
}

/// Build the environment in a staging directory and publish it.
fn build(
    context: &mut Context,
    python3: &Path,
    cache: &Path,
    environment: &Path,
    wanted: &str,
) -> Outcome<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(cache)
        .map_err(|error| files::unusable(&error))?;
    let mut staging = Staging::create(cache)?;
    let slow = Run {
        within: BUILT,
        ..Run::default()
    };
    let created = [
        python3.as_os_str(),
        OsStr::new("-m"),
        OsStr::new("venv"),
        staging.0.as_os_str(),
    ];
    context.run("scanner-environment-venv", &created, &slow)?;
    let python = staging.0.join("bin/python");
    let mut installed = vec![python.as_os_str()];
    installed.extend(INSTALL.map(OsStr::new));
    installed.push(OsStr::new(LOCK));
    let how = Run {
        within: INSTALLED,
        ..Run::default()
    };
    context.run("scanner-environment-install", &installed, &how)?;
    let checked = ["-m", "pip", "--isolated", "check"].map(OsStr::new);
    let mut agreed = vec![python.as_os_str()];
    agreed.extend(checked);
    context.run("scanner-environment-check", &agreed, &slow)?;
    let receipt = format!("{wanted}\n");
    files::write_private(&staging.0.join("ready"), receipt.as_bytes(), 0o600)?;
    std::os::unix::fs::symlink(&staging.0, environment).map_err(|error| files::unusable(&error))?;
    staging.1 = true;
    Ok(())
}

/// The interpreter of the environment for the lock below the invocation's
/// root, built first with `python3` when there is none with a receipt. A
/// receipt of another lock is refused.
pub fn interpreter(context: &mut Context, python3: &Path) -> Outcome<PathBuf> {
    supported(context, python3)?;
    let root = context.root().to_path_buf();
    let lock = files::bounded(&root.join(LOCK), MAX_DOCUMENT, false)?;
    let wanted = digest(&lock);
    let cache = root.join(CACHE);
    let environment = cache.join(&wanted);
    if !environment.join("ready").is_file() {
        discard(context, &cache, &environment)?;
        build(context, python3, &cache, &environment, &wanted)?;
    }
    let receipt = files::bounded(&environment.join("ready"), 1024, false).unwrap_or_default();
    let mut held = &receipt[..];
    while let [rest @ .., b'\n'] = held {
        held = rest;
    }
    require(held == wanted.as_bytes(), "scanner_environment_receipt")?;
    Ok(environment.join("bin/python"))
}

#[cfg(test)]
mod tests;
