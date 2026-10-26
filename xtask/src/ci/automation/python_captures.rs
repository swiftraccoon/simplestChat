//! Two bounded private captures. Replay borrows the caller's output and clock;
//! no capture, pathname or earlier report can authorize a successful result.

use super::python_shards::{Cause, Control, Outcome};
use crate::{
    ci::bounded_log::{Finished, Log},
    tooling::Environment,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    fs::{self, DirBuilder, File},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) const MAX_LOG: u64 = 8 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);
const NAMES: [&str; 2] = ["shard-0.log", "shard-1.log"];

pub(super) struct Captures {
    path: PathBuf,
    original: Option<File>,
    files: [Option<(u64, u64)>; 2],
}
impl Captures {
    pub(super) fn create(environment: &Environment) -> Outcome<Self> {
        let parent = ["RUNNER_TEMP", "TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .find_map(|name| {
                environment
                    .get(std::ffi::OsStr::new(name))
                    .filter(|v| !v.is_empty())
            })
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
            .canonicalize()
            .map_err(|_| Cause::Io)?;
        for _ in 0..32 {
            let path = parent.join(format!(
                "simplestchat-python-shards-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    let original = rustix::fs::open(
                        &path,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    );
                    return match original {
                        Ok(fd) => Ok(Self {
                            path,
                            original: Some(File::from(fd)),
                            files: [None, None],
                        }),
                        Err(_) => {
                            let _ = fs::remove_dir(&path);
                            Err(Cause::Io)
                        }
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(Cause::Io),
            }
        }
        Err(Cause::Io)
    }

    fn current(&self) -> Outcome<()> {
        let original = self.original.as_ref().ok_or(Cause::Io)?;
        let held = original.metadata().map_err(|_| Cause::Io)?;
        let current = fs::symlink_metadata(&self.path).map_err(|_| Cause::Io)?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || (current.dev(), current.ino()) != (held.dev(), held.ino())
            || current.mode() & 0o777 != 0o700
            || current.uid() != rustix::process::getuid().as_raw()
        {
            return Err(Cause::Io);
        }
        Ok(())
    }

    pub(super) fn log(&mut self, ordinal: usize) -> Outcome<Log> {
        self.current()?;
        let file = File::from(
            rustix::fs::openat(
                self.original.as_ref().ok_or(Cause::Io)?,
                NAMES[ordinal],
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| Cause::Io)?,
        );
        let metadata = file.metadata().map_err(|_| Cause::Io);
        match metadata {
            Ok(metadata) => self.files[ordinal] = Some((metadata.dev(), metadata.ino())),
            Err(first) => {
                let _ = nix::unistd::close(file);
                return Err(first);
            }
        }
        Log::start(file, MAX_LOG).map_err(|_| Cause::Io)
    }

    pub(super) fn replay(
        &self,
        ordinal: usize,
        finished: &Finished,
        output: &mut dyn Write,
        control: &mut Control,
    ) -> Outcome<()> {
        control.final_fence()?;
        self.current()?;
        if !finished.complete || finished.bytes > MAX_LOG {
            return Err(Cause::Io);
        }
        let mut file = File::from(
            rustix::fs::openat(
                self.original.as_ref().ok_or(Cause::Io)?,
                NAMES[ordinal],
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Cause::Io)?,
        );
        let result = (|| {
            let check = |file: &File| -> Outcome<()> {
                let metadata = file.metadata().map_err(|_| Cause::Io)?;
                let a = &metadata;
                let b = &finished.metadata;
                if !metadata.is_file()
                    || metadata.mode() & 0o777 != 0o600
                    || metadata.uid() != rustix::process::getuid().as_raw()
                    || (
                        a.dev(),
                        a.ino(),
                        a.len(),
                        a.mtime(),
                        a.mtime_nsec(),
                        a.ctime(),
                        a.ctime_nsec(),
                    ) != (
                        b.dev(),
                        b.ino(),
                        b.len(),
                        b.mtime(),
                        b.mtime_nsec(),
                        b.ctime(),
                        b.ctime_nsec(),
                    )
                    || self.files[ordinal] != Some((metadata.dev(), metadata.ino()))
                {
                    return Err(Cause::Io);
                }
                Ok(())
            };
            check(&file)?;
            let mut digest = sha2::Sha256::new();
            use sha2::Digest;
            let mut total = 0_u64;
            let mut buffer = [0; 16 * 1024];
            loop {
                control.final_fence()?;
                let size = file.read(&mut buffer).map_err(|_| Cause::Io)?;
                control.final_fence()?;
                if size == 0 {
                    break;
                }
                total = total.checked_add(size as u64).ok_or(Cause::Io)?;
                if total > finished.bytes {
                    return Err(Cause::Io);
                }
                digest.update(&buffer[..size]);
                output.write_all(&buffer[..size]).map_err(|_| Cause::Io)?;
                control.final_fence()?;
            }
            if total != finished.bytes || crate::ci::io::hex(&digest.finalize()) != finished.sha256
            {
                return Err(Cause::Io);
            }
            check(&file)?;
            self.current()?;
            control.final_fence()
        })();
        let closed = nix::unistd::close(file).map_err(|_| Cause::Close);
        let fenced = control.final_fence();
        result.and(closed).and(fenced)
    }

    pub(super) fn close(&mut self) -> Outcome<()> {
        let mut first = None;
        let mut note = |result: Outcome<()>| {
            if first.is_none() {
                first = result.err();
            }
        };
        for (ordinal, known) in self.files.iter().enumerate() {
            if let Some(known) = known {
                let result = (|| {
                    self.current()?;
                    let metadata = fs::symlink_metadata(self.path.join(NAMES[ordinal]))
                        .map_err(|_| Cause::Close)?;
                    if !metadata.is_file() || (metadata.dev(), metadata.ino()) != *known {
                        return Err(Cause::Close);
                    }
                    rustix::fs::unlinkat(
                        self.original.as_ref().ok_or(Cause::Close)?,
                        NAMES[ordinal],
                        AtFlags::empty(),
                    )
                    .map_err(|_| Cause::Close)
                })();
                note(result);
            }
        }
        note(self.current());
        if self.current().is_ok() {
            note(fs::remove_dir(&self.path).map_err(|_| Cause::Close));
        }
        note(
            self.original
                .take()
                .map(nix::unistd::close)
                .transpose()
                .map(|_| ())
                .map_err(|_| Cause::Close),
        );
        first.map_or(Ok(()), Err)
    }
}
