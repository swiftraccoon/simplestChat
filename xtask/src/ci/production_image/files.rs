//! Fresh ownership intent, bounded immutable IDs and fixed runner file output.

use super::options::{Context, digest};
use crate::tooling::{Failure, Result};
use rustix::fs::{Mode, OFlags};
use std::fs::{DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub(super) struct Owned {
    pub name: String,
    pub directory: PathBuf,
}

pub(super) fn intent(context: &Context) -> Result<Owned> {
    resource_intent(context, false)
}

pub(super) fn verification_intent(context: &Context) -> Result<Owned> {
    resource_intent(context, true)
}

fn resource_intent(context: &Context, verification: bool) -> Result<Owned> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| failure("Production builder identity unavailable."))?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let prefix = if verification {
        "simplestchat-ci-verify"
    } else {
        "simplestchat-ci"
    };
    let name = format!("{prefix}-{token}");
    let directory = context.temporary.join(format!(
        "production-{}-{token}",
        if verification {
            "verification"
        } else {
            "builder"
        }
    ));
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| failure("Fresh production builder directory unavailable."))?;
    let resource = if verification { "container" } else { "builder" };
    let journal = serde_json::to_vec_pretty(&serde_json::json!({resource:name,
        "state":"creation-planned","revision":context.revision}))
    .map_err(|_| failure("Production builder journal failed."))?;
    new_file(&directory.join("ownership.json"), &journal)?;
    Ok(Owned { name, directory })
}

pub(super) fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| failure("Production image artifact creation failed."))?;
    let mut file = File::from(descriptor);
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    let closed = nix::unistd::close(file);
    written.map_err(|_| failure("Production image artifact write failed."))?;
    closed.map_err(|_| failure("Production image artifact close failed."))
}

pub(super) fn image_id(path: &Path) -> Result<String> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure("Production image ID unavailable."))?;
    let file = File::from(descriptor);
    let metadata = file.metadata();
    let mut bytes = Vec::new();
    let read = if metadata
        .as_ref()
        .is_ok_and(|meta| meta.is_file() && meta.len() <= 128)
    {
        (&file).take(129).read_to_end(&mut bytes).map(|_| ())
    } else {
        Err(std::io::Error::other("Invalid ID file"))
    };
    let closed = nix::unistd::close(file);
    read.map_err(|_| failure("Production image ID unavailable."))?;
    closed.map_err(|_| failure("Production image ID close failed."))?;
    if !metadata.is_ok_and(|meta| meta.len() == bytes.len() as u64) {
        return Err(failure("Production image ID changed while reading."));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| failure("Production image ID invalid."))?
        .trim_end_matches('\n');
    if !digest(text) {
        return Err(failure("Production image ID invalid."));
    }
    Ok(text.to_owned())
}

pub(super) fn absent(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(failure(
            "Production image output already exists or is unavailable.",
        )),
    }
}

pub(super) fn directory_or_absent(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => Err(failure(
            "Production layer cache must be an owned directory.",
        )),
    }
}

pub(super) fn cache_from(context: &Context) -> Result<bool> {
    let path = context.temporary.join("buildx-cache");
    if !directory_or_absent(&path)? {
        return Ok(false);
    }
    match std::fs::symlink_metadata(path.join("index.json")) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => Err(failure("Production layer cache index must be regular.")),
    }
}

pub(super) fn promote_cache(context: &Context) -> Result<()> {
    let cache = context.temporary.join("buildx-cache");
    let next = context.temporary.join("buildx-cache-next");
    if !directory_or_absent(&next)? {
        return Err(failure("Completed production layer cache missing."));
    }
    if !std::fs::symlink_metadata(next.join("index.json")).is_ok_and(|metadata| metadata.is_file())
    {
        return Err(failure(
            "Completed production layer cache index must be regular.",
        ));
    }
    if directory_or_absent(&cache)? {
        std::fs::remove_dir_all(&cache)
            .map_err(|_| failure("Production layer cache replacement failed."))?;
    }
    std::fs::rename(next, cache).map_err(|_| failure("Production layer cache replacement failed."))
}

pub(super) struct EnvironmentFile(File);
impl EnvironmentFile {
    pub(super) fn open(context: &Context) -> Result<Self> {
        let path = context
            .environment
            .get(std::ffi::OsStr::new("GITHUB_ENV"))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| failure("GITHUB_ENV must name an existing absolute regular file."))?;
        let descriptor = rustix::fs::open(
            path,
            OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| failure("Production image environment output unavailable."))?;
        let file = File::from(descriptor);
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Err(failure(
                "Production image environment output must be regular.",
            ));
        }
        Ok(Self(file))
    }
    pub(super) fn publish(self, image: &str) -> Result<()> {
        if !digest(image) {
            return Err(failure("Production image ID invalid."));
        }
        let mut file = self.0;
        let written = file.write_all(format!("PRODUCTION_IMAGE={image}\n").as_bytes());
        let closed = nix::unistd::close(file);
        written.map_err(|_| failure("Production image environment write failed."))?;
        closed.map_err(|_| failure("Production image environment close failed."))
    }
}

pub(super) fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}
