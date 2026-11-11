//! The pinned local `act` runner with one shared concurrency limit for jobs.
//!
//! Preparation authenticates the source archive and the maintained patch,
//! tests the limiter and reuses only an intact earlier build. The patch is
//! checked before any compiler or network use. The whole source tree is
//! validated before one member is written, and a finished build appears in
//! the cache by a single rename, together with its integrity receipt.

use super::process::Runner;
use super::{evidence, io_failure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use simplestchat_toolkit::tooling::{Failure, Result};
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PIN: &str = "build/ci-local-act.json";
const PATCH: &str = "build/ci-local-act.patch";
const MAX_PATH: usize = 4096;
const COMMAND_LIMIT: Duration = Duration::from_secs(300);

/// Bounds on the expanded source archive.
pub(super) struct Limits {
    pub bytes: u64,
    pub member: u64,
    pub members: usize,
}

pub(super) const LIMITS: Limits = Limits {
    bytes: 256 * 1024 * 1024,
    member: 16 * 1024 * 1024,
    members: 10_000,
};

fn refused(code: &str) -> Failure {
    Failure::new(1, format!("Local act preparation failed: {code}"))
}

fn require(condition: bool, code: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(refused(code))
    }
}

/// The SHA-256 of an existing file, read in bounded pieces.
pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(io_failure)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(io_failure)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let bytes = hash.finalize();
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A regular file of at most `maximum` bytes.
fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(io_failure)?;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    require(bytes.len() as u64 <= maximum, "tool_file_size")?;
    Ok(bytes)
}

/// Expose a complete executable and its integrity receipt with one rename.
pub(super) fn publish(staging: &Path, target: &Path, key: &str) -> Result<()> {
    let binary = digest(&staging.join("act"))?;
    let receipt = format!("{{\"inputSha256\": \"{key}\", \"binarySha256\": \"{binary}\"}}\n");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(staging.join("receipt.json"))
        .map_err(io_failure)?;
    file.write_all(receipt.as_bytes()).map_err(io_failure)?;
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(io_failure)?;
    drop(file);
    fs::rename(staging, target).map_err(io_failure)
}

/// One validated member of the source archive.
enum Member {
    Directory,
    File {
        mode: u32,
        start: usize,
        size: usize,
    },
    Link(String),
}

/// The parent directories of a member, nearest last, without the member.
fn parents(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('/').map(|(index, _)| &name[..index])
}

/// `posixpath.normpath` of a link target relative to its member's directory.
fn link_target(name: &str, link: &str) -> String {
    let directory = name.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut parts = Vec::new();
    for part in directory.split('/').chain(link.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Validate the entire bounded source tree before any member is written.
fn source_members(
    expanded: &[u8],
    root: &str,
    limits: &Limits,
) -> Result<BTreeMap<String, Member>> {
    let unreadable = |_| refused("local_act_source_archive");
    let mut archive = tar::Archive::new(std::io::Cursor::new(expanded));
    let mut members = BTreeMap::new();
    for entry in archive.entries().map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            // Archive-wide metadata, not a member of the tree.
            continue;
        }
        let path = entry.path_bytes();
        let name = std::str::from_utf8(&path).map_err(|_| refused("local_act_source_path"))?;
        let name = if kind.is_dir() {
            name.trim_end_matches('/')
        } else {
            name
        };
        let mut parts = name.split('/');
        require(
            members.len() < limits.members
                && !name.is_empty()
                && name.len() <= MAX_PATH
                && parts.next() == Some(root)
                && name
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
                && !name.contains('\\')
                && name.chars().all(|character| character >= ' '),
            "local_act_source_path",
        )?;
        require(!members.contains_key(name), "local_act_source_duplicate")?;
        require(
            kind.is_file() || kind.is_dir() || kind.is_symlink(),
            "local_act_source_type",
        )?;
        let size = entry.size();
        require(
            size <= limits.member && (kind.is_file() || size == 0),
            "local_act_source_size",
        )?;
        let member = if kind.is_dir() {
            Member::Directory
        } else if kind.is_symlink() {
            let link = entry.link_name_bytes().unwrap_or_default();
            let link = std::str::from_utf8(&link).map_err(|_| refused("local_act_source_link"))?;
            Member::Link(link.to_owned())
        } else {
            let oversized = |_| refused("local_act_source_size");
            let start = usize::try_from(entry.raw_file_position()).map_err(oversized)?;
            let size = usize::try_from(size).map_err(oversized)?;
            let end = start.checked_add(size);
            require(
                end.is_some_and(|end| end <= expanded.len()),
                "local_act_source_truncated",
            )?;
            let mode = entry.header().mode().map_err(unreadable)?;
            Member::File { mode, start, size }
        };
        members.insert(name.to_owned(), member);
    }
    require(
        matches!(members.get(root), Some(Member::Directory)),
        "local_act_source_root",
    )?;
    for (name, member) in &members {
        for parent in parents(name) {
            require(
                matches!(members.get(parent), Some(Member::Directory)),
                "local_act_source_parent",
            )?;
        }
        if let Member::Link(link) = member {
            let target = link_target(name, link);
            require(
                !link.is_empty()
                    && link.len() <= MAX_PATH
                    && !link.starts_with('/')
                    && !link.contains('\\')
                    && matches!(members.get(&target), Some(Member::File { .. })),
                "local_act_source_link",
            )?;
        }
    }
    Ok(members)
}

