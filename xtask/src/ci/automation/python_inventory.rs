//! Source-layout admission for the fixed two-process unittest profile.
//! Metadata witnesses detect observed inventory changes; they do not freeze
//! Python imports against privileged same-UID swap/restore races.

use super::python_shards::{Cause, Outcome};
use rustix::fs::{Mode, OFlags};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn new(value: &Metadata) -> Self {
        Self {
            device: value.dev(),
            inode: value.ino(),
            mode: value.mode(),
            bytes: value.len(),
            modified: (value.mtime(), value.mtime_nsec()),
            changed: (value.ctime(), value.ctime_nsec()),
        }
    }
}

pub(super) struct Inventory {
    path: PathBuf,
    original: Option<File>,
    files: BTreeMap<String, Stamp>,
    pub(super) groups: [Vec<String>; 2],
}
impl Inventory {
    pub(super) fn read(root: &Path) -> Outcome<Self> {
        let path = root.join("ops/ansible/tests");
        let original = File::from(
            rustix::fs::open(
                &path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Cause::Inventory)?,
        );
        let mut result = Self {
            path,
            original: Some(original),
            files: BTreeMap::new(),
            groups: [Vec::new(), Vec::new()],
        };
        let admitted = result.scan().and_then(|files| {
            let names = files
                .keys()
                .filter(|name| name.starts_with("test") && name.ends_with(".py"))
                .collect::<Vec<_>>();
            if names.len() < 2 || names.len() > 1024 {
                return Err(Cause::Inventory);
            }
            for (ordinal, name) in names.into_iter().enumerate() {
                let module = name.strip_suffix(".py").ok_or(Cause::Inventory)?;
                if !valid_module(module) {
                    return Err(Cause::Inventory);
                }
                result.groups[ordinal % 2].push(module.to_owned());
            }
            result.files = files;
            Ok(())
        });
        if let Err(first) = admitted {
            let _ = result.close();
            return Err(first);
        }
        Ok(result)
    }

    fn scan(&self) -> Outcome<BTreeMap<String, Stamp>> {
        let original = self.original.as_ref().ok_or(Cause::Inventory)?;
        let held = original.metadata().map_err(|_| Cause::Inventory)?;
        let current = fs::symlink_metadata(&self.path).map_err(|_| Cause::Inventory)?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || (current.dev(), current.ino()) != (held.dev(), held.ino())
        {
            return Err(Cause::Inventory);
        }
        let mut files = BTreeMap::new();
        for (ordinal, entry) in fs::read_dir(&self.path)
            .map_err(|_| Cause::Inventory)?
            .enumerate()
        {
            if ordinal >= 4096 {
                return Err(Cause::Inventory);
            }
            let entry = entry.map_err(|_| Cause::Inventory)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Cause::Inventory)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| Cause::Inventory)?;
            if metadata.file_type().is_symlink() {
                return Err(Cause::Inventory);
            }
            if metadata.is_dir() {
                if name != "__pycache__"
                    || entry
                        .path()
                        .join("__init__.py")
                        .try_exists()
                        .map_err(|_| Cause::Inventory)?
                {
                    return Err(Cause::Inventory);
                }
                continue;
            }
            if !metadata.is_file() || files.insert(name, Stamp::new(&metadata)).is_some() {
                return Err(Cause::Inventory);
            }
        }
        let current = fs::symlink_metadata(&self.path).map_err(|_| Cause::Inventory)?;
        if !current.is_dir() || (current.dev(), current.ino()) != (held.dev(), held.ino()) {
            return Err(Cause::Inventory);
        }
        Ok(files)
    }

    pub(super) fn current(&self) -> Outcome<()> {
        if self.scan()? != self.files {
            return Err(Cause::Inventory);
        }
        Ok(())
    }

    pub(super) fn close(&mut self) -> Outcome<()> {
        self.original
            .take()
            .map(nix::unistd::close)
            .transpose()
            .map(|_| ())
            .map_err(|_| Cause::Close)
    }
}

fn valid_module(value: &str) -> bool {
    value.len() <= 128
        && value.strip_prefix("test_").is_some_and(|rest| {
            rest.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}
