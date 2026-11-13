//! Publication and verified reuse of one installation directory.

use super::{
    FAST_TOOLS, Fault, Limits, Result, archive, digest, files,
    lock::{self, Lock},
    require,
    transport::{self, Transport},
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

/// Where an installation's inputs are and what it is for.
#[derive(Clone, Copy, Debug)]
pub struct Site<'a> {
    /// The lock file.
    pub lock: &'a Path,
    /// The parent of the default directories, one per lock and platform.
    pub cache: &'a Path,
    /// One of the lock's platform names.
    pub platform: &'a str,
    /// The bounds archives and installed files are held to.
    pub limits: Limits,
}

impl Site<'_> {
    /// Outside Cargo's own caches, whose cleanup would remove the tools.
    fn directory(&self, lock: &Lock, given: Option<&Path>) -> PathBuf {
        given.map_or_else(
            || self.cache.join(&lock.digest).join(self.platform),
            Path::to_path_buf,
        )
    }
}

/// Whether nothing, not even a dangling link, is at `path`.
fn absent(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(_) => Ok(false),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(files::unusable(&error)),
    }
}

fn directories(mode: u32) -> DirBuilder {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(mode);
    builder
}

/// A directory of this installation's own beside its destination, removed
/// again unless it was published.
struct Staging(PathBuf);

impl Staging {
    fn create(parent: &Path) -> Result<Self> {
        let process = std::process::id();
        let moment = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        for attempt in 0..64 {
            let path = parent.join(format!(".security-tools-{process}-{moment:08x}-{attempt}"));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(files::unusable(&error)),
            }
        }
        Err(Fault("tool_entry_unavailable"))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.0.symlink_metadata().is_ok() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// A JSON string as the receipt has always spelled it: ASCII only.
fn quoted(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(character);
            }
            ' '..='~' => quoted.push(character),
            _ => {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    let _ = write!(quoted, "\\u{unit:04x}");
                }
            }
        }
    }
    quoted.push('"');
    quoted
}

/// One installed tool: its name, its version, its archive and its files.
struct Installed<'a> {
    name: &'a str,
    version: &'a str,
    archive: &'a str,
    files: Vec<(String, String)>,
}

fn receipt(digest: &str, platform: &str, installed: &[Installed<'_>]) -> String {
    let mut text = String::from("{\n  \"schemaVersion\": 1,\n");
    let _ = writeln!(text, "  \"lockSha256\": {},", quoted(digest));
    let _ = writeln!(text, "  \"platform\": {},", quoted(platform));
    text.push_str("  \"tools\": {");
    for (index, tool) in installed.iter().enumerate() {
        text.push_str(if index == 0 { "\n" } else { ",\n" });
        let _ = writeln!(text, "    {}: {{", quoted(tool.name));
        let _ = writeln!(text, "      \"version\": {},", quoted(tool.version));
        let _ = writeln!(text, "      \"archiveSha256\": {},", quoted(tool.archive));
        text.push_str("      \"files\": {");
        for (index, (destination, hash)) in tool.files.iter().enumerate() {
            text.push_str(if index == 0 { "\n" } else { ",\n" });
            let _ = write!(text, "        {}: {}", quoted(destination), quoted(hash));
        }
        text.push_str("\n      }\n    }");
    }
    text.push_str("\n  }\n}\n");
    text
}

/// Install the named tools, or the fast gate's when none is named, as one
/// complete directory, or verify the directory that is already there. The
/// directory's path is the answer either way.
pub fn install(
    site: &Site<'_>,
    names: &[String],
    directory: Option<&Path>,
    transport: &mut dyn Transport,
) -> Result<PathBuf> {
    let lock = Lock::load(site.lock)?;
    let mut selected: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    if selected.is_empty() {
        selected = FAST_TOOLS.into_iter().collect();
    }
    require(
        selected.iter().all(|name| lock.tools.contains_key(*name)),
        "unknown_tool",
    )?;
    require(
        selected
            .iter()
            .all(|name| lock.tools[*name].platforms.contains_key(site.platform)),
        "unsupported_tool_platform",
    )?;
    let root = site.directory(&lock, directory);
    if !absent(&root)? {
        for name in &selected {
            verified(site, &lock, name, &root)?;
        }
        return Ok(root);
    }
    let parent = match root.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => &root,
    };
    directories(0o755)
        .create(parent)
        .map_err(|error| files::unusable(&error))?;
    files::private_directory(parent)?;
    let staging = Staging::create(parent)?;
    let bin = staging.0.join("bin");
    directories(0o700)
        .create(&bin)
        .map_err(|error| files::unusable(&error))?;
    let mut installed = Vec::new();
    for name in &selected {
        let tool = &lock.tools[*name];
        let asset = &tool.platforms[site.platform];
        let archive = transport::download(asset, &site.limits, transport)?;
        let mut hashes = Vec::new();
        for (destination, content) in archive::selected(asset, &archive, &site.limits)? {
            let path = bin.join(&destination);
            if let Some(above) = path.parent() {
                directories(0o700)
                    .create(above)
                    .map_err(|error| files::unusable(&error))?;
            }
            files::write_private(&path, &content, 0o700)?;
            hashes.push((destination, digest(&content)));
        }
        installed.push(Installed {
            name,
            version: &tool.version,
            archive: &asset.sha256,
            files: hashes,
        });
    }
    let receipt = receipt(&lock.digest, site.platform, &installed);
    files::write_private(&staging.0.join("receipt.json"), receipt.as_bytes(), 0o600)?;
    require(absent(&root)?, "tool_directory_appeared")?;
    std::fs::rename(&staging.0, &root).map_err(|error| files::unusable(&error))?;
    Ok(root)
}

