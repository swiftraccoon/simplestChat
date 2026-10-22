//! A fixed selected-input profile, not complete Cargo compiler I/O closure.
//! Shared directory owners avoid one ancestor chain per regular file. Every
//! retained File has an explicit attempted close; rustix's transient directory
//! cursor is internally RAII-closed (its API does not report close errors).

use super::Failure;
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::Mode,
};
use rustix::fs::Dir;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

const FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);
const MAX_ANCESTORS: usize = 32;
const MAX_DIRECTORIES: usize = 16;
const MAX_FILES: usize = 64;
const MAX_FILE: u64 = 1024 * 1024;
const MAX_LOCK: u64 = 4 * MAX_FILE;
const MAX_TOTAL: u64 = 16 * MAX_FILE;
const MAX_ENTRIES: usize = 8192;
const MAX_NAMES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 8;
const FIXED: [&str; 9] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "ops/Cargo.toml",
    "load_tests/Cargo.toml",
    "xtask/Cargo.toml",
    "vendor/mediasoup-0.29.0/Cargo.toml",
    "vendor/mediasoup-sys-0.19.0/Cargo.toml",
];

fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn close(file: File) -> Result<(), Failure> {
    nix::unistd::close(file).map_err(|_| Failure::Close)
}

/// Measured count, original byte total and canonical inventory digest only.
/// No filesystem paths or private names are exposed, even through Debug.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInventory {
    profile: &'static str,
    sha256: String,
    files: usize,
    bytes: u64,
}
impl SourceInventory {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn files(&self) -> usize {
        self.files
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

struct Directory {
    file: File,
    before: Metadata,
    parent: Option<usize>,
    name: OsString,
    entries: Option<Vec<Vec<u8>>>,
}
struct Input {
    file: File,
    before: Metadata,
    parent: usize,
    name: String,
    path: String,
    digest: [u8; 32],
}
struct Tree {
    directories: Vec<Directory>,
    selected: BTreeMap<String, usize>,
    root: usize,
    files: Vec<Input>,
    bytes: u64,
    entries: usize,
    names: usize,
}

impl Tree {
    fn open(path: &Path) -> Result<Self, Failure> {
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    if names.len() + 1 >= MAX_ANCESTORS || name.len() > 255 {
                        return Err(Failure::Bounds);
                    }
                    names.push(name.to_owned());
                }
                Component::RootDir | Component::CurDir => {}
                _ => return Err(Failure::Input),
            }
        }
        let start = if path.is_absolute() { "/" } else { "." };
        let first = File::from(
            open(start, FLAGS | OFlag::O_DIRECTORY, Mode::empty()).map_err(|_| Failure::Input)?,
        );
        let before = match first.metadata() {
            Ok(before) => before,
            Err(_) => {
                close(first)?;
                return Err(Failure::Input);
            }
        };
        let mut tree = Self {
            directories: vec![Directory {
                file: first,
                before,
                parent: None,
                name: OsString::new(),
                entries: None,
            }],
            selected: BTreeMap::new(),
            root: 0,
            files: Vec::new(),
            bytes: 0,
            entries: 0,
            names: 0,
        };
        let result = (|| {
            for name in names {
                let parent = tree.directories.len() - 1;
                tree.push_directory(parent, name)?;
            }
            tree.root = tree.directories.len() - 1;
            tree.selected.insert(String::new(), tree.root);
            tree.capture_entries(tree.root)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = tree.close_with(close);
            return Err(error);
        }
        Ok(tree)
    }

    fn push_directory(&mut self, parent: usize, name: OsString) -> Result<usize, Failure> {
        let file = File::from(
            openat(
                &self.directories[parent].file,
                name.as_os_str(),
                FLAGS | OFlag::O_DIRECTORY,
                Mode::empty(),
            )
            .map_err(|_| Failure::Namespace)?,
        );
        let before = match file.metadata() {
            Ok(before) => before,
            Err(_) => {
                close(file)?;
                return Err(Failure::Input);
            }
        };
        self.directories.push(Directory {
            file,
            before,
            parent: Some(parent),
            name,
            entries: None,
        });
        Ok(self.directories.len() - 1)
    }

    fn directory(&mut self, path: &str) -> Result<usize, Failure> {
        if let Some(index) = self.selected.get(path) {
            return Ok(*index);
        }
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        if self.selected.len() >= MAX_DIRECTORIES {
            return Err(Failure::Bounds);
        }
        let parent = self.directory(parent_path)?;
        if self.selected.len() >= MAX_DIRECTORIES {
            return Err(Failure::Bounds);
        }
        let index = self.push_directory(parent, name.into())?;
        self.selected.insert(path.into(), index);
        self.capture_entries(index)?;
        Ok(index)
    }

