//! Descriptor-bound private staging. Only the known owned entries are removed.

use super::plan::{Context, MAX_BYTES, VERSION};
use crate::tooling::{Failure, Result};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

pub(super) fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure("Compose staging operation failed."))
}
fn open_at(directory: &File, name: &str, flags: OFlags) -> Result<File> {
    rustix::fs::openat(
        directory,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map(File::from)
    .map_err(|_| failure("Compose staging descriptor unavailable."))
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure("Compose staging close failed."))
}
fn same(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}
fn unchanged(left: &Metadata, right: &Metadata) -> bool {
    same(left, right)
        && left.len() == right.len()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}
fn binding(directory: &File, name: &str, expected: &Metadata, is_directory: bool) -> Result<()> {
    let flags = if is_directory {
        OFlags::DIRECTORY
    } else {
        OFlags::NONBLOCK
    };
    let visible = open_at(directory, name, OFlags::RDONLY | flags)?;
    let metadata = io(visible.metadata());
    let closed = close(visible);
    let metadata = metadata?;
    closed?;
    if !same(&metadata, expected)
        || if is_directory {
            !metadata.is_dir()
                || metadata.mode() != expected.mode()
                || metadata.uid() != expected.uid()
                || metadata.gid() != expected.gid()
        } else {
            !metadata.is_file()
        }
    {
        return Err(failure("Compose staging identity changed."));
    }
    Ok(())
}
fn write_new(directory: &File, name: &str, bytes: &[u8]) -> Result<()> {
    let mut file = open_at(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
    )?;
    let written = io(file.write_all(bytes).and_then(|()| file.sync_all()));
    let closed = close(file);
    written?;
    closed
}

/// Authenticate the bytes of the actual private install input, independently
/// of the source hash. Stable metadata and binding bracket the full read-back.
fn authenticate_copy(directory: &File, file: &mut File, expected: &str) -> Result<Metadata> {
    let before = io(file.metadata())?;
    if !before.is_file() || before.nlink() != 1 || before.len() == 0 || before.len() > MAX_BYTES {
        return Err(failure(
            "Verified Compose copy must be bounded regular bytes.",
        ));
    }
    binding(directory, "verified", &before, false)?;
    io(file.seek(SeekFrom::Start(0)))?;
    let mut digest = Sha256::new();
    let mut count = 0u64;
    let mut buffer = vec![0; 65536];
    loop {
        let length = io(file.read(&mut buffer))?;
        if length == 0 {
            break;
        }
        count += length as u64;
        if count > MAX_BYTES {
            return Err(failure("Verified Compose copy exceeds its bound."));
        }
        digest.update(&buffer[..length]);
    }
    let after = io(file.metadata())?;
    binding(directory, "verified", &after, false)?;
    let actual = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if count != before.len() || !unchanged(&before, &after) || actual != expected {
        return Err(failure(
            "Verified Compose copy checksum or identity mismatch.",
        ));
    }
    Ok(after)
}

