//! Requirements bytes and stamps remain bound through setup, including restores.
use super::{
    files::{self, Directory},
    plan,
};
use crate::tooling::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    modified: i64,
    modified_ns: i64,
    changed: i64,
    changed_ns: i64,
}
impl Stamp {
    fn new(v: &Metadata) -> Self {
        Self {
            device: v.dev(),
            inode: v.ino(),
            bytes: v.len(),
            mode: v.mode(),
            uid: v.uid(),
            gid: v.gid(),
            modified: v.mtime(),
            modified_ns: v.mtime_nsec(),
            changed: v.ctime(),
            changed_ns: v.ctime_nsec(),
        }
    }
}
pub(super) struct Source {
    parent: Directory,
    file: File,
    name: String,
    before: Metadata,
    pub stamp: Stamp,
    pub sha256: String,
}
impl Source {
    pub fn open(path: &Path, maximum: u64) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(plan::failure)?
            .to_owned();
        let parent = Directory::open(path.parent().ok_or_else(plan::failure)?)?;
        let file = rustix::fs::openat(
            parent.file(),
            name.as_str(),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map(File::from);
        let mut file = match file {
            Ok(file) => file,
            Err(_) => {
                let _ = parent.finish();
                return Err(plan::failure());
            }
        };
        let result = (|| {
            let before = file.metadata().map_err(|_| plan::failure())?;
            if !before.is_file() || before.len() == 0 || before.len() > maximum {
                return Err(plan::failure());
            }
            let mut digest = Sha256::new();
            let mut bytes = 0_u64;
            let mut buffer = [0; 65536];
            loop {
                if !files::same(&before, &file.metadata().map_err(|_| plan::failure())?) {
                    return Err(plan::failure());
                }
                let count = file.read(&mut buffer).map_err(|_| plan::failure())?;
                if !files::same(&before, &file.metadata().map_err(|_| plan::failure())?) {
                    return Err(plan::failure());
                }
                if count == 0 {
                    break;
                }
                bytes += count as u64;
                if bytes > before.len() {
                    return Err(plan::failure());
                }
                digest.update(&buffer[..count]);
            }
            if bytes != before.len() {
                return Err(plan::failure());
            }
            parent.check(&name, &before)?;
            let sha256 = digest
                .finalize()
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect();
            Ok((before, sha256))
        })();
        match result {
            Ok((before, sha256)) => Ok(Self {
                parent,
                file,
                name,
                stamp: Stamp::new(&before),
                before,
                sha256,
            }),
            Err(error) => {
                let _ = files::close(file);
                let _ = parent.finish();
                Err(error)
            }
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !files::same(
            &self.before,
            &self.file.metadata().map_err(|_| plan::failure())?,
        ) {
            return Err(plan::failure());
        }
        self.parent.check(&self.name, &self.before)
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.validate();
        let closed = files::close(self.file);
        let parent = self.parent.finish();
        checked.and(closed).and(parent)
    }
}
