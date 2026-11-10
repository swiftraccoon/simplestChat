//! Retained originals and exclusive private output under the live outer owner.

use super::{Error, Result, control::Control};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    (a.dev(), a.ino(), a.mode(), a.uid(), a.gid()) == (b.dev(), b.ino(), b.mode(), b.uid(), b.gid())
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
/// One admitted flat namespace. File observations never reconstruct this owner.
pub(super) struct Directory {
    pub path: PathBuf,
    file: Option<File>,
    before: Metadata,
}
impl Directory {
    pub fn open(path: &Path, control: &mut Control) -> Result<Self> {
        control.check()?;
        if !path.is_absolute() || fs::canonicalize(path).map_err(|_| Error::Io)? != path {
            return Err(Error::Input);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::DIRECTORY)
                    .bits() as i32,
            )
            .open(path)
            .map_err(|_| Error::Io)?;
        let observed = file.metadata().map_err(|_| Error::Io);
        let before = match observed {
            Ok(value) => value,
            Err(error) => {
                control.record(error);
                let _ = close(file, control);
                return Err(error);
            }
        };
        let mut value = Self {
            path: path.into(),
            file: Some(file),
            before,
        };
        if let Err(error) = value.check(control) {
            control.record(error);
            if value.release(control).is_err() {
                control.cleanup_failed = true;
            }
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        let actual = self
            .file
            .as_ref()
            .ok_or(Error::Close)?
            .metadata()
            .map_err(|_| Error::Io)?;
        let visible = fs::symlink_metadata(&self.path).map_err(|_| Error::Changed)?;
        if !actual.is_dir()
            || !same(&self.before, &actual)
            || !same(&self.before, &visible)
            || actual.uid() != rustix::process::getuid().as_raw()
            || actual.mode() & 0o7777 != 0o700
        {
            return Err(Error::Changed);
        }
        control.check()
    }
    pub fn names(&self, control: &mut Control) -> Result<Vec<String>> {
        self.check(control)?;
        let mut names = Vec::new();
        let cursor = rustix::fs::Dir::read_from(self.file.as_ref().ok_or(Error::Close)?)
            .map_err(|_| Error::Io)?;
        for row in cursor {
            control.check()?;
            let row = row.map_err(|_| Error::Io)?;
            if [b".".as_slice(), b".."].contains(&row.file_name().to_bytes()) {
                continue;
            }
            if names.len() >= 64 {
                return Err(Error::Refused);
            }
            names.push(
                row.file_name()
                    .to_str()
                    .map_err(|_| Error::Input)?
                    .to_string(),
            );
        }
        names.sort();
        self.check(control)?;
        Ok(names)
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        let mut first = self.check(control).err();
        if let Some(error) = first {
            control.record(error);
        }
        if let Err(error) = self.release(control) {
            first.get_or_insert(error);
            control.record(error);
        }
        if let Err(error) = control.check() {
            first.get_or_insert(error);
        }
        first.map_or(Ok(()), Err)
    }
    fn release(&mut self, control: &mut Control) -> Result<()> {
        let mut first = None;
        if let Some(file) = self.file.take()
            && close(file, control).is_err()
        {
            first.get_or_insert(Error::Close);
        }
        first.map_or(Ok(()), Err)
    }
}
impl Original {
    pub fn open(path: &Path, maximum: u64, control: &mut Control) -> Result<Self> {
        Self::admit(path, maximum, control, false)
    }
    pub fn system(path: &Path, maximum: u64, control: &mut Control) -> Result<Self> {
        Self::admit(path, maximum, control, true)
    }
    fn admit(path: &Path, maximum: u64, control: &mut Control, system: bool) -> Result<Self> {
        control.check()?;
        if !path.is_absolute() || fs::canonicalize(path).map_err(|_| Error::Io)? != path {
            return Err(Error::Input);
        }
        let parent_path = path.parent().ok_or(Error::Input)?;
        let name = path.file_name().ok_or(Error::Input)?;
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(
                rustix::fs::OFlags::NOFOLLOW.bits() as i32
                    | rustix::fs::OFlags::CLOEXEC.bits() as i32
                    | rustix::fs::OFlags::DIRECTORY.bits() as i32,
            )
            .open(parent_path)
            .map_err(|_| Error::Io)?;
        let parent_before = match parent.metadata() {
            Ok(value) => value,
            Err(_) => {
                control.record(Error::Io);
                let _ = close(parent, control);
                return Err(Error::Io);
            }
        };
        let opened = rustix::fs::openat(
            &parent,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| Error::Io);
        let file = match opened {
            Ok(file) => file,
            Err(error) => {
                control.record(error);
                let _ = close(parent, control);
                return Err(error);
            }
        };
        let observed = file.metadata().map_err(|_| Error::Io);
        let before = match observed {
            Ok(value) => value,
            Err(error) => {
                control.record(error);
                let _ = close(file, control);
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
            control.check()?;
            if !value.before.is_file()
                || value.bytes == 0
                || value.before.mode() & 0o022 != 0
                || value.bytes > maximum
                || (value.before.uid() != rustix::process::getuid().as_raw()
                    && !(system && value.before.uid() == 0))
            {
                return Err(Error::Refused);
            }
            value.sha256 = value.hash(control)?;
            value.check(control)
        })();
        if let Err(error) = admitted {
            control.record(error);
            if value.release(control).is_err() {
                control.cleanup_failed = true;
            }
            return Err(error);
        }
        Ok(value)
    }
    pub fn file(&self) -> Result<&File> {
        self.file.as_ref().ok_or(Error::Close)
    }
    fn hash(&self, control: &mut Control) -> Result<String> {
        let mut hash = Sha256::new();
        let mut offset = 0;
        let mut block = [0u8; 65536];
        while offset < self.bytes {
            control.check()?;
            let count = match self.file()?.read_at(
                &mut block[..(self.bytes - offset).min(65536) as usize],
                offset,
            ) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Io),
            };
            control.check()?;
            if count == 0 {
                return Err(Error::Changed);
            }
            hash.update(&block[..count]);
            offset += count as u64;
        }
        Ok(hex(&hash.finalize()))
    }
    pub fn metadata(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        let parent = self.parent.as_ref().ok_or(Error::Close)?;
        let parent_path = self.path.parent().ok_or(Error::Input)?;
        if !binding(
            &self.parent_before,
            &parent.metadata().map_err(|_| Error::Io)?,
        ) || !binding(
            &self.parent_before,
            &fs::symlink_metadata(parent_path).map_err(|_| Error::Io)?,
        ) || !same(
            &self.before,
            &self.file()?.metadata().map_err(|_| Error::Io)?,
        ) {
            return Err(Error::Changed);
        }
        let visible = rustix::fs::statat(
            parent,
            self.path.file_name().ok_or(Error::Input)?,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| Error::Changed)?;
        if visible.st_dev as u64 != self.before.dev()
            || visible.st_ino as u64 != self.before.ino()
            || u64::from(visible.st_mode) != u64::from(self.before.mode())
        {
            return Err(Error::Changed);
        }
        control.check()
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        self.metadata(control)?;
        if self.hash(control)? != self.sha256 {
            return Err(Error::Changed);
        }
        self.metadata(control)
    }
    pub fn data(&self, maximum: u64, control: &mut Control) -> Result<Vec<u8>> {
        self.check(control)?;
        if self.bytes > maximum {
            return Err(Error::Refused);
        }
        let mut data = vec![0; usize::try_from(self.bytes).map_err(|_| Error::Refused)?];
        let mut at = 0;
        while at < data.len() {
            control.check()?;
            let end = (at + 65536).min(data.len());
            let read = match self.file()?.read_at(&mut data[at..end], at as u64) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Io),
            };
            control.check()?;
            if read == 0 {
                return Err(Error::Changed);
            }
            at += read;
        }
        self.check(control)?;
        Ok(data)
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        let mut first = self.check(control).err();
        if let Some(error) = first {
            control.record(error);
        }
        if let Err(error) = self.release(control) {
            first.get_or_insert(error);
            control.record(error);
        }
        if let Err(error) = control.check() {
            first.get_or_insert(error);
        }
        first.map_or(Ok(()), Err)
    }
    fn release(&mut self, control: &mut Control) -> Result<()> {
        let mut first = None;
        for file in [self.file.take(), self.parent.take()].into_iter().flatten() {
            if close(file, control).is_err() {
                first.get_or_insert(Error::Close);
            }
        }
        first.map_or(Ok(()), Err)
    }
}
pub(super) fn private_file(path: &Path, control: &mut Control) -> Result<File> {
    control.check()?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits() as i32 | rustix::fs::OFlags::CLOEXEC.bits() as i32,
        )
        .open(path)
        .map_err(|_| Error::Io)?;
    if let Err(error) = control.check() {
        let _ = close(file, control);
        return Err(error);
    }
    Ok(file)
}
pub(super) fn write(path: &Path, data: &[u8], control: &mut Control) -> Result<()> {
    let mut file = private_file(path, control)?;
    let body = (|| {
        for block in data.chunks(65536) {
            control.check()?;
            file.write_all(block).map_err(|_| Error::Io)?;
            control.check()?;
        }
        file.flush().map_err(|_| Error::Io)?;
        control.check()?;
        file.sync_all().map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(error) = body {
        control.record(error);
    }
    let closed = close(file, control);
    body.and(closed).and_then(|()| control.check())
}
pub(super) fn close(file: File, control: &mut Control) -> Result<()> {
    close_result(nix::unistd::close(file).map_err(|_| Error::Close), control)
}
fn close_result(result: Result<()>, control: &mut Control) -> Result<()> {
    if let Err(error) = result {
        control.cleanup_failed = true;
        control.record(error);
    }
    result
}
pub(super) fn safe_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_bytes().len() <= 4096
        && path.components().all(|part| {
            matches!(
                part,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn observed_close_refusal_preserves_first_body_error_and_cleanup_uncertainty() {
        let mut control = Control::new(Instant::now()).unwrap();
        control.record(Error::Io);
        let closed = close_result(Err(Error::Close), &mut control);
        control.settle();
        let finished = control.finish();
        assert_eq!(closed, Err(Error::Close));
        assert_eq!(finished, Err(Error::Io));
        assert!(control.cleanup_failed);
    }
    fn work() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "maintain-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }
    #[test]
    fn actual_link_and_mutation_refusals_still_empty_every_known_owner_slot() {
        let path = work();
        let mut control = Control::new(Instant::now()).unwrap();
        let leaf = path.join("input");
        write(&leaf, b"original", &mut control).unwrap();
        let mut original = Original::open(&leaf, 20, &mut control).unwrap();
        fs::write(&leaf, b"different").unwrap();
        let changed = original.check(&mut control);
        let finished = original.finish(&mut control);
        let closed = original.file.is_none() && original.parent.is_none();
        control.settle();
        let mutation_owner = control.finish();
        let mut control = Control::new(Instant::now()).unwrap();
        fs::hard_link(&leaf, path.join("alias")).unwrap();
        let mut linked = Original::open(&leaf, 20, &mut control).unwrap();
        let linked_accepted = linked.check(&mut control);
        fs::remove_file(path.join("alias")).unwrap();
        let link_changed = linked.check(&mut control);
        let _ = linked.finish(&mut control);
        control.settle();
        let final_control = control.finish();
        fs::remove_dir_all(path).unwrap();
        assert_eq!(changed, Err(Error::Changed));
        assert_eq!(finished, Err(Error::Changed));
        assert!(closed);
        assert!(linked_accepted.is_ok());
        assert_eq!(link_changed, Err(Error::Changed));
        assert_eq!(mutation_owner, Err(Error::Changed));
        assert_eq!(final_control, Err(Error::Changed));
    }
    #[test]
    fn expired_role_cannot_read_but_always_attempts_original_file_and_parent_close() {
        let path = work();
        let mut control = Control::new(Instant::now()).unwrap();
        let leaf = path.join("input");
        write(&leaf, b"private", &mut control).unwrap();
        let mut original = Original::open(&leaf, 20, &mut control).unwrap();
        control.start = Instant::now() - Duration::from_secs(511);
        let read = original.data(20, &mut control);
        let close = original.finish(&mut control);
        let empty = original.file.is_none() && original.parent.is_none();
        control.settle();
        let finished = control.finish();
        fs::remove_dir_all(path).unwrap();
        assert_eq!(read, Err(Error::Deadline));
        assert_eq!(close, Err(Error::Deadline));
        assert!(empty);
        assert_eq!(finished, Err(Error::Deadline));
    }
    #[test]
    fn exact_directory_witness_refuses_namespace_replacement_and_closes_original() {
        let path = work();
        let child = path.join("tree");
        fs::create_dir(&child).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        let mut control = Control::new(Instant::now()).unwrap();
        let mut original = Directory::open(&child, &mut control).unwrap();
        fs::rename(&child, path.join("old")).unwrap();
        fs::create_dir(&child).unwrap();
        let changed = original.check(&mut control);
        let closed = original.finish(&mut control);
        let empty = original.file.is_none();
        control.settle();
        let _ = control.finish();
        fs::remove_dir_all(path).unwrap();
        assert_eq!(changed, Err(Error::Changed));
        assert_eq!(closed, Err(Error::Changed));
        assert!(empty);
    }
    #[test]
    fn empty_or_writable_input_and_restored_directory_membership_are_not_admitted() {
        use std::os::unix::fs::PermissionsExt;
        let path = work();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut control = Control::new(Instant::now()).unwrap();
        let leaf = path.join("input");
        fs::write(&leaf, b"").unwrap();
        let empty = Original::open(&leaf, 20, &mut control).err();
        control.settle();
        let _ = control.finish();
        let mut control = Control::new(Instant::now()).unwrap();
        fs::write(&leaf, b"bytes").unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o622)).unwrap();
        let writable = Original::open(&leaf, 20, &mut control).err();
        control.settle();
        let _ = control.finish();
        let mut control = Control::new(Instant::now()).unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o600)).unwrap();
        let mut tree = Directory::open(&path, &mut control).unwrap();
        fs::write(path.join("temporary"), b"transient").unwrap();
        fs::remove_file(path.join("temporary")).unwrap();
        let changed = tree.check(&mut control);
        let finished = tree.finish(&mut control);
        let closed = tree.file.is_none();
        control.settle();
        let _ = control.finish();
        fs::remove_dir_all(path).unwrap();
        assert_eq!(empty, Some(Error::Refused));
        assert_eq!(writable, Some(Error::Refused));
        assert_eq!(changed, Err(Error::Changed));
        assert_eq!(finished, Err(Error::Changed));
        assert!(closed);
    }
}
