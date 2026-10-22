//! Fixed, read-only namespace owner. The shared tree retains at most 48 Files,
//! including one transient verification probe; no directory enumeration occurs.

use super::{Failure, Result};
use nix::{
    errno::Errno,
    fcntl::{AtFlags, OFlag, open, openat, readlinkat},
    sys::stat::{Mode, fstatat},
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, os::unix::fs::MetadataExt};

const FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);
const MAX_FILES: usize = 48;
const OS_RELEASE: u64 = 65536;

fn convert<T: TryInto<U>, U>(value: T) -> Result<U> {
    value.try_into().map_err(|_| Failure::Unavailable)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub len: u64,
    pub links: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
}
impl Stamp {
    fn binding(&self, other: &Self) -> bool {
        self.dev == other.dev
            && self.ino == other.ino
            && self.mode == other.mode
            && self.uid == other.uid
            && self.gid == other.gid
    }
    fn directory(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
    fn regular(&self) -> bool {
        self.mode & 0o170000 == 0o100000
    }
    fn protected(&self, mode: u32, directory: bool) -> Result<()> {
        if self.uid != 0
            || self.gid != 0
            || self.mode & 0o7777 != mode
            || if directory {
                !self.directory()
            } else {
                !self.regular()
            }
        {
            return Err(Failure::Path);
        }
        Ok(())
    }
}

/// Test adapters project ownership only; actual file kinds, bytes, modes,
/// timestamps, visible bindings, reads and closes remain the same implementation.
pub(super) trait Access: Clone {
    fn ownership(&self, _path: &str, uid: u32, gid: u32) -> (u32, u32) {
        (uid, gid)
    }
    fn event(&self, _event: &'static str, _path: &str) -> Result<()> {
        Ok(())
    }
    fn stamp(&self, file: &File, path: &str) -> Result<Stamp> {
        self.event("metadata", path)?;
        let value = file.metadata().map_err(|_| Failure::Unavailable)?;
        let (uid, gid) = self.ownership(path, value.uid(), value.gid());
        Ok(Stamp {
            dev: value.dev(),
            ino: value.ino(),
            mode: value.mode(),
            uid,
            gid,
            len: value.len(),
            links: value.nlink(),
            modified: (value.mtime(), value.mtime_nsec()),
            changed: (value.ctime(), value.ctime_nsec()),
        })
    }
    fn close(&self, file: File) -> Result<()> {
        nix::unistd::close(file).map_err(|_| Failure::Unavailable)
    }
}
#[derive(Clone, Copy)]
pub(super) struct NativeAccess;
impl Access for NativeAccess {}

struct Directory {
    file: File,
    before: Stamp,
    path: String,
    parent: usize,
    name: String,
}
struct Input {
    file: File,
    before: Stamp,
    parent: usize,
    name: String,
    path: String,
}
struct Link {
    parent: usize,
    before: Stamp,
    target: Vec<u8>,
}
pub(super) struct Tree<A: Access> {
    access: A,
    directories: Vec<Directory>,
    files: Vec<Input>,
    link: Option<Link>,
    absent_os_release: Option<usize>,
}
impl Tree<NativeAccess> {
    pub fn native() -> Result<Self> {
        let file = File::from(
            open("/", FLAGS | OFlag::O_DIRECTORY, Mode::empty())
                .map_err(|_| Failure::Unavailable)?,
        );
        Self::open(file, NativeAccess)
    }
}
impl<A: Access> Tree<A> {
    pub fn open(file: File, access: A) -> Result<Self> {
        let before = match access.stamp(&file, "") {
            Ok(value) => value,
            Err(error) => {
                let _ = access.close(file);
                return Err(error);
            }
        };
        if !before.directory() || before.uid != 0 || before.gid != 0 || before.mode & 0o022 != 0 {
            let _ = access.close(file);
            return Err(Failure::Path);
        }
        Ok(Self {
            access,
            directories: vec![Directory {
                file,
                before,
                path: String::new(),
                parent: 0,
                name: String::new(),
            }],
            files: Vec::new(),
            link: None,
            absent_os_release: None,
        })
    }
    fn capacity(&self) -> Result<()> {
        // Reserve one additional transient visible-binding probe for verify().
        if self.directories.len() + self.files.len() + 2 > MAX_FILES {
            Err(Failure::Unavailable)
        } else {
            Ok(())
        }
    }
    fn directory(&mut self, path: &str) -> Result<usize> {
        if let Some(index) = self.directories.iter().position(|value| value.path == path) {
            return Ok(index);
        }
        self.capacity()?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.directory(parent)?;
        self.capacity()?;
        let file = File::from(
            openat(
                &self.directories[parent].file,
                name,
                FLAGS | OFlag::O_DIRECTORY,
                Mode::empty(),
            )
            .map_err(|error| match error {
                Errno::ELOOP | Errno::ENOTDIR => Failure::Path,
                _ => Failure::Unavailable,
            })?,
        );
        let admitted = self.access.stamp(&file, path).and_then(|before| {
            if !before.directory() || before.uid != 0 || before.gid != 0 || before.mode & 0o022 != 0
            {
                Err(Failure::Path)
            } else {
                Ok(before)
            }
        });
        let before = match admitted {
            Ok(value) => value,
            Err(error) => {
                let _ = self.access.close(file);
                return Err(error);
            }
        };
        self.directories.push(Directory {
            file,
            before,
            parent,
            name: name.into(),
            path: path.into(),
        });
        Ok(self.directories.len() - 1)
    }
    pub fn protected_directory(&mut self, path: &str, mode: u32) -> Result<()> {
        let index = self.directory(path)?;
        self.directories[index].before.protected(mode, true)
    }
    pub fn protected_file(&mut self, path: &str, mode: u32, maximum: u64) -> Result<usize> {
        self.input(path, mode, maximum, true)
    }
    fn input(&mut self, path: &str, mode: u32, maximum: u64, nonempty: bool) -> Result<usize> {
        self.capacity()?;
        let (parent, name) = path.rsplit_once('/').ok_or(Failure::Path)?;
        let parent = self.directory(parent)?;
        self.capacity()?;
        let initial = self.stat(parent, name, path)?;
        initial.protected(mode, false)?;
        if (nonempty && initial.len == 0) || initial.len > maximum {
            return Err(Failure::Size);
        }
        let file = File::from(
            openat(&self.directories[parent].file, name, FLAGS, Mode::empty()).map_err(
                |error| {
                    if error == Errno::ELOOP {
                        Failure::Path
                    } else {
                        Failure::Unavailable
                    }
                },
            )?,
        );
        let admitted = self.access.stamp(&file, path).and_then(|before| {
            before.protected(mode, false)?;
            if before != initial {
                return Err(Failure::Unavailable);
            }
            Ok(before)
        });
        let before = match admitted {
            Ok(value) => value,
            Err(error) => {
                let _ = self.access.close(file);
                return Err(error);
            }
        };
        self.files.push(Input {
            file,
            before,
            parent,
            name: name.into(),
            path: path.into(),
        });
        Ok(self.files.len() - 1)
    }
    fn read(&mut self, index: usize, mut consume: impl FnMut(&[u8])) -> Result<()> {
        let value = &mut self.files[index];
        self.access.event("read", &value.path)?;
        if self.access.stamp(&value.file, &value.path)? != value.before {
            return Err(Failure::Unavailable);
        }
        let mut bytes = [0; 65536];
        let mut total = 0_u64;
        while total < value.before.len {
            let remaining = (value.before.len - total).min(bytes.len() as u64) as usize;
            let count = value
                .file
                .read(&mut bytes[..remaining])
                .map_err(|_| Failure::Unavailable)?;
            if count == 0 {
                return Err(Failure::Unavailable);
            }
            total = total
                .checked_add(count as u64)
                .ok_or(Failure::Unavailable)?;
            consume(&bytes[..count]);
        }
        self.access.event("read_complete", &value.path)?;
        if total != value.before.len || self.access.stamp(&value.file, &value.path)? != value.before
        {
            return Err(Failure::Unavailable);
        }
        Ok(())
    }
    pub fn hash(&mut self, index: usize) -> Result<[u8; 32]> {
        let mut digest = Sha256::new();
        self.read(index, |bytes| digest.update(bytes))?;
        Ok(digest.finalize().into())
    }
    fn stat(&self, parent: usize, name: &str, path: &str) -> Result<Stamp> {
        let value = fstatat(
            &self.directories[parent].file,
            name,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|_| Failure::Unavailable)?;
        let (uid, gid) = self.access.ownership(path, value.st_uid, value.st_gid);
        Ok(Stamp {
            dev: convert(value.st_dev)?,
            ino: value.st_ino,
            mode: convert(value.st_mode)?,
            uid,
            gid,
            len: convert(value.st_size)?,
            links: convert(value.st_nlink)?,
            modified: (value.st_mtime, value.st_mtime_nsec),
            changed: (value.st_ctime, value.st_ctime_nsec),
        })
    }
    fn link(&self, parent: usize) -> Result<(Stamp, Vec<u8>)> {
        let stamp = self.stat(parent, "os-release", "etc/os-release")?;
        if stamp.len > 4096 {
            return Err(Failure::Unavailable);
        }
        let target = readlinkat(&self.directories[parent].file, "os-release")
            .map_err(|_| Failure::Unavailable)?
            .as_encoded_bytes()
            .to_vec();
        if target.len() > 4096 {
            return Err(Failure::Unavailable);
        }
        Ok((stamp, target))
    }
    pub fn os_release(&mut self) -> Result<Vec<u8>> {
        let parent = self.directory("etc")?;
        let path = match fstatat(
            &self.directories[parent].file,
            "os-release",
            AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Err(Errno::ENOENT) => {
                self.absent_os_release = Some(parent);
                "usr/lib/os-release"
            }
            Err(_) => return Err(Failure::Unavailable),
            Ok(value) if value.st_mode & 0o170000 == 0o120000 => {
                let (before, target) = self.link(parent)?;
                if before.uid != 0
                    || before.gid != 0
                    || !matches!(
                        target.as_slice(),
                        b"../usr/lib/os-release" | b"/usr/lib/os-release"
                    )
                {
                    return Err(Failure::Path);
                }
                self.link = Some(Link {
                    parent,
                    before,
                    target,
                });
                "usr/lib/os-release"
            }
            Ok(_) => "etc/os-release",
        };
        let index = self.input(path, 0o644, OS_RELEASE, false)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(self.files[index].before.len as usize)
            .map_err(|_| Failure::Unavailable)?;
        self.read(index, |part| bytes.extend_from_slice(part))?;
        Ok(bytes)
    }
    pub fn verify(&self) -> Result<()> {
        self.access.event("verify", "")?;
        for (index, value) in self.directories.iter().enumerate() {
            if !value
                .before
                .binding(&self.access.stamp(&value.file, &value.path)?)
            {
                return Err(Failure::Unavailable);
            }
            if index != 0 {
                self.visible(value.parent, &value.name, &value.path, &value.before, true)?;
            }
        }
        for value in &self.files {
            if value.before != self.access.stamp(&value.file, &value.path)? {
                return Err(Failure::Unavailable);
            }
            self.visible(value.parent, &value.name, &value.path, &value.before, false)?;
        }
        if let Some(link) = &self.link {
            let (stamp, target) = self.link(link.parent)?;
            if stamp != link.before || target != link.target {
                return Err(Failure::Unavailable);
            }
        }
        if let Some(parent) = self.absent_os_release
            && !matches!(
                fstatat(
                    &self.directories[parent].file,
                    "os-release",
                    AtFlags::AT_SYMLINK_NOFOLLOW,
                ),
                Err(Errno::ENOENT),
            )
        {
            return Err(Failure::Unavailable);
        }
        Ok(())
    }
    fn visible(
        &self,
        parent: usize,
        name: &str,
        path: &str,
        before: &Stamp,
        dir: bool,
    ) -> Result<()> {
        let file = File::from(
            openat(
                &self.directories[parent].file,
                name,
                if dir {
                    FLAGS | OFlag::O_DIRECTORY
                } else {
                    FLAGS
                },
                Mode::empty(),
            )
            .map_err(|_| Failure::Unavailable)?,
        );
        let observed = self.access.stamp(&file, path);
        let closed = self.access.close(file);
        let observed = observed?;
        closed?;
        if if dir {
            before.binding(&observed)
        } else {
            before == &observed
        } {
            Ok(())
        } else {
            Err(Failure::Unavailable)
        }
    }
    pub fn finish(self) -> Result<()> {
        let verified = self.verify();
        let mut failed = false;
        for value in self.files {
            failed |= self.access.close(value.file).is_err();
        }
        for value in self.directories.into_iter().rev() {
            failed |= self.access.close(value.file).is_err();
        }
        verified?;
        if failed {
            Err(Failure::Unavailable)
        } else {
            Ok(())
        }
    }
}