/// Write only validated files and directories and internal symbolic links to
/// regular files, and return the source root inside `workspace`.
pub(super) fn extract_source(
    archive: &Path,
    workspace: &Path,
    revision: &str,
    limits: &Limits,
) -> Result<PathBuf> {
    let hexadecimal = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    require(
        revision.len() == 40 && revision.bytes().all(hexadecimal),
        "local_act_source_revision",
    )?;
    let compressed = fs::File::open(archive).map_err(io_failure)?;
    let mut expanded = Vec::new();
    flate2::read::GzDecoder::new(compressed)
        .take(limits.bytes + 1)
        .read_to_end(&mut expanded)
        .map_err(|_| refused("local_act_source_archive"))?;
    require(
        expanded.len() as u64 <= limits.bytes,
        "local_act_source_expansion",
    )?;
    let root = format!("act-{revision}");
    let members = source_members(&expanded, &root, limits)?;
    let mut directories = members
        .iter()
        .filter(|(_, member)| matches!(member, Member::Directory))
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    directories.sort_by_key(|name| name.matches('/').count());
    for name in directories {
        DirBuilder::new()
            .mode(0o755)
            .create(workspace.join(name))
            .map_err(io_failure)?;
    }
    for (name, member) in &members {
        let Member::File { mode, start, size } = member else {
            continue;
        };
        let target = workspace.join(name);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(io_failure)?;
        output
            .write_all(&expanded[*start..*start + *size])
            .map_err(io_failure)?;
        // Only the owner's execute bit decides executability; set-id and
        // group or world write bits never survive.
        let mut mode = mode & 0o755;
        if mode & 0o100 == 0 {
            mode &= !0o111;
        }
        output
            .set_permissions(Permissions::from_mode(mode | 0o600))
            .map_err(io_failure)?;
    }
    for (name, member) in &members {
        if let Member::Link(link) = member {
            symlink(link, workspace.join(name)).map_err(io_failure)?;
        }
    }
    Ok(workspace.join(root))
}

/// A build workspace inside the cache, removed when preparation ends.
struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The Go settings every compiler command runs under.
fn go_environment(runner: &Runner) -> simplestchat_toolkit::tooling::Environment {
    let mut environment = runner.environment.clone();
    for (name, value) in [
        ("GOENV", "off"),
        ("GOTOOLCHAIN", "local"),
        ("GOFLAGS", ""),
        ("CGO_ENABLED", "0"),
        ("GOWORK", "off"),
        ("GOPRIVATE", ""),
        ("GONOSUMDB", ""),
        ("GOINSECURE", ""),
        ("GOSUMDB", "sum.golang.org"),
    ] {
        environment.insert(name.into(), value.into());
    }
    environment
}

/// Run one bounded build command in `directory` and pass its output on.
async fn build_step(runner: &mut Runner, directory: &Path, argv: &[&str]) -> Result<()> {
    let checkout = std::mem::replace(&mut runner.root, directory.to_path_buf());
    let output = runner.capture(argv[0], &argv[1..]).await;
    runner.root = checkout;
    let output = output.map_err(|failure| {
        if super::engine::interrupted(&failure) {
            failure
        } else {
            refused("local_act_build_command_failed")
        }
    })?;
    eprint!("{}", String::from_utf8_lossy(&output));
    Ok(())
}