    fn names(file: &File) -> Result<Vec<Vec<u8>>, Failure> {
        let mut names = Vec::new();
        let mut bytes = 0_usize;
        for entry in Dir::read_from(file).map_err(|_| Failure::Input)? {
            let entry = entry.map_err(|_| Failure::Input)?;
            let name = entry.file_name().to_bytes();
            if [b".".as_slice(), b".."].contains(&name) {
                continue;
            }
            bytes = bytes.checked_add(name.len()).ok_or(Failure::Bounds)?;
            if names.len() >= MAX_ENTRIES || bytes > MAX_NAMES || name.len() > 255 {
                return Err(Failure::Bounds);
            }
            names.push(name.to_vec());
        }
        names.sort();
        if names.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Failure::Changed);
        }
        Ok(names)
    }

    fn capture_entries(&mut self, index: usize) -> Result<(), Failure> {
        let node = &self.directories[index];
        if !same(
            &node.before,
            &node.file.metadata().map_err(|_| Failure::Input)?,
        ) {
            return Err(Failure::Changed);
        }
        let entries = Self::names(&node.file)?;
        self.entries = self
            .entries
            .checked_add(entries.len())
            .ok_or(Failure::Bounds)?;
        self.names = entries
            .iter()
            .try_fold(self.names, |count, name| count.checked_add(name.len()))
            .ok_or(Failure::Bounds)?;
        if self.entries > MAX_ENTRIES || self.names > MAX_NAMES {
            return Err(Failure::Bounds);
        }
        if !same(
            &node.before,
            &node.file.metadata().map_err(|_| Failure::Input)?,
        ) {
            return Err(Failure::Changed);
        }
        self.directories[index].entries = Some(entries);
        Ok(())
    }

    fn refuse(&mut self, parent: &str, names: &[&str]) -> Result<(), Failure> {
        let index = self.directory(parent)?;
        if self.directories[index]
            .entries
            .as_ref()
            .unwrap()
            .iter()
            .any(|name| names.iter().any(|value| name == value.as_bytes()))
        {
            return Err(Failure::Namespace);
        }
        Ok(())
    }

    fn read(&mut self, path: &str) -> Result<(), Failure> {
        if self.files.len() >= MAX_FILES {
            return Err(Failure::Bounds);
        }
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.directory(parent_path)?;
        let file = File::from(
            openat(&self.directories[parent].file, name, FLAGS, Mode::empty())
                .map_err(|_| Failure::Input)?,
        );
        let result = (|| {
            let before = file.metadata().map_err(|_| Failure::Input)?;
            let maximum = if path == "Cargo.lock" {
                MAX_LOCK
            } else {
                MAX_FILE
            };
            if !before.is_file() || before.len() > maximum {
                return Err(Failure::Bounds);
            }
            let total = self
                .bytes
                .checked_add(before.len())
                .ok_or(Failure::Bounds)?;
            if total > MAX_TOTAL {
                return Err(Failure::Bounds);
            }
            Ok((before, total))
        })();
        let (before, total) = match result {
            Ok(value) => value,
            Err(error) => {
                let _ = close(file);
                return Err(error);
            }
        };
        self.files.push(Input {
            file,
            before,
            parent,
            name: name.into(),
            path: path.into(),
            digest: [0; 32],
        });
        self.bytes = total;
        let input = self.files.last_mut().unwrap();
        let mut digest = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0; 65536];
        loop {
            input.verify_stamp()?;
            let size = input.file.read(&mut buffer).map_err(|_| Failure::Input)?;
            input.verify_stamp()?;
            if size == 0 {
                break;
            }
            bytes = bytes.checked_add(size as u64).ok_or(Failure::Bounds)?;
            if bytes > input.before.len() {
                return Err(Failure::Changed);
            }
            digest.update(&buffer[..size]);
        }
        if bytes != input.before.len() {
            return Err(Failure::Changed);
        }
        input.digest = digest.finalize().into();
        Ok(())
    }

    fn source(&mut self, path: &str, depth: usize) -> Result<(), Failure> {
        if depth > MAX_DEPTH {
            return Err(Failure::Bounds);
        }
        let index = self.directory(path)?;
        let names = self.directories[index].entries.as_ref().unwrap().clone();
        for name in names {
            let name = std::str::from_utf8(&name).map_err(|_| Failure::Namespace)?;
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || b"_.-".contains(&v))
                || !name.as_bytes()[0].is_ascii_alphanumeric()
            {
                return Err(Failure::Namespace);
            }
            let child = format!("{path}/{name}");
            let metadata = rustix::fs::statat(
                &self.directories[index].file,
                name,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|_| Failure::Input)?;
            match rustix::fs::FileType::from_raw_mode(metadata.st_mode) {
                rustix::fs::FileType::Directory => self.source(&child, depth + 1)?,
                rustix::fs::FileType::RegularFile if name.ends_with(".rs") => {
                    self.read(&child)?;
                }
                _ => return Err(Failure::Namespace),
            }
        }
        Ok(())
    }

    fn verify(&self) -> Result<(), Failure> {
        for node in &self.directories {
            let unchanged = |value: &Metadata| {
                if node.entries.is_some() {
                    same(&node.before, value)
                } else {
                    binding(&node.before, value)
                }
            };
            if !unchanged(&node.file.metadata().map_err(|_| Failure::Input)?) {
                return Err(Failure::Changed);
            }
            if let Some(parent) = node.parent {
                let visible = File::from(
                    openat(
                        &self.directories[parent].file,
                        node.name.as_os_str(),
                        FLAGS | OFlag::O_DIRECTORY,
                        Mode::empty(),
                    )
                    .map_err(|_| Failure::Changed)?,
                );
                let unchanged = visible.metadata().is_ok_and(|value| unchanged(&value));
                let closed = close(visible);
                if !unchanged {
                    return Err(Failure::Changed);
                }
                closed?;
            }
            if let Some(entries) = &node.entries
                && (*entries != Self::names(&node.file)?
                    || !same(
                        &node.before,
                        &node.file.metadata().map_err(|_| Failure::Input)?,
                    ))
            {
                return Err(Failure::Changed);
            }
        }
        for input in &self.files {
            input.verify_stamp()?;
            let visible = File::from(
                openat(
                    &self.directories[input.parent].file,
                    input.name.as_str(),
                    FLAGS,
                    Mode::empty(),
                )
                .map_err(|_| Failure::Changed)?,
            );
            let unchanged = visible
                .metadata()
                .is_ok_and(|value| same(&input.before, &value));
            let closed = close(visible);
            if !unchanged {
                return Err(Failure::Changed);
            }
            closed?;
        }
        Ok(())
    }

    fn inventory(&self) -> SourceInventory {
        let mut inputs: Vec<_> = self.files.iter().collect();
        inputs.sort_by(|a, b| a.path.cmp(&b.path));
        let mut digest = Sha256::new();
        digest.update(b"simplestchat-ops-source-inventory-v1\0");
        digest.update((inputs.len() as u32).to_be_bytes());
        for input in inputs {
            digest.update((input.path.len() as u16).to_be_bytes());
            digest.update(input.path.as_bytes());
            digest.update(input.before.len().to_be_bytes());
            digest.update(input.digest);
        }
        SourceInventory {
            profile: "workspace-ops-v1",
            sha256: digest
                .finalize()
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect(),
            files: self.files.len(),
            bytes: self.bytes,
        }
    }

    fn close_with(self, mut close: impl FnMut(File) -> Result<(), Failure>) -> Result<(), Failure> {
        let mut failed = false;
        for file in self.files {
            failed |= close(file.file).is_err();
        }
        for directory in self.directories.into_iter().rev() {
            failed |= close(directory.file).is_err();
        }
        if failed { Err(Failure::Close) } else { Ok(()) }
    }
}
impl Input {
    fn verify_stamp(&self) -> Result<(), Failure> {
        if same(
            &self.before,
            &self.file.metadata().map_err(|_| Failure::Input)?,
        ) {
            Ok(())
        } else {
            Err(Failure::Changed)
        }
    }
}

