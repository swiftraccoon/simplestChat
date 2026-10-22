//! Retained source, temporary captures and shared private artifact namespace.
use super::{config::Config, failure, policy::LIMIT};
use crate::{
    ci::{bounded_log::Log, command::SeparateLogs, release_controller::files::Directory},
    tooling::Result,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    io::{Read, Seek, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

fn mapped<T>(value: impl Into<std::result::Result<T, std::io::Error>>) -> Result<T> {
    value.into().map_err(|_| failure(1))
}
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure(1))
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn private(directory: &Directory) -> Result<()> {
    let m = mapped(directory.file().metadata())?;
    if m.mode() & 0o7777 != 0o700 || m.uid() != rustix::process::getuid().as_raw() {
        return Err(failure(1));
    }
    directory.verify().map_err(|_| failure(1))
}
fn open(parent: &Directory, name: &str) -> Result<File> {
    rustix::fs::openat(
        parent.file(),
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| failure(1))
}
fn nonce() -> Result<[u8; 16]> {
    let mut file = mapped(File::open("/dev/urandom"))?;
    let mut bytes = [0; 16];
    let read = mapped(file.read_exact(&mut bytes));
    let closed = close(file);
    read?;
    closed?;
    Ok(bytes)
}
pub(super) fn uuid() -> Result<String> {
    let mut b = nonce()?;
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    let hex = b.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
pub(super) struct Source {
    directory: Directory,
    file: File,
    before: Metadata,
    pub path: PathBuf,
    pub sha256: String,
}
impl Source {
    pub fn new(root: &Path) -> Result<Self> {
        let directory = Directory::open(&root.join("build")).map_err(|_| failure(1))?;
        let file = match open(&directory, "test-mdns.c") {
            Ok(file) => file,
            Err(error) => {
                let _ = directory.finish();
                return Err(error);
            }
        };
        let admitted = mapped(file.metadata()).and_then(|m| {
            if !m.is_file() || m.len() == 0 || m.len() > 64 * 1024 {
                Err(failure(1))
            } else {
                Ok(m)
            }
        });
        let before = match admitted {
            Ok(m) => m,
            Err(e) => {
                let _ = close(file);
                let _ = directory.finish();
                return Err(e);
            }
        };
        let mut source = Self {
            directory,
            file,
            before,
            path: root.join("build/test-mdns.c"),
            sha256: String::new(),
        };
        match source.hash() {
            Ok(hash) => {
                source.sha256 = hash;
                Ok(source)
            }
            Err(e) => {
                let _ = source.finish();
                Err(e)
            }
        }
    }
    fn hash(&mut self) -> Result<String> {
        self.directory.verify().map_err(|_| failure(1))?;
        self.directory
            .check("test-mdns.c", &self.before)
            .map_err(|_| failure(1))?;
        if !same(&self.before, &mapped(self.file.metadata())?) {
            return Err(failure(1));
        }
        mapped(self.file.rewind())?;
        let mut bytes = Vec::new();
        mapped(
            Read::by_ref(&mut self.file)
                .take(self.before.len() + 1)
                .read_to_end(&mut bytes),
        )?;
        if bytes.len() as u64 != self.before.len()
            || !same(&self.before, &mapped(self.file.metadata())?)
        {
            return Err(failure(1));
        }
        self.directory
            .check("test-mdns.c", &self.before)
            .map_err(|_| failure(1))?;
        Ok(Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
    pub fn verify(&mut self) -> Result<()> {
        if self.hash()? != self.sha256 {
            return Err(failure(1));
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let checked = if self.sha256.is_empty() {
            Ok(())
        } else {
            self.verify()
        };
        let file = close(self.file);
        let directory = self.directory.finish().map_err(|_| failure(1));
        checked.and(file).and(directory)
    }
}
pub(super) struct Output {
    directory: Directory,
}
impl Output {
    pub fn new(path: &Path) -> Result<Self> {
        let mut directory = Directory::open(Path::new("/")).map_err(|_| failure(1))?;
        for component in path.components() {
            let Component::Normal(name) = component else {
                if component == Component::RootDir {
                    continue;
                }
                let _ = directory.finish();
                return Err(failure(1));
            };
            let Some(name) = name.to_str() else {
                let _ = directory.finish();
                return Err(failure(1));
            };
            if !matches!(
                rustix::fs::mkdirat(directory.file(), name, Mode::from_bits_truncate(0o700)),
                Ok(()) | Err(rustix::io::Errno::EXIST)
            ) {
                let _ = directory.finish();
                return Err(failure(1));
            }
            directory = directory.descend(name).map_err(|_| failure(1))?;
        }
        if let Err(e) = private(&directory) {
            let _ = directory.finish();
            return Err(e);
        }
        Ok(Self { directory })
    }
    pub fn publish(&self, phase: &str, bytes: &[u8]) -> Result<()> {
        private(&self.directory)?;
        if bytes.len() > 128 * 1024 {
            return Err(failure(1));
        }
        let name = match phase {
            "preflight" => "mdns-preflight-results.json",
            "snapshot" => "mdns-snapshot-results.json",
            _ => return Err(failure(1)),
        };
        let mut file = self.directory.create(name).map_err(|_| failure(1))?;
        let written = (|| {
            let before = mapped(file.metadata())?;
            if before.mode() & 0o7777 != 0o600
                || before.nlink() != 1
                || before.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure(1));
            }
            mapped(file.write_all(bytes))?;
            mapped(file.sync_all())?;
            let after = mapped(file.metadata())?;
            if !binding(&before, &after) || after.len() != bytes.len() as u64 {
                return Err(failure(1));
            }
            self.directory.check(name, &after).map_err(|_| failure(1))?;
            Ok(())
        })();
        let closed = close(file);
        written?;
        closed?;
        if self
            .directory
            .read(name, 128 * 1024)
            .map_err(|_| failure(1))?
            .as_deref()
            != Some(bytes)
        {
            return Err(failure(1));
        }
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        self.directory.finish().map_err(|_| failure(1))
    }
}
pub(super) struct Temporary {
    parent: Directory,
    directory: Directory,
    name: String,
    entries: BTreeMap<String, Metadata>,
    pub path: PathBuf,
}
impl Temporary {
    pub fn new(config: &Config) -> Result<Self> {
        Self::new_after(config, |_| {})
    }
    pub(super) fn new_after(config: &Config, after: impl FnOnce(&Path)) -> Result<Self> {
        let bytes = nonce()?;
        let name = format!(
            "simplestchat-mdns.{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let parent = Directory::open(&config.temporary).map_err(|_| failure(1))?;
        if rustix::fs::mkdirat(
            parent.file(),
            name.as_str(),
            Mode::from_bits_truncate(0o700),
        )
        .is_err()
        {
            let _ = parent.finish();
            return Err(failure(1));
        }
        after(&config.temporary.join(&name));
        let directory = match Directory::open(&config.temporary).and_then(|p| p.descend(&name)) {
            Ok(v) => v,
            Err(_) => {
                let _ = parent.finish();
                return Err(failure(1));
            }
        };
        let temp = Self {
            parent,
            directory,
            name: name.clone(),
            entries: BTreeMap::new(),
            path: config.temporary.join(name),
        };
        if let Err(e) = private(&temp.directory) {
            // No positive private admission exists. Closing is authorized;
            // deleting a replacement or special-mode leaf is not.
            let _ = temp.directory.finish();
            let _ = temp.parent.finish();
            return Err(e);
        }
        Ok(temp)
    }
    pub async fn logs(&mut self, index: usize) -> Result<SeparateLogs> {
        private(&self.directory)?;
        let mut logs = Vec::new();
        for stream in ["stdout", "stderr"] {
            let name = format!("command{index}.{stream}");
            let admitted = (|| {
                let file = self.directory.create(&name).map_err(|_| failure(1))?;
                let metadata = match mapped(file.metadata()) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = close(file);
                        return Err(e);
                    }
                };
                if !metadata.is_file()
                    || metadata.mode() & 0o7777 != 0o600
                    || metadata.uid() != rustix::process::getuid().as_raw()
                    || metadata.nlink() != 1
                {
                    let _ = close(file);
                    return Err(failure(1));
                }
                self.entries.insert(name, metadata);
                Log::start(file, LIMIT)
            })();
            match admitted {
                Ok(log) => logs.push(log),
                Err(e) => {
                    for log in logs {
                        let _ = log.finish().await;
                    }
                    return Err(e);
                }
            }
        }
        let stderr = logs.pop().unwrap();
        let stdout = logs.pop().unwrap();
        Ok(SeparateLogs { stdout, stderr })
    }
    pub fn read(&self, index: usize) -> Result<Vec<u8>> {
        let name = format!("command{index}.stdout");
        let before = self.entries.get(&name).ok_or_else(|| failure(1))?;
        let mut file = open(&self.directory, &name)?;
        let read = (|| {
            let observed = mapped(file.metadata())?;
            if !binding(before, &observed) || observed.len() > LIMIT {
                return Err(failure(1));
            }
            let mut bytes = Vec::new();
            mapped(
                Read::by_ref(&mut file)
                    .take(observed.len() + 1)
                    .read_to_end(&mut bytes),
            )?;
            if bytes.len() as u64 != observed.len() || !same(&observed, &mapped(file.metadata())?) {
                return Err(failure(1));
            }
            self.directory
                .check(&name, &observed)
                .map_err(|_| failure(1))?;
            Ok(bytes)
        })();
        let closed = close(file);
        read.and_then(|v| closed.map(|()| v))
    }
    pub fn probe(&mut self, executable: bool) -> Result<Option<PathBuf>> {
        let file = match open(&self.directory, "probe") {
            Ok(v) => v,
            Err(e) => {
                if matches!(
                    rustix::fs::statat(self.directory.file(), "probe", AtFlags::SYMLINK_NOFOLLOW),
                    Err(rustix::io::Errno::NOENT)
                ) {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        let checked = (|| {
            let m = mapped(file.metadata())?;
            if !m.is_file()
                || m.nlink() != 1
                || m.uid() != rustix::process::getuid().as_raw()
                || m.mode() & 0o7000 != 0
                || (executable && m.mode() & 0o111 == 0)
            {
                return Err(failure(1));
            }
            self.directory.check("probe", &m).map_err(|_| failure(1))?;
            Ok(m)
        })();
        let closed = close(file);
        let metadata = checked?;
        closed?;
        self.entries.insert("probe".into(), metadata);
        Ok(Some(self.path.join("probe")))
    }
    pub fn verify_probe(&self) -> Result<()> {
        let expected = self.entries.get("probe").ok_or_else(|| failure(1))?;
        let file = open(&self.directory, "probe")?;
        let checked = mapped(file.metadata()).and_then(|m| {
            if !same(expected, &m) {
                return Err(failure(1));
            }
            self.directory
                .check("probe", expected)
                .map_err(|_| failure(1))
        });
        let closed = close(file);
        checked.and(closed)
    }
    pub fn finish(self) -> Result<()> {
        let mut result = private(&self.directory);
        // Only fixed admitted entries may be removed; never recursive cleanup.
        for (name, before) in &self.entries {
            let removed = (|| {
                let file = open(&self.directory, name)?;
                let observed = mapped(file.metadata());
                let closed = close(file);
                let observed = observed?;
                if !binding(before, &observed) {
                    return Err(failure(1));
                }
                closed?;
                self.directory
                    .check(name, &observed)
                    .map_err(|_| failure(1))?;
                rustix::fs::unlinkat(self.directory.file(), name.as_str(), AtFlags::empty())
                    .map_err(|_| failure(1))
            })();
            result = result.and(removed);
        }
        // A compiler may have produced a partial probe on failure. It is not
        // adopted for cleanup: an unknown residual entry makes rmdir fail.
        let checked = self
            .parent
            .verify()
            .map_err(|_| failure(1))
            .and(self.directory.verify().map_err(|_| failure(1)));
        let retained = rustix::fs::openat(
            self.parent.file(),
            self.name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure(1));
        let identity = retained.as_ref().map_err(|_| failure(1)).and_then(|f| {
            if !binding(
                &mapped(f.metadata())?,
                &mapped(self.directory.file().metadata())?,
            ) {
                return Err(failure(1));
            }
            Ok(())
        });
        let directory = self.directory.finish().map_err(|_| failure(1));
        let removed = if checked.is_ok() && identity.is_ok() {
            rustix::fs::unlinkat(self.parent.file(), self.name.as_str(), AtFlags::REMOVEDIR)
                .map_err(|_| failure(1))
        } else {
            Err(failure(1))
        };
        let absent = if removed.is_ok() {
            match rustix::fs::statat(
                self.parent.file(),
                self.name.as_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Err(rustix::io::Errno::NOENT) => Ok(()),
                _ => Err(failure(1)),
            }
        } else {
            Err(failure(1))
        };
        let retained = retained.and_then(close);
        let parent = self.parent.finish().map_err(|_| failure(1));
        result = result
            .and(checked)
            .and(identity)
            .and(directory)
            .and(removed)
            .and(absent)
            .and(retained)
            .and(parent);
        result
    }
}
