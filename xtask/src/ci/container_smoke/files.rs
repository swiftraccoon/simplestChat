//! Anchored source snapshots and exclusive private smoke artifacts; no extraction.

use super::{
    engine::failure,
    options::{self, Context},
    plan::{Asset, Role},
};
use crate::tooling::Result;
use rustix::fs::{Mode, OFlags};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
};

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

pub(super) const MAX_ASSET: u64 = 16 * 1024 * 1024;
pub(super) const MAX_LOG: u64 = 1024 * 1024;
const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}

struct Directory {
    files: Vec<File>,
    stamps: Vec<Metadata>,
    names: Vec<OsString>,
}
impl Directory {
    fn open(path: &Path) -> Result<Self> {
        if !options::safe_path(path) {
            return Err(failure());
        }
        let root = File::from(
            rustix::fs::open("/", FLAGS | OFlags::DIRECTORY, Mode::empty())
                .map_err(|_| failure())?,
        );
        let stamp = match root.metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                let _ = close(root);
                return Err(failure());
            }
        };
        let mut value = Self {
            files: vec![root],
            stamps: vec![stamp],
            names: vec![],
        };
        let admitted = (|| {
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) if value.names.len() < 32 => {
                        let file = File::from(
                            rustix::fs::openat(
                                value.file(),
                                name,
                                FLAGS | OFlags::DIRECTORY,
                                Mode::empty(),
                            )
                            .map_err(|_| failure())?,
                        );
                        let stamp = match file.metadata() {
                            Ok(stamp) => stamp,
                            Err(_) => {
                                let _ = close(file);
                                return Err(failure());
                            }
                        };
                        value.files.push(file);
                        value.stamps.push(stamp);
                        value.names.push(name.into());
                    }
                    _ => return Err(failure()),
                }
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    fn file(&self) -> &File {
        self.files.last().expect("root descriptor retained")
    }
    fn verify(&self) -> Result<()> {
        for (index, file) in self.files.iter().enumerate() {
            if !binding(
                &self.stamps[index],
                &file.metadata().map_err(|_| failure())?,
            ) {
                return Err(failure());
            }
        }
        for (index, name) in self.names.iter().enumerate() {
            let file = File::from(
                rustix::fs::openat(
                    &self.files[index],
                    name,
                    FLAGS | OFlags::DIRECTORY,
                    Mode::empty(),
                )
                .map_err(|_| failure())?,
            );
            let observed = file.metadata().map_err(|_| failure());
            let closed = close(file);
            if !binding(&self.stamps[index + 1], &observed?) {
                return Err(failure());
            }
            closed?;
        }
        Ok(())
    }
    fn private(&self) -> Result<()> {
        self.verify()?;
        let current = self.file().metadata().map_err(|_| failure())?;
        if current.mode() & 0o777 != 0o700 || current.uid() != rustix::process::getuid().as_raw() {
            return Err(failure());
        }
        Ok(())
    }
    fn names(&self) -> Result<Vec<OsString>> {
        self.verify()?;
        let mut names = Vec::new();
        for entry in rustix::fs::Dir::read_from(self.file()).map_err(|_| failure())? {
            let entry = entry.map_err(|_| failure())?;
            let name = entry.file_name().to_bytes();
            if [b".".as_slice(), b".."].contains(&name) {
                continue;
            }
            if names.len() >= 4096 {
                return Err(failure());
            }
            names.push(OsString::from_vec(name.to_vec()));
        }
        names.sort();
        self.verify()?;
        Ok(names)
    }
    fn finish(self) -> Result<()> {
        self.finish_with(close)
    }
    fn finish_with(self, mut closer: impl FnMut(File) -> Result<()>) -> Result<()> {
        let checked = self.verify();
        let mut failed = false;
        for file in self.files.into_iter().rev() {
            failed |= closer(file).is_err();
        }
        checked?;
        if failed { Err(failure()) } else { Ok(()) }
    }
    fn check(&self, name: &std::ffi::OsStr, before: &Metadata) -> Result<()> {
        self.verify()?;
        let file = File::from(
            rustix::fs::openat(self.file(), name, FLAGS, Mode::empty()).map_err(|_| failure())?,
        );
        let observed = file.metadata().map_err(|_| failure());
        let closed = close(file);
        if !same(before, &observed?) {
            return Err(failure());
        }
        closed
    }
    fn read(&self, name: &std::ffi::OsStr, maximum: u64) -> Result<Snapshot> {
        self.verify()?;
        let mut file = File::from(
            rustix::fs::openat(self.file(), name, FLAGS, Mode::empty()).map_err(|_| failure())?,
        );
        let admitted = (|| {
            let stamp = file.metadata().map_err(|_| failure())?;
            if !stamp.is_file() || stamp.len() > maximum {
                return Err(failure());
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(maximum + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| failure())?;
            if bytes.len() as u64 != stamp.len()
                || !same(&stamp, &file.metadata().map_err(|_| failure())?)
            {
                return Err(failure());
            }
            self.check(name, &stamp)?;
            Ok((stamp, bytes))
        })();
        match admitted {
            Ok((stamp, bytes)) => Ok(Snapshot {
                file: Some(file),
                stamp,
                bytes,
                name: name.into(),
            }),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    fn metadata(&self, name: &std::ffi::OsStr) -> Result<Snapshot> {
        self.verify()?;
        let file = File::from(
            rustix::fs::openat(self.file(), name, FLAGS, Mode::empty()).map_err(|_| failure())?,
        );
        let admitted = file.metadata().map_err(|_| failure()).and_then(|stamp| {
            if stamp.is_file() {
                self.check(name, &stamp).map(|()| stamp)
            } else {
                Err(failure())
            }
        });
        let closed = close(file);
        let stamp = admitted?;
        closed?;
        Ok(Snapshot {
            file: None,
            stamp,
            bytes: vec![],
            name: name.into(),
        })
    }
}

struct Snapshot {
    file: Option<File>,
    stamp: Metadata,
    bytes: Vec<u8>,
    name: OsString,
}
impl Snapshot {
    fn verify(&self, directory: &Directory) -> Result<()> {
        if let Some(file) = &self.file
            && !same(&self.stamp, &file.metadata().map_err(|_| failure())?)
        {
            return Err(failure());
        }
        directory.check(&self.name, &self.stamp)
    }
}

pub(super) struct Inputs {
    public: Directory,
    migrations: Directory,
    assets: BTreeMap<Asset, Snapshot>,
    migration_files: Vec<Snapshot>,
    migration_names: Vec<OsString>,
}
impl Inputs {
    pub fn read(root: &Path) -> Result<Self> {
        let public = Directory::open(&root.join("web/public"))?;
        let migrations = match Directory::open(&root.join("migrations")) {
            Ok(value) => value,
            Err(error) => {
                let _ = public.finish();
                return Err(error);
            }
        };
        let mut value = Self {
            public,
            migrations,
            assets: BTreeMap::new(),
            migration_files: vec![],
            migration_names: vec![],
        };
        let admitted = (|| {
            for asset in Asset::ALL {
                value.assets.insert(
                    asset,
                    value
                        .public
                        .read(std::ffi::OsStr::new(asset.name()), MAX_ASSET)?,
                );
            }
            value.migration_names = value.migrations.names()?;
            for name in &value.migration_names {
                if name.as_encoded_bytes().ends_with(b".sql") {
                    value.migration_files.push(value.migrations.metadata(name)?);
                }
            }
            if value.migration_files.is_empty() {
                return Err(failure());
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn asset(&self, asset: Asset) -> &[u8] {
        &self.assets[&asset].bytes
    }
    pub fn migrations(&self) -> usize {
        self.migration_files.len()
    }
    pub fn finish(self) -> Result<()> {
        let mut failed = self
            .migrations
            .names()
            .map_or(true, |names| names != self.migration_names);
        for snapshot in self.assets.into_values() {
            failed |= snapshot.verify(&self.public).is_err();
            if let Some(file) = snapshot.file {
                failed |= close(file).is_err();
            }
        }
        for snapshot in self.migration_files {
            failed |= snapshot.verify(&self.migrations).is_err();
            if let Some(file) = snapshot.file {
                failed |= close(file).is_err();
            }
        }
        failed |= self.public.finish().is_err();
        failed |= self.migrations.finish().is_err();
        if failed { Err(failure()) } else { Ok(()) }
    }
}

pub(super) struct Artifacts {
    pub path: PathBuf,
    pub token: String,
    directory: Directory,
}
impl Artifacts {
    pub fn new(context: &Context) -> Result<Self> {
        let mut random = File::open("/dev/urandom").map_err(|_| failure())?;
        let mut bytes = [0; 16];
        let read = random.read_exact(&mut bytes).map_err(|_| failure());
        let closed = close(random);
        read?;
        closed?;
        let token = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let selected = match &context.artifacts {
            Some(path) => path.clone(),
            None => context
                .temporary
                .join(format!("simplestchat-container-tests.{token}")),
        };
        // The selected temporary parent may legitimately use Darwin's /var
        // alias. Freeze its physical path before creating the private child;
        // source inputs continue to require a wholly nofollow path walk.
        let path = selected
            .parent()
            .ok_or_else(failure)?
            .canonicalize()
            .map_err(|_| failure())?
            .join(selected.file_name().ok_or_else(failure)?);
        let parent = Directory::open(path.parent().ok_or_else(failure)?)?;
        let name = path.file_name().ok_or_else(failure)?;
        let created = rustix::fs::mkdirat(parent.file(), name, Mode::from_bits_truncate(0o700));
        if created.is_err() {
            let _ = parent.finish();
            return Err(failure());
        }
        let directory = Directory::open(&path);
        let parent_closed = parent.finish();
        let directory = directory?;
        let admitted = directory
            .private()
            .and_then(|()| {
                if directory.names()?.is_empty() {
                    Ok(())
                } else {
                    Err(failure())
                }
            })
            .and(parent_closed);
        if let Err(error) = admitted {
            let _ = directory.finish();
            return Err(error);
        }
        Ok(Self {
            path,
            token,
            directory,
        })
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if !matches!(
            name,
            "ownership.json"
                | "database.id"
                | "app.id"
                | "rooms.json"
                | "index.html"
                | "manifest.webmanifest"
                | "sw.js"
                | "icon-192.png"
                | "icon-512.png"
                | "database.log"
                | "app.log"
                | "container-smoke.json"
        ) || bytes.len() as u64 > MAX_ASSET
        {
            return Err(failure());
        }
        self.directory.private()?;
        let mut file = File::from(
            rustix::fs::openat(
                self.directory.file(),
                name,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| failure())?,
        );
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| failure());
        let stamp = file.metadata().map_err(|_| failure());
        let closed = close(file);
        written?;
        closed?;
        self.directory.check(std::ffi::OsStr::new(name), &stamp?)?;
        self.directory.private()
    }
    pub fn create_log(&self, role: Role) -> Result<(File, Metadata)> {
        self.directory.private()?;
        let file = File::from(
            rustix::fs::openat(
                self.directory.file(),
                format!("{}.log", role.label()),
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| failure())?,
        );
        let admitted = file.metadata().map_err(|_| failure()).and_then(|stamp| {
            if !stamp.is_file()
                || stamp.len() != 0
                || stamp.mode() & 0o777 != 0o600
                || stamp.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure());
            }
            self.directory.check(
                std::ffi::OsStr::new(&format!("{}.log", role.label())),
                &stamp,
            )?;
            Ok(stamp)
        });
        match admitted {
            Ok(stamp) => Ok((file, stamp)),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    pub fn check_log(&self, role: Role, initial: &Metadata) -> Result<()> {
        self.directory.private()?;
        let name = format!("{}.log", role.label());
        let file = File::from(
            rustix::fs::openat(self.directory.file(), name.as_str(), FLAGS, Mode::empty())
                .map_err(|_| failure())?,
        );
        let observed = file.metadata().map_err(|_| failure());
        let closed = close(file);
        let stamp = observed?;
        if !binding(initial, &stamp) || !stamp.is_file() || stamp.len() > MAX_LOG {
            return Err(failure());
        }
        closed?;
        self.directory.check(std::ffi::OsStr::new(&name), &stamp)?;
        self.directory.private()
    }
    pub fn finish(self) -> Result<()> {
        self.directory.finish()
    }
}
