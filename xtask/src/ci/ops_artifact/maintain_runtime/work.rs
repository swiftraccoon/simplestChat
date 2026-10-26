//! Fixed mutable work directory; original parent and child remain open through retirement.

use super::{Error, Result, control::Control};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) struct Work {
    pub path: PathBuf,
    parent: Option<File>,
    child: Option<File>,
    parent_stamp: Metadata,
    stamp: Option<Metadata>,
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    (a.dev(), a.ino(), a.mode(), a.uid(), a.gid()) == (b.dev(), b.ino(), b.mode(), b.uid(), b.gid())
}
impl Work {
    pub fn create(output: &Path, control: &mut Control) -> Result<Self> {
        control.check()?;
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC)
                    .bits() as i32,
            )
            .open(output)
            .map_err(|_| Error::Io)?;
        let parent_stamp = match parent.metadata() {
            Ok(value) => value,
            Err(_) => {
                control.record(Error::Io);
                let _ = super::files::close(parent, control);
                return Err(Error::Io);
            }
        };
        let mut value = Self {
            path: output.join("work"),
            parent: Some(parent),
            child: None,
            parent_stamp,
            stamp: None,
        };
        let admitted = (|| {
            value.parent_check(control)?;
            let parent = value.parent.as_ref().ok_or(Error::Close)?;
            control.check()?;
            rustix::fs::mkdirat(parent, "work", rustix::fs::Mode::from_bits_truncate(0o700))
                .map_err(|_| Error::Io)?;
            let child = rustix::fs::openat(
                parent,
                "work",
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| Error::Io)?;
            value.child = Some(child);
            control.check()?;
            value.stamp = Some(
                value
                    .child
                    .as_ref()
                    .ok_or(Error::Close)?
                    .metadata()
                    .map_err(|_| Error::Io)?,
            );
            value.check(control)
        })();
        if let Err(error) = admitted {
            control.record(error);
            let _ = value.finish(control);
            return Err(error);
        }
        Ok(value)
    }
    fn parent_check(&self, control: &mut Control) -> Result<()> {
        control.check_retirement()?;
        let actual = self
            .parent
            .as_ref()
            .ok_or(Error::Close)?
            .metadata()
            .map_err(|_| Error::Io)?;
        let visible = fs::symlink_metadata(self.path.parent().ok_or(Error::Input)?)
            .map_err(|_| Error::Changed)?;
        if !actual.is_dir()
            || actual.uid() != rustix::process::getuid().as_raw()
            || actual.mode() & 0o7777 != 0o700
            || !binding(&self.parent_stamp, &actual)
            || !binding(&self.parent_stamp, &visible)
        {
            return Err(Error::Changed);
        }
        control.check_retirement()
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        self.retirement_check(control)
    }
    fn retirement_check(&self, control: &mut Control) -> Result<()> {
        self.parent_check(control)?;
        let stamp = self.stamp.as_ref().ok_or(Error::Changed)?;
        let actual = self
            .child
            .as_ref()
            .ok_or(Error::Close)?
            .metadata()
            .map_err(|_| Error::Io)?;
        let visible = rustix::fs::statat(
            self.parent.as_ref().ok_or(Error::Close)?,
            "work",
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| Error::Changed)?;
        if !actual.is_dir()
            || actual.uid() != rustix::process::getuid().as_raw()
            || actual.mode() & 0o7777 != 0o700
            || !binding(stamp, &actual)
            || visible.st_dev as u64 != stamp.dev()
            || visible.st_ino as u64 != stamp.ino()
            || u64::from(visible.st_mode) != u64::from(stamp.mode())
        {
            return Err(Error::Changed);
        }
        control.check_retirement()
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        let checked = self.retirement_check(control);
        let mut first = checked.err();
        if let Some(error) = first {
            control.record(error);
        }
        if first.is_none() {
            let removed = (|| {
                clear(self.child.as_ref().ok_or(Error::Close)?, false, control)?;
                self.retirement_check(control)?;
                rustix::fs::unlinkat(
                    self.parent.as_ref().ok_or(Error::Close)?,
                    "work",
                    rustix::fs::AtFlags::REMOVEDIR,
                )
                .map_err(|_| Error::Io)?;
                control.check_retirement()?;
                match rustix::fs::statat(
                    self.parent.as_ref().ok_or(Error::Close)?,
                    "work",
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                ) {
                    Err(rustix::io::Errno::NOENT) => Ok(()),
                    _ => Err(Error::Changed),
                }
            })();
            if let Err(error) = removed {
                first.get_or_insert(error);
                control.record(error);
            }
        }
        if first.is_some() {
            control.cleanup_failed = true;
        }
        for file in [self.child.take(), self.parent.take()]
            .into_iter()
            .flatten()
        {
            if super::files::close(file, control).is_err() {
                first.get_or_insert(Error::Close);
                control.record(Error::Close);
            }
        }
        if let Err(error) = control.check_retirement() {
            first.get_or_insert(error);
        }
        first.map_or(Ok(()), Err)
    }
}
fn clear(directory: &File, nested: bool, control: &mut Control) -> Result<()> {
    control.check_retirement()?;
    // Only native-tools is a generated subdirectory. All other work entries are
    // fixed VM files/sockets; a new nested namespace is an unreviewed profile.
    // Dir's internal cursor descriptor has an opaque RAII close return.
    let cursor = rustix::fs::Dir::read_from(directory).map_err(|_| Error::Io)?;
    for entry in cursor {
        control.check_retirement()?;
        let entry = entry.map_err(|_| Error::Io)?;
        let name = entry.file_name();
        if [b".".as_slice(), b".."].contains(&name.to_bytes()) {
            continue;
        }
        let before = rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::Io)?;
        control.check_retirement()?;
        if before.st_uid != rustix::process::getuid().as_raw() {
            return Err(Error::Changed);
        }
        let is_directory =
            rustix::fs::FileType::from_raw_mode(before.st_mode) == rustix::fs::FileType::Directory;
        if is_directory {
            if nested || name.to_bytes() != b"native-tools" {
                return Err(Error::Refused);
            }
            let child = rustix::fs::openat(
                directory,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| Error::Io)?;
            let body = (|| {
                control.check_retirement()?;
                let actual = child.metadata().map_err(|_| Error::Io)?;
                if actual.dev() != before.st_dev as u64
                    || actual.ino() != before.st_ino as u64
                    || u64::from(actual.mode()) != u64::from(before.st_mode)
                {
                    return Err(Error::Changed);
                }
                clear(&child, true, control)?;
                let visible =
                    rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|_| Error::Changed)?;
                if visible.st_dev != before.st_dev
                    || visible.st_ino != before.st_ino
                    || visible.st_mode != before.st_mode
                {
                    return Err(Error::Changed);
                }
                control.check_retirement()?;
                rustix::fs::unlinkat(directory, name, rustix::fs::AtFlags::REMOVEDIR)
                    .map_err(|_| Error::Io)?;
                control.check_retirement()
            })();
            let closed = super::files::close(child, control);
            body.and(closed)?;
        } else {
            control.check_retirement()?;
            let visible =
                rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| Error::Changed)?;
            if visible.st_dev != before.st_dev
                || visible.st_ino != before.st_ino
                || visible.st_mode != before.st_mode
            {
                return Err(Error::Changed);
            }
            rustix::fs::unlinkat(directory, name, rustix::fs::AtFlags::empty())
                .map_err(|_| Error::Io)?;
            control.check_retirement()?;
        }
    }
    control.check_retirement()
}

#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
