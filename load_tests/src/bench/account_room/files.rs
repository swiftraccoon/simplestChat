//! Fresh descriptor-anchored directories and bounded private files. Checks are
//! optimistic namespace observations, not a lock against later same-UID writes.

use anyhow::{Result, ensure};
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::Mode,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

const READ: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);
const DIR: OFlag = READ.union(OFlag::O_DIRECTORY);

fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file)?;
    Ok(())
}

pub(super) struct Directory {
    path: PathBuf,
    file: Option<File>,
    parent: Option<File>,
    stamp: Metadata,
    ancestors: Vec<(PathBuf, Metadata)>,
    created: Mutex<BTreeMap<String, Metadata>>,
}

impl Directory {
    pub fn new(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute() && path.components().count() <= 20,
            "invalid_account_room_output"
        );
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid_output_parent"))?;
        let mut current = File::from(open(Path::new("/"), DIR, Mode::empty())?);
        let mut ancestors = Vec::new();
        let admission: Result<()> = (|| {
            let mut visible = PathBuf::from("/");
            ancestors.push((visible.clone(), current.metadata()?));
            for part in parent.components() {
                let Component::Normal(name) = part else {
                    ensure!(
                        matches!(part, Component::RootDir),
                        "invalid_output_component"
                    );
                    continue;
                };
                let next = File::from(openat(&current, name, DIR, Mode::empty())?);
                let stamp = match next.metadata() {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        let _ = close(next);
                        return Err(error.into());
                    }
                };
                close(std::mem::replace(&mut current, next))?;
                visible.push(name);
                ensure!(
                    binding(&stamp, &fs::symlink_metadata(&visible)?),
                    "account_room_namespace_changed"
                );
                ancestors.push((visible.clone(), stamp));
            }
            Ok(())
        })();
        if let Err(error) = admission {
            let _ = close(current);
            return Err(error);
        }
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid_output_leaf"))?;
        let created = nix::sys::stat::mkdirat(&current, name, Mode::from_bits_truncate(0o700));
        if let Err(error) = created {
            let _ = close(current);
            return Err(error.into());
        }
        let file = match openat(&current, name, DIR, Mode::empty()) {
            Ok(file) => File::from(file),
            Err(error) => {
                let _ = close(current);
                return Err(error.into());
            }
        };
        let admitted = (|| -> Result<Metadata> {
            let stamp = file.metadata()?;
            ensure!(
                stamp.is_dir() && stamp.mode() & 0o7777 == 0o700,
                "account_room_output_refused"
            );
            Ok(stamp)
        })();
        let stamp = match admitted {
            Ok(stamp) => stamp,
            Err(error) => {
                let _ = close(file);
                let _ = close(current);
                return Err(error);
            }
        };
        let value = Self {
            path: path.to_owned(),
            file: Some(file),
            parent: Some(current),
            stamp,
            ancestors,
            created: Mutex::new(BTreeMap::new()),
        };
        let result = (|| -> Result<()> {
            value.check()?;
            let record = value.create(".creator", 0)?;
            ensure!(
                record.initial.uid() == value.stamp.uid(),
                "output_owner_mismatch"
            );
            record.finish(&value)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn descriptor(&self) -> &File {
        self.file.as_ref().expect("directory owner live")
    }
    pub fn check(&self) -> Result<()> {
        for (path, stamp) in &self.ancestors {
            ensure!(
                binding(stamp, &fs::symlink_metadata(path)?),
                "output_ancestor_changed"
            );
        }
        let retained = self.descriptor().metadata()?;
        let visible = fs::symlink_metadata(&self.path)?;
        ensure!(
            binding(&self.stamp, &retained)
                && binding(&self.stamp, &visible)
                && visible.is_dir()
                && visible.mode() & 0o7777 == 0o700,
            "account_room_namespace_changed"
        );
        Ok(())
    }
    pub fn create(&self, name: &str, maximum: u64) -> Result<Record> {
        ensure!(
            !name.is_empty() && !name.contains('/') && name.len() < 100,
            "invalid_account_room_basename"
        );
        self.check()?;
        let file = File::from(openat(
            self.descriptor(),
            name,
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let admission = (|| -> Result<Metadata> {
            let stamp = file.metadata()?;
            ensure!(
                stamp.is_file()
                    && stamp.nlink() == 1
                    && stamp.len() == 0
                    && stamp.mode() & 0o7777 == 0o600
                    && stamp.uid() == self.stamp.uid(),
                "account_room_file_refused"
            );
            self.check()?;
            Ok(stamp)
        })();
        match admission {
            Ok(initial) => {
                self.created
                    .lock()
                    .expect("creation registry")
                    .insert(name.into(), initial.clone());
                Ok(Record {
                    file: Some(file),
                    name: name.into(),
                    initial,
                    maximum,
                    expected: Some(Sha256::new()),
                })
            }
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    fn visible(&self, name: &str, expected: &Metadata) -> Result<()> {
        let file = File::from(openat(self.descriptor(), name, READ, Mode::empty())?);
        let observed = file.metadata();
        let closed = close(file);
        let observed = observed?;
        ensure!(same(expected, &observed), "account_room_file_changed");
        self.original(name, &observed)?;
        closed?;
        self.check()
    }
    fn original(&self, name: &str, observed: &Metadata) -> Result<()> {
        let created = self.created.lock().expect("creation registry");
        ensure!(
            created
                .get(name)
                .is_some_and(|initial| binding(initial, observed)),
            "account_room_file_replaced"
        );
        Ok(())
    }
    pub fn read(&self, name: &str, maximum: u64) -> Result<Vec<u8>> {
        self.check()?;
        ensure!(!name.contains('/'), "invalid_account_room_basename");
        let mut file = File::from(openat(self.descriptor(), name, READ, Mode::empty())?);
        let result = (|| -> Result<Vec<u8>> {
            let stamp = file.metadata()?;
            ensure!(
                stamp.is_file()
                    && stamp.nlink() == 1
                    && stamp.len() <= maximum
                    && stamp.uid() == self.stamp.uid()
                    && stamp.mode() & 0o7777 == 0o600,
                "account_room_input_refused"
            );
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(maximum + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 == stamp.len() && same(&stamp, &file.metadata()?),
                "account_room_file_changed"
            );
            self.visible(name, &stamp)?;
            Ok(bytes)
        })();
        let closed = close(file);
        let bytes = result?;
        closed?;
        Ok(bytes)
    }
    pub fn json(&self, name: &str, value: &impl serde::Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= 1024 * 1024, "account_room_report_limit");
        let mut file = self.create(name, 1024 * 1024)?;
        file.append(&bytes)?;
        file.finish(self).map(|_| ())
    }
    pub fn remove(&self, names: &[&str]) -> Result<()> {
        self.check()?;
        let mut result = Ok(());
        for name in names {
            let checked = self.read(name, 128 * 1024 * 1024);
            let removed = if checked.as_ref().err().is_some_and(|error| {
                error.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::ENOENT)
            }) {
                Ok(())
            } else {
                checked.and_then(|_| {
                    nix::unistd::unlinkat(
                        self.descriptor(),
                        *name,
                        nix::unistd::UnlinkatFlags::NoRemoveDir,
                    )?;
                    Ok(())
                })
            };
            if result.is_ok() {
                result = removed;
            }
        }
        let checked = self.check();
        result?;
        checked
    }
    pub fn finish(mut self) -> Result<()> {
        let checked = self.check();
        let closed = close(self.file.take().expect("directory owner live"));
        let parent_closed = close(self.parent.take().expect("parent owner live"));
        checked?;
        closed?;
        parent_closed
    }
    pub fn remove_directory(mut self) -> Result<()> {
        let removed = self.check().and_then(|_| {
            nix::unistd::unlinkat(
                self.parent.as_ref().expect("parent owner live"),
                self.path.file_name().expect("owned leaf"),
                nix::unistd::UnlinkatFlags::RemoveDir,
            )?;
            Ok(())
        });
        let closed = close(self.file.take().expect("directory owner live"));
        let parent_closed = close(self.parent.take().expect("parent owner live"));
        removed?;
        closed?;
        parent_closed
    }
}

pub(super) struct Record {
    file: Option<File>,
    name: String,
    initial: Metadata,
    maximum: u64,
    expected: Option<Sha256>,
}
impl Record {
    pub fn duplicate(&mut self) -> Result<File> {
        let file = self.file.as_ref().expect("record owner live").try_clone()?;
        self.expected = None;
        Ok(file)
    }
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let file = self.file.as_mut().expect("record owner live");
        let size = file.metadata()?.len();
        ensure!(
            bytes.len() as u64 <= self.maximum.saturating_sub(size),
            "account_room_artifact_limit"
        );
        file.write_all(bytes)?;
        if let Some(expected) = &mut self.expected {
            expected.update(bytes);
        }
        Ok(())
    }
    pub fn finish(mut self, directory: &Directory) -> Result<String> {
        let mut file = self.file.take().expect("record owner live");
        let result = (|| -> Result<String> {
            file.flush()?;
            file.sync_all()?;
            let stamp = file.metadata()?;
            ensure!(
                binding(&self.initial, &stamp) && stamp.nlink() == 1 && stamp.len() <= self.maximum,
                "account_room_file_changed"
            );
            file.seek(SeekFrom::Start(0))?;
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 65_536];
            let mut count = 0_u64;
            loop {
                let count_read = file.read(&mut buffer)?;
                if count_read == 0 {
                    break;
                }
                count += count_read as u64;
                ensure!(count <= self.maximum, "account_room_artifact_limit");
                digest.update(&buffer[..count_read]);
            }
            ensure!(
                count == stamp.len() && same(&stamp, &file.metadata()?),
                "account_room_file_changed"
            );
            directory.visible(&self.name, &stamp)?;
            let actual = digest.finalize();
            if let Some(expected) = self.expected.take() {
                ensure!(
                    actual == expected.finalize(),
                    "account_room_payload_changed"
                );
            }
            Ok(hex::encode(actual))
        })();
        let closed = close(file);
        let digest = result?;
        closed?;
        Ok(digest)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = close(file);
        }
        if let Some(file) = self.parent.take() {
            let _ = close(file);
        }
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = close(file);
        }
    }
}
