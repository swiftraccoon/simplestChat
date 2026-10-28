//! Original no-follow descriptors, exact role profiles and the fixed inert-input handoff.

use super::{Error, Result, control::Control};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}
pub(super) fn close(file: File, control: &mut Control) -> Result<()> {
    if nix::unistd::close(file).is_err() {
        control.cleanup_failed = true;
        control.record(Error::Close);
        return Err(Error::Close);
    }
    control.check()
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    (a.dev(), a.ino(), a.mode(), a.uid(), a.gid(), a.nlink())
        == (b.dev(), b.ino(), b.mode(), b.uid(), b.gid(), b.nlink())
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && (
            a.len(),
            a.mtime(),
            a.mtime_nsec(),
            a.ctime(),
            a.ctime_nsec(),
        ) == (
            b.len(),
            b.mtime(),
            b.mtime_nsec(),
            b.ctime(),
            b.ctime_nsec(),
        )
}
pub(super) struct Original {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    file: Option<File>,
    parent: Option<File>,
    before: Metadata,
    parent_before: Metadata,
}
enum Profile {
    Private { uid: u32, gid: u32, mode: u32 },
    Tool { uid: u32 },
}
impl Original {
    pub fn open(
        path: &Path,
        maximum: u64,
        uid: u32,
        mode: u32,
        control: &mut Control,
    ) -> Result<Self> {
        Self::open_role(
            path,
            maximum,
            Profile::Private {
                uid,
                gid: uid,
                mode,
            },
            control,
        )
    }
    pub fn system_tool(path: &Path, control: &mut Control) -> Result<Self> {
        Self::open_role(path, 256 * 1024 * 1024, Profile::Tool { uid: 0 }, control)
    }
    #[cfg(test)]
    pub(super) fn fixture_tool(path: &Path, control: &mut Control) -> Result<Self> {
        Self::open_role(
            path,
            256 * 1024 * 1024,
            Profile::Tool {
                uid: rustix::process::getuid().as_raw(),
            },
            control,
        )
    }
    #[cfg(test)]
    pub(super) fn fixture(
        path: &Path,
        maximum: u64,
        mode: u32,
        control: &mut Control,
    ) -> Result<Self> {
        Self::open_role(
            path,
            maximum,
            Profile::Private {
                uid: rustix::process::getuid().as_raw(),
                gid: rustix::process::getgid().as_raw(),
                mode,
            },
            control,
        )
    }
    fn open_role(
        path: &Path,
        maximum: u64,
        profile: Profile,
        control: &mut Control,
    ) -> Result<Self> {
        control.check()?;
        if !path.is_absolute() || fs::canonicalize(path).map_err(|_| Error::Io)? != path {
            return Err(Error::Refused);
        }
        let parent_path = path.parent().ok_or(Error::Input)?;
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::DIRECTORY)
                    .bits() as i32,
            )
            .open(parent_path)
            .map_err(|_| Error::Io)?;
        let observed = (|| {
            let parent_before = parent.metadata().map_err(|_| Error::Io)?;
            if !parent_before.is_dir() || parent_before.mode() & 0o022 != 0 {
                return Err(Error::Refused);
            }
            let file = rustix::fs::openat(
                &parent,
                path.file_name().ok_or(Error::Input)?,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| Error::Io)?;
            let metadata = file.metadata().map_err(|_| Error::Io);
            match metadata {
                Ok(before) => Ok((file, before, parent_before)),
                Err(error) => {
                    control.record(error);
                    let _ = close(file, control);
                    Err(error)
                }
            }
        })();
        let (file, before, parent_before) = match observed {
            Ok(v) => v,
            Err(error) => {
                control.record(error);
                let _ = close(parent, control);
                return Err(error);
            }
        };
        let mut value = Self {
            path: path.into(),
            sha256: String::new(),
            bytes: before.len(),
            file: Some(file),
            parent: Some(parent),
            before,
            parent_before,
        };
        let admitted = (|| {
            let role = match profile {
                Profile::Private { uid, gid, mode } => {
                    value.before.uid() == uid
                        && value.before.gid() == gid
                        && value.before.nlink() == 1
                        && value.before.mode() & 0o7777 == mode
                }
                Profile::Tool { uid } => {
                    value.before.uid() == uid
                        && value.before.nlink() >= 1
                        && value.before.mode() & 0o111 != 0
                        && value.before.mode() & 0o022 == 0
                }
            };
            if !value.before.is_file() || !role || value.bytes == 0 || value.bytes > maximum {
                return Err(Error::Refused);
            }
            value.sha256 = value.hash(control)?;
            value.check(control)
        })();
        if let Err(error) = admitted {
            control.record(error);
            let _ = value.release(control);
            return Err(error);
        }
        Ok(value)
    }
    pub fn file(&self) -> Result<&File> {
        self.file.as_ref().ok_or(Error::Close)
    }
    pub fn closed(&self) -> bool {
        self.file.is_none() && self.parent.is_none()
    }
    pub fn require_parent(&self, uid: u32, mode: u32, control: &mut Control) -> Result<()> {
        self.check_metadata(control)?;
        let held = self
            .parent
            .as_ref()
            .ok_or(Error::Close)?
            .metadata()
            .map_err(|_| Error::Io)?;
        if held.uid() != uid || held.gid() != uid || held.mode() & 0o7777 != mode {
            return Err(Error::Refused);
        }
        self.check_metadata(control)
    }
    pub fn check_metadata(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        let parent = self.parent.as_ref().ok_or(Error::Close)?;
        if !binding(
            &self.parent_before,
            &parent.metadata().map_err(|_| Error::Io)?,
        ) || !binding(
            &self.parent_before,
            &fs::symlink_metadata(self.path.parent().ok_or(Error::Input)?)
                .map_err(|_| Error::Changed)?,
        ) || !same(
            &self.before,
            &self.file()?.metadata().map_err(|_| Error::Io)?,
        ) || !same(
            &self.before,
            &fs::symlink_metadata(&self.path).map_err(|_| Error::Changed)?,
        ) {
            return Err(Error::Changed);
        }
        control.check()
    }
    fn hash(&self, control: &mut Control) -> Result<String> {
        let mut hash = Sha256::new();
        let mut at = 0;
        let mut block = [0u8; 65536];
        while at < self.bytes {
            control.check()?;
            let size = (self.bytes - at).min(block.len() as u64) as usize;
            let count = match self.file()?.read_at(&mut block[..size], at) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                v => v.map_err(|_| Error::Io)?,
            };
            control.check()?;
            if count == 0 {
                return Err(Error::Changed);
            }
            hash.update(&block[..count]);
            at += count as u64;
        }
        Ok(hex(&hash.finalize()))
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        self.check_metadata(control)?;
        if self.hash(control)? != self.sha256 {
            return Err(Error::Changed);
        }
        self.check_metadata(control)
    }
    pub fn data(&self, maximum: u64, control: &mut Control) -> Result<Vec<u8>> {
        self.check(control)?;
        if self.bytes > maximum {
            return Err(Error::Bounds);
        }
        let mut bytes = vec![0u8; self.bytes as usize];
        let mut at = 0;
        while at < bytes.len() {
            control.check()?;
            let count = match self.file()?.read_at(&mut bytes[at..], at as u64) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.map_err(|_| Error::Io)?,
            };
            if count == 0 {
                return Err(Error::Changed);
            }
            at += count;
            control.check()?;
        }
        self.check(control)?;
        Ok(bytes)
    }
    fn release(&mut self, control: &mut Control) -> Result<()> {
        let mut first = None;
        for file in [self.file.take(), self.parent.take()].into_iter().flatten() {
            if let Err(error) = close(file, control) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        let first = self.check(control).err();
        if let Some(error) = first {
            control.record(error);
        }
        let closed = self.release(control);
        if let Some(error) = first {
            return Err(error);
        }
        closed
    }
    pub fn retire_exact(mut self, expected: &[u8], control: &mut Control) -> Result<()> {
        let result = (|| {
            if self.data(expected.len() as u64, control)? != expected {
                return Err(Error::Changed);
            }
            self.check(control)?;
            rustix::fs::unlinkat(
                self.parent.as_ref().ok_or(Error::Close)?,
                self.path.file_name().ok_or(Error::Input)?,
                rustix::fs::AtFlags::empty(),
            )
            .map_err(|_| Error::Io)?;
            let held = self.file()?.metadata().map_err(|_| Error::Io)?;
            if held.dev() != self.before.dev()
                || held.ino() != self.before.ino()
                || held.nlink() != 0
                || self.hash(control)? != self.sha256
            {
                return Err(Error::Changed);
            }
            match fs::symlink_metadata(&self.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Error::Io),
                Ok(_) => return Err(Error::Changed),
            }
            control.check()
        })();
        if let Err(error) = result {
            control.record(error);
        }
        let closed = self.release(control);
        result.and(closed)
    }
}
pub(super) fn write_new(
    path: &Path,
    bytes: &[u8],
    uid: u32,
    mode: u32,
    control: &mut Control,
) -> Result<Original> {
    control.check()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(path)
        .map_err(|_| Error::Io)?;
    let result = (|| {
        for block in bytes.chunks(65536) {
            control.check()?;
            file.write_all(block).map_err(|_| Error::Io)?;
            control.check()?;
        }
        file.flush().map_err(|_| Error::Io)?;
        rustix::fs::fchown(
            &file,
            Some(rustix::process::Uid::from_raw(uid)),
            Some(rustix::process::Gid::from_raw(uid)),
        )
        .map_err(|_| Error::Io)?;
        #[cfg(target_os = "macos")]
        let mode = u16::try_from(mode).map_err(|_| Error::Input)?;
        rustix::fs::fchmod(&file, rustix::fs::Mode::from_bits_truncate(mode))
            .map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(error) = result {
        control.record(error);
    }
    let closed = close(file, control);
    result.and(closed)?;
    Original::open(path, bytes.len() as u64, uid, mode, control)
}
pub(super) fn inert_images_handoff(control: &mut Control) -> Result<()> {
    let mut original = Original::open(
        Path::new(super::CONFIG).join("images.json").as_path(),
        3,
        0,
        0o600,
        control,
    )?;
    if let Err(error) = original.require_parent(0, 0o700, control) {
        control.record(error);
        let _ = original.finish(control);
        return Err(error);
    }
    original.retire_exact(b"{}\n", control)
}