/// The path of an installed tool's executable, once the receipt is this
/// lock's and every one of the tool's files has the bytes it records.
pub fn path(site: &Site<'_>, name: &str, directory: Option<&Path>) -> Result<PathBuf> {
    let lock = Lock::load(site.lock)?;
    require(lock.tools.contains_key(name), "unknown_tool")?;
    let root = site.directory(&lock, directory);
    verified(site, &lock, name, &root)
}

fn verified(site: &Site<'_>, lock: &Lock, name: &str, root: &Path) -> Result<PathBuf> {
    let tool = &lock.tools[name];
    let asset = tool
        .platforms
        .get(site.platform)
        .ok_or(Fault("unsupported_tool_platform"))?;
    let bin = root.join("bin");
    files::private_directory(root)?;
    files::private_directory(&bin)?;
    let content = files::bounded(&root.join("receipt.json"), lock::MAX_DOCUMENT, true)?;
    let value = lock::document(&content)?;
    let receipt = lock::record(
        &value,
        &["schemaVersion", "lockSha256", "platform", "tools"],
    )?;
    require(
        receipt["schemaVersion"].as_u64() == Some(1)
            && receipt["lockSha256"].as_str() == Some(&lock.digest),
        "tool_receipt_lock",
    )?;
    require(
        receipt["platform"].as_str() == Some(site.platform),
        "tool_receipt_platform",
    )?;
    let Value::Object(installed) = &receipt["tools"] else {
        return Err(Fault("tool_receipt_tools"));
    };
    let item = installed.get(name).ok_or(Fault("tool_not_installed"))?;
    let item = lock::record(item, &["version", "archiveSha256", "files"])?;
    require(
        item["version"].as_str() == Some(&tool.version)
            && item["archiveSha256"].as_str() == Some(&asset.sha256),
        "tool_receipt_identity",
    )?;
    let destinations: Vec<&str> = asset.files.values().map(String::as_str).collect();
    for (destination, expected) in lock::record(&item["files"], &destinations)? {
        let mut above = Path::new(destination).parent();
        while let Some(directory) = above.filter(|path| !path.as_os_str().is_empty()) {
            files::private_directory(&bin.join(directory))?;
            above = directory.parent();
        }
        let content = files::bounded(&bin.join(destination), site.limits.executable, true)?;
        require(
            expected.as_str() == Some(&digest(&content)),
            "tool_executable_digest",
        )?;
    }
    let path = bin.join(&tool.executable);
    require(
        rustix::fs::access(&path, rustix::fs::Access::EXEC_OK).is_ok(),
        "tool_not_executable",
    )?;
    Ok(path)
}
