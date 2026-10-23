//! Private unlinked captures; interpretation requires original Finished seals.

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

use super::{Result, fail};
use crate::ci::{
    bounded_log::Log,
    command::{Command, SeparateLogs},
    ops_artifact::verify::tools::{Captures, Seal},
    release_controller::files::Directory,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub(super) fn number<T: TryInto<u64>>(value: T) -> Result<u64> {
    value.try_into().map_err(|_| fail(1, "http_policy_capture"))
}
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| fail(1, "http_policy_close"))
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
pub(super) struct Reader {
    file: File,
    initial: Metadata,
}
impl Reader {
    fn bytes(&mut self, maximum: u64, seal: &Seal) -> Result<Vec<u8>> {
        let before = self
            .file
            .metadata()
            .map_err(|_| fail(1, "fixture_container_inspection"))?;
        if !same(&before, &seal.metadata)
            || before.dev() != self.initial.dev()
            || before.ino() != self.initial.ino()
            || before.mode() != self.initial.mode()
            || before.uid() != self.initial.uid()
            || before.gid() != self.initial.gid()
            || before.nlink() != 0
            || before.len() != seal.bytes
            || seal.bytes > maximum
        {
            return Err(fail(1, "fixture_container_inspection"));
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| fail(1, "fixture_container_inspection"))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.file)
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| fail(1, "fixture_container_inspection"))?;
        if bytes.len() as u64 != seal.bytes
            || crate::ci::io::hex(&Sha256::digest(&bytes)) != seal.sha256
            || !same(
                &self
                    .file
                    .metadata()
                    .map_err(|_| fail(1, "fixture_container_inspection"))?,
                &seal.metadata,
            )
        {
            return Err(fail(1, "fixture_container_inspection"));
        }
        Ok(bytes)
    }
    fn finish(self) -> Result<()> {
        close(self.file)
    }
}
pub(super) struct Scratch {
    directory: Directory,
}
impl Scratch {
    pub fn open(parent: &Path) -> Result<Self> {
        // The captured existing parent may use ordinary platform /tmp symlinks.
        let parent = parent
            .canonicalize()
            .map_err(|_| fail(1, "http_policy_capture"))?;
        if parent.components().count() > 17 {
            return Err(fail(1, "http_policy_capture"));
        }
        Ok(Self {
            directory: Directory::open(&parent).map_err(|_| fail(1, "http_policy_capture"))?,
        })
    }
    fn anonymous(&self) -> Result<(File, Reader)> {
        self.directory
            .verify()
            .map_err(|_| fail(1, "http_policy_capture"))?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            ".release-http-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let writer = rustix::fs::openat(
            self.directory.file(),
            name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map(File::from)
        .map_err(|_| fail(1, "http_policy_capture"))?;
        let admitted = (|| {
            let initial = writer
                .metadata()
                .map_err(|_| fail(1, "http_policy_capture"))?;
            if !initial.is_file()
                || initial.mode() & 0o7777 != 0o600
                || initial.nlink() != 1
                || initial.uid() != rustix::process::geteuid().as_raw()
                || initial.len() != 0
            {
                return Err(fail(1, "http_policy_capture"));
            }
            let reader = rustix::fs::openat(
                self.directory.file(),
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| fail(1, "http_policy_capture"))?;
            let checked = (|| {
                if !same(
                    &initial,
                    &reader
                        .metadata()
                        .map_err(|_| fail(1, "http_policy_capture"))?,
                ) {
                    return Err(fail(1, "http_policy_capture"));
                }
                let visible = rustix::fs::statat(
                    self.directory.file(),
                    name.as_str(),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|_| fail(1, "http_policy_capture"))?;
                if number(visible.st_dev)? != initial.dev() || visible.st_ino != initial.ino() {
                    return Err(fail(1, "http_policy_capture"));
                }
                rustix::fs::unlinkat(self.directory.file(), name.as_str(), AtFlags::empty())
                    .map_err(|_| fail(1, "http_policy_capture"))?;
                let after = reader
                    .metadata()
                    .map_err(|_| fail(1, "http_policy_capture"))?;
                if after.dev() != initial.dev()
                    || after.ino() != initial.ino()
                    || after.mode() != initial.mode()
                    || after.uid() != initial.uid()
                    || after.gid() != initial.gid()
                    || after.len() != 0
                    || after.nlink() != 0
                    || after.mtime() != initial.mtime()
                    || after.mtime_nsec() != initial.mtime_nsec()
                {
                    return Err(fail(1, "http_policy_capture"));
                }
                Ok(after)
            })();
            match checked {
                Ok(initial) => Ok(Reader {
                    file: reader,
                    initial,
                }),
                Err(e) => {
                    let _ = close(reader);
                    Err(e)
                }
            }
        })();
        match admitted {
            Ok(reader) => Ok((writer, reader)),
            Err(e) => {
                let _ = close(writer);
                Err(e)
            }
        }
    }
    pub async fn capture(&self) -> Result<Capture> {
        self.capture_with(|_, _| Ok(())).await
    }
    async fn capture_with(
        &self,
        after_first: impl FnOnce(&Log, &Reader) -> Result<()>,
    ) -> Result<Capture> {
        let (out, stdout) = self.anonymous()?;
        let out = match Log::start(out, 65536) {
            Ok(log) => log,
            Err(e) => {
                let _ = stdout.finish();
                return Err(e);
            }
        };
        let second = after_first(&out, &stdout).and_then(|()| self.anonymous());
        let (err, stderr) = match second {
            Ok(pair) => pair,
            Err(e) => {
                let _ = out.finish().await;
                let _ = stdout.finish();
                return Err(e);
            }
        };
        match Log::start(err, 4096) {
            Ok(err) => Ok(Capture {
                logs: Some(SeparateLogs {
                    stdout: out,
                    stderr: err,
                }),
                stdout,
                stderr,
            }),
            Err(e) => {
                let _ = out.finish().await;
                let _ = stdout.finish();
                let _ = stderr.finish();
                Err(e)
            }
        }
    }
    pub fn finish(self) -> Result<()> {
        self.directory
            .finish()
            .map_err(|_| fail(1, "http_policy_capture"))
    }
}
pub(super) struct Capture {
    logs: Option<SeparateLogs>,
    stdout: Reader,
    stderr: Reader,
}
impl Capture {
    pub fn attach(&mut self, command: &mut Command) -> Result<()> {
        command.capture = None;
        command.separate_logs = Some(Arc::new(
            self.logs
                .take()
                .ok_or_else(|| fail(1, "http_policy_capture"))?,
        ));
        Ok(())
    }
    pub fn finish(mut self, seals: Result<Captures>) -> Result<Vec<u8>> {
        let checked = (|| {
            let seals = seals?;
            let out = self.stdout.bytes(65536, &seals.stdout);
            let err = self.stderr.bytes(4096, &seals.stderr);
            let out = out?;
            if !err?.is_empty() {
                return Err(fail(1, "fixture_container_inspection"));
            }
            Ok(out)
        })();
        let out = self.stdout.finish();
        let err = self.stderr.finish();
        checked.and_then(|bytes| out.and(err).map(|_| bytes))
    }
}