/// Authenticate source and patch, test the limiter, and return the path of
/// the authenticated executable. No global tool is changed.
pub(super) async fn prepare(runner: &mut Runner) -> Result<PathBuf> {
    let root = runner.root.clone();
    let pin_bytes = bounded(&root.join(PIN), 4096)?;
    let pin: Value = serde_json::from_slice(&pin_bytes).map_err(|_| refused("local_act_pin"))?;
    let text = |field: &str| pin[field].as_str().ok_or_else(|| refused("local_act_pin"));
    let patch = root.join(PATCH);
    require(
        digest(&patch)? == text("patchSha256")?,
        "local_act_patch_digest",
    )?;
    let go = go_environment(runner);
    let checkout_environment = std::mem::replace(&mut runner.environment, go);
    let limit = runner.limit.replace(COMMAND_LIMIT);
    let built = build(runner, &root, &pin_bytes, &pin, &patch).await;
    runner.environment = checkout_environment;
    runner.limit = limit;
    built
}

async fn build(
    runner: &mut Runner,
    root: &Path,
    pin_bytes: &[u8],
    pin: &Value,
    patch: &Path,
) -> Result<PathBuf> {
    let text = |field: &str| pin[field].as_str().ok_or_else(|| refused("local_act_pin"));
    let identity = runner
        .capture("go", ["env", "GOVERSION", "GOOS", "GOARCH"])
        .await?;
    let mut hash = Sha256::new();
    hash.update(pin_bytes);
    hash.update(&identity);
    let key = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let cache = root.join(".cache/ci-act");
    let target = cache.join(&key);
    let binary = target.join("act");
    if target.exists() {
        let recorded: Value = serde_json::from_slice(&bounded(&target.join("receipt.json"), 4096)?)
            .map_err(|_| refused("local_act_cache_integrity"))?;
        let executable = fs::metadata(&binary)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
        let intact = json!({"inputSha256": key, "binarySha256": digest(&binary)?});
        require(
            executable && recorded == intact,
            "local_act_cache_integrity",
        )?;
        return Ok(binary);
    }
    fs::create_dir_all(&cache).map_err(io_failure)?;
    for name in ["go-build", "go-modules", "go-path", "tmp"] {
        match fs::create_dir(cache.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(io_failure(error));
            }
            _ => {}
        }
    }
    for (name, directory) in [
        ("GOCACHE", "go-build"),
        ("GOMODCACHE", "go-modules"),
        ("GOPATH", "go-path"),
        ("GOTMPDIR", "tmp"),
        ("TMPDIR", "tmp"),
    ] {
        runner
            .environment
            .insert(name.into(), cache.join(directory).into_os_string());
    }
    let workspace = Workspace(cache.join(format!("build-{}", evidence::random_id()?)));
    DirBuilder::new()
        .mode(0o700)
        .create(&workspace.0)
        .map_err(io_failure)?;
    let archive = workspace.0.join("source.tar.gz");
    let revision = text("revision")?;
    let expected = pin["archiveBytes"]
        .as_u64()
        .ok_or_else(|| refused("local_act_pin"))?;
    let url = format!("https://codeload.github.com/nektos/act/tar.gz/{revision}");
    let size = expected.to_string();
    let download = [
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-time",
        "120",
        "--max-filesize",
        size.as_str(),
        "--output",
        archive.to_str().ok_or_else(|| refused("local_act_cache"))?,
        url.as_str(),
    ];
    runner.capture("curl", download).await.map_err(|failure| {
        if super::engine::interrupted(&failure) {
            failure
        } else {
            refused("local_act_archive_download")
        }
    })?;
    let downloaded = fs::metadata(&archive).map_err(io_failure)?.len();
    require(downloaded == expected, "local_act_archive_size")?;
    require(
        digest(&archive)? == text("archiveSha256")?,
        "local_act_archive_digest",
    )?;
    let source = extract_source(&archive, &workspace.0, revision, &LIMITS)?;
    let published = workspace.0.join("published");
    DirBuilder::new()
        .mode(0o700)
        .create(&published)
        .map_err(io_failure)?;
    let version = format!("-X main.version={}-local-job-limit", text("version")?);
    let output = published.join("act");
    let patch = patch.to_str().ok_or_else(|| refused("local_act_cache"))?;
    let output = output.to_str().ok_or_else(|| refused("local_act_cache"))?;
    let limiter = "^TestLocalGlobalLeafLimit";
    let commands: [&[&str]; 3] = [
        &["patch", "--batch", "--forward", "-p1", "-i", patch],
        &[
            "go",
            "test",
            "-mod=readonly",
            "./pkg/runner",
            "-run",
            limiter,
            "-count=1",
        ],
        &[
            "go",
            "build",
            "-mod=readonly",
            "-trimpath",
            "-ldflags",
            version.as_str(),
            "-o",
            output,
            ".",
        ],
    ];
    for command in commands {
        build_step(runner, &source, command).await?;
    }
    publish(&published, &target, &key)?;
    Ok(binary)
}

#[cfg(test)]
#[path = "act_tests.rs"]
mod tests;
