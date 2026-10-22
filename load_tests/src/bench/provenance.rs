//! Scoped source snapshots and independently frozen executable hashes.
//!
//! These identities describe inputs, not a reproducible-build attestation.
//! Source symlinks contribute their link text; descriptor traversal refuses
//! symlink ancestors. No import starts Git, a runtime or a workload.

use super::process::{Command, Host, Sink};
use anyhow::{Result, ensure};
use nix::fcntl::{AtFlags, OFlag, open, openat, readlinkat};
use nix::sys::stat::{Mode, SFlag, fstatat};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const TRACKED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "src",
    "vendor",
    "build/pip-constraints.txt",
];
const SCOPE: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "src",
    "vendor",
    "build/pip-constraints.txt",
    "build.rs",
    ".cargo/config",
    ".cargo/config.toml",
];
const MIB: u64 = 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_TOTAL: u64 = 2 * 1024 * MIB;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTreeIdentity {
    pub source_tree_sha256: String,
    pub source_tree_files: usize,
    pub source_tree_missing_paths: Vec<String>,
    pub source_tree_scope: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerIdentity {
    pub root: PathBuf,
    pub binary: PathBuf,
    pub revision: String,
    pub tracked_diff_sha256: String,
    #[serde(flatten)]
    pub source_tree: SourceTreeIdentity,
    pub cargo_lock_sha256: String,
    pub binary_sha256: String,
    pub binary_bytes: u64,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn stamp(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.permissions().mode(),
    )
}

fn hash_opened(mut file: File, maximum: u64) -> Result<(String, u64)> {
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= maximum,
        "Build input type or size refused"
    );
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    let mut count = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        ensure!(count <= maximum, "Build input grew beyond its bound");
        digest.update(&buffer[..read]);
    }
    ensure!(
        count == before.len() && stamp(&file.metadata()?) == stamp(&before),
        "Build input changed while hashing"
    );
    nix::unistd::close(file)?;
    Ok((hex(&digest.finalize()), count))
}