pub(super) struct Owned {
    parent: File,
    parent_path: PathBuf,
    directory: File,
    download: Option<File>,
    identity: Metadata,
    pub path: PathBuf,
    report_name: String,
    name: String,
    verified: Option<Metadata>,
}
impl Owned {
    pub(super) fn new(context: &Context) -> Result<Self> {
        let mut random = io(File::open("/dev/urandom"))?;
        let mut bytes = [0; 16];
        let read = io(random.read_exact(&mut bytes));
        let closed = close(random);
        read?;
        closed?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!("simplestchat-compose-{token}");
        let parent = rustix::fs::open(
            &context.temporary,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure("Compose temporary directory unavailable."))?;
        if rustix::fs::mkdirat(&parent, name.as_str(), Mode::from_bits_truncate(0o700)).is_err() {
            close(parent)?;
            return Err(failure("Fresh Compose directory unavailable."));
        }
        let directory = match open_at(&parent, &name, OFlags::RDONLY | OFlags::DIRECTORY) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = rustix::fs::unlinkat(&parent, name.as_str(), AtFlags::REMOVEDIR);
                let _ = close(parent);
                return Err(error);
            }
        };
        let identity = match directory.metadata() {
            Ok(identity) => identity,
            Err(_) => {
                let _ = close(directory);
                let _ = rustix::fs::unlinkat(&parent, name.as_str(), AtFlags::REMOVEDIR);
                let _ = close(parent);
                return Err(failure("Compose directory metadata unavailable."));
            }
        };
        Ok(Self {
            parent,
            parent_path: context.temporary.clone(),
            directory,
            download: None,
            identity,
            path: context.temporary.join(&name),
            report_name: format!("compose-install-{token}.json"),
            name,
            verified: None,
        })
    }

    pub(super) fn prepare(&mut self, context: &Context) -> Result<()> {
        self.check_directory()?;
        write_new(
            &self.directory,
            "intent.json",
            &serde_json::to_vec(&serde_json::json!({
                "version":VERSION,"architecture":context.asset.architecture,
                "expectedSha256":context.asset.digest,"maxBytes":MAX_BYTES
            }))
            .map_err(|_| failure("Compose intent unavailable."))?,
        )?;
        self.download = Some(open_at(
            &self.directory,
            "download",
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NONBLOCK,
        )?);
        Ok(())
    }

    pub(super) fn download_sink(&self) -> Result<std::sync::Arc<File>> {
        let file = self
            .download
            .as_ref()
            .ok_or_else(|| failure("Compose download not owned."))?;
        io(file.try_clone()).map(std::sync::Arc::new)
    }

    fn check_directory(&self) -> Result<()> {
        let visible = rustix::fs::open(
            &self.parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure("Compose parent binding unavailable."))?;
        let observed = io(visible.metadata());
        let closed = close(visible);
        let observed = observed?;
        closed?;
        if !same(&observed, &io(self.parent.metadata())?) {
            return Err(failure("Compose parent identity changed."));
        }
        binding(&self.parent, &self.name, &self.identity, true)
    }

    /// Copy the stopped curl output through a retained FD, authenticate the copy,
    /// and explicitly close both files before any sudo invocation is permitted.
    pub(super) fn verify(&mut self, expected: &str) -> Result<u64> {
        self.check_directory()?;
        let mut source = self
            .download
            .take()
            .ok_or_else(|| failure("Compose download not owned."))?;
        let result: Result<(u64, Metadata)> = (|| {
            let before = io(source.metadata())?;
            if !before.is_file()
                || before.nlink() != 1
                || before.len() == 0
                || before.len() > MAX_BYTES
            {
                return Err(failure(
                    "Compose download must be bounded nonempty regular bytes.",
                ));
            }
            binding(&self.directory, "download", &before, false)?;
            let mut output = open_at(
                &self.directory,
                "verified",
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
            )?;
            let copy: Result<(u64, Metadata)> = (|| {
                io(source.seek(SeekFrom::Start(0)))?;
                let mut digest = Sha256::new();
                let mut buffer = vec![0; 65536];
                let mut count = 0u64;
                loop {
                    let length = io(source.read(&mut buffer))?;
                    if length == 0 {
                        break;
                    }
                    count += length as u64;
                    if count > MAX_BYTES {
                        return Err(failure("Compose download exceeds its bound."));
                    }
                    digest.update(&buffer[..length]);
                    io(output.write_all(&buffer[..length]))?;
                }
                let after = io(source.metadata())?;
                binding(&self.directory, "download", &after, false)?;
                if count != before.len()
                    || !unchanged(&before, &after)
                    || digest
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                        != expected
                {
                    return Err(failure("Compose download checksum or identity mismatch."));
                }
                io(output.sync_all())?;
                Ok((
                    count,
                    authenticate_copy(&self.directory, &mut output, expected)?,
                ))
            })();
            let closed = close(output);
            let result = copy?;
            closed?;
            Ok(result)
        })();
        let closed = close(source);
        let (count, metadata) = result?;
        closed?;
        self.verified = Some(metadata);
        self.check_verified()?;
        Ok(count)
    }

    pub(super) fn check_verified(&self) -> Result<()> {
        self.check_directory()?;
        let expected = self
            .verified
            .as_ref()
            .ok_or_else(|| failure("Compose copy not verified."))?;
        let file = open_at(
            &self.directory,
            "verified",
            OFlags::RDONLY | OFlags::NONBLOCK,
        )?;
        let observed = io(file.metadata());
        let closed = close(file);
        let observed = observed?;
        closed?;
        if !observed.is_file() || !unchanged(expected, &observed) {
            return Err(failure("Verified Compose copy changed."));
        }
        Ok(())
    }

    /// Close every held input and remove only this owned directory's three fixed
    /// entries. A replaced prefix or unexpected extra entry refuses clean success.
    pub(super) fn cleanup(mut self) -> (Reporter, Result<()>) {
        let mut passed = true;
        if let Some(file) = self.download.take() {
            passed &= close(file).is_ok();
        }
        for name in ["download", "verified", "intent.json"] {
            match rustix::fs::unlinkat(&self.directory, name, AtFlags::empty()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(_) => passed = false,
            }
        }
        if binding(&self.parent, &self.name, &self.identity, true).is_ok() {
            passed &=
                rustix::fs::unlinkat(&self.parent, self.name.as_str(), AtFlags::REMOVEDIR).is_ok();
        } else {
            passed = false;
        }
        passed &= close(self.directory).is_ok();
        let reporter = Reporter {
            parent: self.parent,
            name: self.report_name,
        };
        let result = if passed {
            Ok(())
        } else {
            Err(failure("Compose staging cleanup failed."))
        };
        (reporter, result)
    }
}

/// The original parent descriptor survives staging retirement. A replacement
/// RUNNER_TEMP pathname cannot redirect this fixed, exclusive private report.
pub(super) struct Reporter {
    parent: File,
    name: String,
}
impl Reporter {
    pub(super) fn write(self, value: &impl serde::Serialize) -> Result<()> {
        let result = (|| {
            let bytes =
                serde_json::to_vec_pretty(value).map_err(|_| failure("Compose report invalid."))?;
            write_new(&self.parent, &self.name, &bytes)
        })();
        let closed = close(self.parent);
        result?;
        closed
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
