//! Fresh private summary only; no response, chat, identity or error payload logs.
use super::io::{Result, fail};
use crate::ci::release_controller::files::Directory;
use rustix::fs::Mode;
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

pub(super) fn uuid() -> Result<String> {
    let mut file = File::open("/dev/urandom").map_err(|_| fail(1, "artifact_failed"))?;
    let mut bytes = [0_u8; 16];
    let read = file
        .read_exact(&mut bytes)
        .map_err(|_| fail(1, "artifact_failed"));
    let closed = nix::unistd::close(file).map_err(|_| fail(1, "artifact_failed"));
    read?;
    closed?;
    bytes[6] = bytes[6] & 15 | 64;
    bytes[8] = bytes[8] & 63 | 128;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
pub(super) trait Artifacts {
    fn publish(&mut self, value: &Value) -> Result<()>;
    fn finish(self) -> Result<()>;
}
pub(super) struct Output {
    directory: Directory,
    summary: Option<std::fs::Metadata>,
}
impl Output {
    pub fn new(path: &Path, default_output: bool) -> Result<Self> {
        let parent = path.parent().ok_or_else(|| fail(2, "invalid_output"))?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| fail(2, "invalid_output"))?;
        let directory = if default_output {
            let base = parent.parent().ok_or_else(|| fail(1, "artifact_failed"))?;
            let base = Directory::open(base).map_err(|_| fail(1, "artifact_failed"))?;
            let created =
                rustix::fs::mkdirat(base.file(), "results", Mode::from_bits_truncate(0o700));
            if created.is_err_and(|e| e != rustix::io::Errno::EXIST) {
                let _ = base.finish();
                return Err(fail(1, "artifact_failed"));
            }
            let results = base
                .descend("results")
                .map_err(|_| fail(1, "artifact_failed"))?;
            if created.is_ok()
                && !results.file().metadata().is_ok_and(|m| {
                    m.mode() & 0o7777 == 0o700 && m.uid() == rustix::process::getuid().as_raw()
                })
            {
                let _ = results.finish();
                return Err(fail(1, "artifact_failed"));
            }
            results
        } else {
            Directory::open(parent).map_err(|_| fail(1, "artifact_failed"))?
        };
        let created = rustix::fs::mkdirat(directory.file(), name, Mode::from_bits_truncate(0o700));
        if created.is_err() {
            let _ = directory.finish();
            return Err(fail(1, "artifact_failed"));
        }
        let directory = directory
            .descend(name)
            .map_err(|_| fail(1, "artifact_failed"))?;
        let good = directory.file().metadata().is_ok_and(|m| {
            m.mode() & 0o7777 == 0o700 && m.uid() == rustix::process::getuid().as_raw()
        });
        if !good {
            let _ = directory.finish();
            return Err(fail(1, "artifact_failed"));
        }
        Ok(Self {
            directory,
            summary: None,
        })
    }
}
impl Artifacts for Output {
    fn publish(&mut self, value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| fail(1, "artifact_failed"))?;
        if bytes.len() > 16 * 1024 {
            return Err(fail(1, "artifact_failed"));
        }
        let mut file = self
            .directory
            .create("summary.json.part")
            .map_err(|_| fail(1, "artifact_failed"))?;
        let observed = (|| {
            let original = file.metadata().map_err(|_| fail(1, "artifact_failed"))?;
            if !original.is_file()
                || original.mode() & 0o7777 != 0o600
                || original.nlink() != 1
                || original.uid() != rustix::process::getuid().as_raw()
            {
                return Err(fail(1, "artifact_failed"));
            }
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| fail(1, "artifact_failed"))?;
            let written = file.metadata().map_err(|_| fail(1, "artifact_failed"))?;
            if original.dev() != written.dev()
                || original.ino() != written.ino()
                || original.mode() != written.mode()
                || original.uid() != written.uid()
                || original.gid() != written.gid()
                || written.nlink() != 1
                || written.len() != bytes.len() as u64
            {
                return Err(fail(1, "artifact_failed"));
            }
            self.directory
                .check("summary.json.part", &written)
                .map_err(|_| fail(1, "artifact_failed"))?;
            rustix::fs::renameat_with(
                self.directory.file(),
                "summary.json.part",
                self.directory.file(),
                "summary.json",
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| fail(1, "artifact_failed"))?;
            // Rename may legitimately change ctime. The same original retained
            // descriptor supplies its post-rename stamp, never a new baseline.
            let renamed = file.metadata().map_err(|_| fail(1, "artifact_failed"))?;
            if written.dev() != renamed.dev()
                || written.ino() != renamed.ino()
                || written.mode() != renamed.mode()
                || written.uid() != renamed.uid()
                || written.gid() != renamed.gid()
                || written.nlink() != renamed.nlink()
                || written.len() != renamed.len()
                || written.mtime() != renamed.mtime()
                || written.mtime_nsec() != renamed.mtime_nsec()
            {
                return Err(fail(1, "artifact_failed"));
            }
            self.directory
                .check("summary.json", &renamed)
                .map_err(|_| fail(1, "artifact_failed"))?;
            Ok(renamed)
        })();
        let closed = nix::unistd::close(file).map_err(|_| fail(1, "artifact_failed"));
        let stamp = observed?;
        closed?;
        let mut visible = rustix::fs::openat(
            self.directory.file(),
            "summary.json",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| fail(1, "artifact_failed"))?;
        let readback = (|| {
            let after = visible.metadata().map_err(|_| fail(1, "artifact_failed"))?;
            if after.dev() != stamp.dev()
                || after.ino() != stamp.ino()
                || after.mode() != stamp.mode()
                || after.uid() != stamp.uid()
                || after.gid() != stamp.gid()
                || after.len() != stamp.len()
                || after.nlink() != 1
                || after.mtime() != stamp.mtime()
                || after.mtime_nsec() != stamp.mtime_nsec()
                || after.ctime() != stamp.ctime()
                || after.ctime_nsec() != stamp.ctime_nsec()
            {
                return Err(fail(1, "artifact_failed"));
            }
            let mut observed = Vec::new();
            Read::by_ref(&mut visible)
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut observed)
                .map_err(|_| fail(1, "artifact_failed"))?;
            if observed != bytes {
                return Err(fail(1, "artifact_failed"));
            }
            self.directory
                .check("summary.json", &after)
                .map_err(|_| fail(1, "artifact_failed"))?;
            Ok(after)
        })();
        let closed = nix::unistd::close(visible).map_err(|_| fail(1, "artifact_failed"));
        self.summary = Some(readback?);
        closed?;
        self.directory
            .file()
            .sync_all()
            .map_err(|_| fail(1, "artifact_failed"))?;
        self.directory
            .verify()
            .map_err(|_| fail(1, "artifact_failed"))
    }
    fn finish(self) -> Result<()> {
        let checked = self.summary.as_ref().map_or(Ok(()), |stamp| {
            self.directory
                .check("summary.json", stamp)
                .map_err(|_| fail(1, "artifact_failed"))
        });
        let closed = self
            .directory
            .finish()
            .map_err(|_| fail(1, "artifact_failed"));
        checked.and(closed)
    }
}