/// Stream at most 2 GiB from a regular executable input. As in the reference,
/// an explicitly selected binary symlink identifies its target's bytes.
pub fn hash_file(binary: &Path) -> Result<(String, u64)> {
    let file = File::from(open(
        binary,
        OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    hash_opened(file, MAX_TOTAL)
}

/// Recheck a frozen executable immediately before its owned invocation. A
/// changed, unreadable or oversized input fails without exposing its contents.
pub fn verify_executable(binary: &Path, expected_sha256: &str, role: &str) -> Result<()> {
    ensure!(
        hash_file(binary)?.0 == expected_sha256,
        "{role} binary changed after the comparison started"
    );
    Ok(())
}

/// Every intermediate component is an actual directory below the opened root.
/// The final component is left unopened so symlinks can be hashed as links.
fn parent(root: &Path, relative: &str) -> Result<(OwnedFd, OsString)> {
    let parts: Vec<_> = Path::new(relative).components().collect();
    ensure!(
        !parts.is_empty()
            && parts.len() <= 64
            && parts
                .iter()
                .all(|part| matches!(part, Component::Normal(_))),
        "Invalid build input path"
    );
    let mut directory = open(
        root,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    for part in &parts[..parts.len() - 1] {
        directory = openat(
            &directory,
            part.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
    }
    Ok((
        directory,
        parts.last().expect("nonempty path").as_os_str().into(),
    ))
}

fn source_file(root: &Path, relative: &str, maximum: u64) -> Result<(String, u64, bool)> {
    let (directory, name) = parent(root, relative)?;
    let file = File::from(openat(
        &directory,
        name.as_os_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let executable = file.metadata()?.permissions().mode() & 0o111 != 0;
    let (hash, bytes) = hash_opened(file, maximum)?;
    Ok((hash, bytes, executable))
}

fn generator_files(
    root: &Path,
    relative: &Path,
    files: &mut Vec<String>,
    visited: &mut usize,
) -> Result<()> {
    ensure!(
        relative.components().count() <= 64,
        "Generator source nesting refused"
    );
    let scoped = Path::new("load_tests/src")
        .join(relative)
        .join("source-placeholder");
    let scoped = scoped
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid source path encoding"))?;
    let (directory, _) = parent(root, scoped)?;
    for entry in rustix::fs::Dir::read_from(&directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if [b".".as_slice(), b".."].contains(&name) {
            continue;
        }
        *visited += 1;
        ensure!(*visited <= MAX_FILES, "Too many generator source entries");
        let name = OsStr::from_bytes(name);
        let kind = SFlag::from_bits_truncate(
            fstatat(&directory, name, AtFlags::AT_SYMLINK_NOFOLLOW)?.st_mode,
        );
        let relative = relative.join(name);
        if kind == SFlag::S_IFDIR {
            generator_files(root, &relative, files, visited)?;
        } else if kind == SFlag::S_IFREG && relative.extension() == Some(OsStr::new("rs")) {
            files.push(
                relative
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid source path encoding"))?
                    .into(),
            );
        }
    }
    Ok(())
}

/// Hash path,NUL,contents,NUL for every actual Rust file, in UTF8 byte order.
/// Documentation, links and non-Rust files do not enter the generator identity.
pub fn generator_source_identity(root: &Path) -> Result<String> {
    // Admit both real directory components before enumerating any filenames.
    let _ = parent(root, "load_tests/src/source-placeholder")?;
    let mut files = Vec::new();
    generator_files(root, Path::new(""), &mut files, &mut 0)?;
    files.sort();
    ensure!(!files.is_empty(), "No generator sources");
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    for path in files {
        let (directory, name) = parent(root, &format!("load_tests/src/{path}"))?;
        let file = File::from(openat(
            &directory,
            name.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        let before = file.metadata()?;
        ensure!(
            before.is_file() && before.len() <= 64 * MIB,
            "Generator source type or size refused"
        );
        total += before.len();
        ensure!(total <= MAX_TOTAL, "Generator source total refused");
        digest.update(path.as_bytes());
        digest.update([0]);
        let mut file = file;
        let mut count = 0_u64;
        let mut buffer = [0; 65536];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            ensure!(
                count <= before.len(),
                "Generator source changed while hashing"
            );
            digest.update(&buffer[..read]);
        }
        ensure!(
            count == before.len() && stamp(&file.metadata()?) == stamp(&before),
            "Generator source changed while hashing"
        );
        nix::unistd::close(file)?;
        digest.update([0]);
    }
    Ok(format!("sha256:{}", hex(&digest.finalize())))
}

async fn git(
    host: &dyn Host,
    environment: &BTreeMap<OsString, OsString>,
    root: &Path,
    arguments: &[&str],
    maximum: u64,
) -> Result<String> {
    let mut child = host.spawn(Command {
        executable: "git".into(),
        arguments: std::iter::once("-C".into())
            .chain(std::iter::once(
                root.as_os_str()
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid checkout path encoding"))?
                    .into(),
            ))
            .chain(arguments.iter().map(|value| (*value).into()))
            .collect(),
        directory: root.into(),
        environment: environment.clone(),
        timeout: Duration::from_secs(20),
        stdout_limit: maximum,
        stderr_limit: MIB,
        input: Vec::new(),
        stdout: Sink::Capture,
        stderr: Sink::Capture,
        cleanup: false,
    })?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(exit) = child.poll().await? {
                return Ok(exit);
            }
            tokio::select! {
                _ = host.cancelled() => anyhow::bail!("Benchmark interrupted"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
    })
    .await;
    let stopped = child.stop(Duration::ZERO).await;
    let exit = result.map_err(|_| anyhow::anyhow!("Source identity command timed out"))??;
    stopped?;
    ensure!(
        exit.code == Some(0) && exit.signal.is_none(),
        "Source identity command failed"
    );
    ensure!(
        child.output().len() as u64 <= maximum,
        "Source identity output bound"
    );
    String::from_utf8(child.output().to_vec())
        .map_err(|_| anyhow::anyhow!("Invalid source identity encoding"))
}

fn paths(value: &str) -> Result<BTreeSet<String>> {
    ensure!(
        value.is_empty() || value.ends_with('\0'),
        "Invalid Git path framing"
    );
    let mut paths = BTreeSet::new();
    for path in value.split_terminator('\0') {
        ensure!(
            !path.is_empty()
                && path.len() <= 4096
                && SCOPE.iter().any(|scope| path == *scope
                    || path
                        .strip_prefix(scope)
                        .is_some_and(|tail| tail.starts_with('/'))),
            "Out-of-scope source input"
        );
        if !matches!(
            Path::new(path).file_name().and_then(OsStr::to_str),
            Some("CLAUDE.md" | "CLAUDE.local.md")
        ) {
            paths.insert(path.to_owned());
        }
        ensure!(paths.len() <= MAX_FILES, "Too many source inputs");
    }
    Ok(paths)
}

/// Include versioned and nonignored untracked inputs in the reference scope.
/// Git path lists are bounded at 8 MiB each; files at 256 MiB, 100000 entries and 2 GiB
/// total. Missing tracked paths remain separate from the stable tree hash.
/// Private instruction basenames are excluded even when tracked. Source files
/// may change between reads; this is an input snapshot, not an atomic checkout.
pub async fn source_tree_identity(
    host: &dyn Host,
    environment: &BTreeMap<OsString, OsString>,
    root: &Path,
) -> Result<SourceTreeIdentity> {
    let working: Vec<_> = [
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
    ]
    .into_iter()
    .chain(SCOPE.iter().copied())
    .collect();
    let committed: Vec<_> = ["ls-tree", "-r", "--name-only", "-z", "HEAD", "--"]
        .into_iter()
        .chain(SCOPE.iter().copied())
        .collect();
    let (working, committed) = tokio::try_join!(
        git(host, environment, root, &working, 8 * MIB),
        git(host, environment, root, &committed, 8 * MIB)
    )?;
    let mut files = paths(&working)?;
    files.extend(paths(&committed)?);
    ensure!(files.len() <= MAX_FILES, "Too many source inputs");
    let mut entries = Vec::<Value>::new();
    let mut missing = Vec::new();
    let mut total = 0_u64;
    for path in files {
        let observed: Result<(Value, u64)> = (|| {
            let (directory, name) = parent(root, &path)?;
            let info = fstatat(&directory, name.as_os_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
            if SFlag::from_bits_truncate(info.st_mode) == SFlag::S_IFLNK {
                let target = readlinkat(&directory, name.as_os_str())?;
                let target = target
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid source link encoding"))?;
                Ok((json!([path, "symlink", digest(target.as_bytes())]), 0))
            } else {
                ensure!(
                    SFlag::from_bits_truncate(info.st_mode) == SFlag::S_IFREG,
                    "Unsupported build input type"
                );
                let (hash, bytes, executable) = source_file(root, &path, 256 * MIB)?;
                Ok((json!([path, "file", executable, hash]), bytes))
            }
        })();
        match observed {
            Ok((entry, bytes)) => {
                total += bytes;
                ensure!(total <= MAX_TOTAL, "Source tree size refused");
                entries.push(entry);
            }
            Err(error)
                if error.downcast_ref::<nix::errno::Errno>()
                    == Some(&nix::errno::Errno::ENOENT) =>
            {
                missing.push(path)
            }
            Err(error) => return Err(error),
        }
    }
    #[derive(Serialize)]
    struct Tree<'a> {
        format: &'static str,
        entries: &'a [Value],
    }
    Ok(SourceTreeIdentity {
        source_tree_sha256: digest(&serde_json::to_vec(&Tree {
            format: "simplestchat-source-tree-v1",
            entries: &entries,
        })?),
        source_tree_files: entries.len(),
        source_tree_missing_paths: missing,
        source_tree_scope: SCOPE.iter().map(|scope| (*scope).into()).collect(),
    })
}

/// ECMAScript trim includes the BOM and excludes Unicode NEXT LINE.
fn js_trim(value: &str) -> &str {
    value.trim_matches(|character: char| {
        character == '\u{feff}' || (character.is_whitespace() && character != '\u{85}')
    })
}

/// Freeze Git/source/binary identities independently. Keep the historical
/// trimmed diff scope for older readers; it does not see untracked inputs.
/// Cargo.lock must be a real scoped file, so its separate content digest cannot
/// follow a source link outside the checkout. Git and owned cleanup failures
/// refuse the identity rather than publish incomplete provenance.
pub async fn identity(
    host: &dyn Host,
    environment: &BTreeMap<OsString, OsString>,
    root: &Path,
    binary: &Path,
) -> Result<ServerIdentity> {
    let revision = git(host, environment, root, &["rev-parse", "HEAD"], MIB).await?;
    let arguments: Vec<_> = ["diff", "HEAD", "--"]
        .into_iter()
        .chain(TRACKED.iter().copied())
        .collect();
    let difference = git(host, environment, root, &arguments, MIB).await?;
    let source_tree = source_tree_identity(host, environment, root).await?;
    let cargo_lock_sha256 = source_file(root, "Cargo.lock", 256 * MIB)?.0;
    let (binary_sha256, binary_bytes) = hash_file(binary)?;
    Ok(ServerIdentity {
        root: root.into(),
        binary: binary.into(),
        revision: js_trim(&revision).into(),
        tracked_diff_sha256: digest(js_trim(&difference).as_bytes()),
        source_tree,
        cargo_lock_sha256,
        binary_sha256,
        binary_bytes,
    })
}

/// Preserve all three independently observed identities in the generator label.
pub fn server_revision_label(server: &ServerIdentity) -> String {
    format!(
        "git:{};source:sha256:{};binary:sha256:{}",
        server.revision, server.source_tree.source_tree_sha256, server.binary_sha256
    )
}

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;