/// Hash the fixed discovery manifests/config and every Rust file under ops/src.
/// Unselected workspace/vendor source is not a compiler-input closure. The tree
/// retains at most 32 ancestor, 16 selected-directory and 64 regular-file owners
/// (112), plus one enumeration cursor or visible-binding descriptor at a time.
/// Membership/stamp/binding checks detect observed changes, not atomic snapshots.
pub fn collect_inputs(source_root: &Path) -> Result<SourceInventory, Failure> {
    collect_after(source_root, |_| {})
}

fn collect_after(
    source_root: &Path,
    after: impl FnOnce(&SourceInventory),
) -> Result<SourceInventory, Failure> {
    let mut tree = Tree::open(source_root)?;
    let result = (|| {
        tree.refuse(".cargo", &["config"])?;
        tree.refuse("ops", &["build.rs", ".cargo"])?;
        for path in FIXED {
            tree.read(path)?;
        }
        tree.source("ops/src", 0)?;
        if tree.files.len() == FIXED.len() {
            return Err(Failure::Namespace);
        }
        let inventory = tree.inventory();
        after(&inventory);
        tree.verify()?;
        Ok(inventory)
    })();
    let closed = tree.close_with(close);
    let inventory = result?;
    closed?;
    Ok(inventory)
}

#[cfg(test)]
#[path = "distribution_inventory_tests.rs"]
mod tests;
