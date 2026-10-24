//! Original source/tool witnesses and an owned, nonlink Git archive extraction.

use super::{
    control::{Failure, Owner, Result},
    files::{self, Dir, Held, Output, same},
    options::hex,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    time::Instant,
};

pub(super) type Environment = BTreeMap<OsString, OsString>;
const SOURCE_ENTRIES: usize = 100_000;
const SOURCE_NAMES: usize = 16 * 1024 * 1024;
#[derive(Default)]
struct SourceBudget {
    entries: usize,
    names: usize,
}
impl SourceBudget {
    fn admit(&mut self, path: &Path) -> Result<()> {
        let entries = self.entries.checked_add(1).ok_or(Failure::Bounds)?;
        let names = self
            .names
            .checked_add(path.as_os_str().as_bytes().len())
            .ok_or(Failure::Bounds)?;
        if entries > SOURCE_ENTRIES || names > SOURCE_NAMES {
            return Err(Failure::Bounds);
        }
        self.entries = entries;
        self.names = names;
        Ok(())
    }
}
#[cfg(test)]
thread_local! {
    static EXPIRE_MIGRATION:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};
    static EXTRA_BEFORE_SEAL:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};
    static REFUSE_CREATED: std::cell::Cell<Option<FixtureCreated>> =
        const { std::cell::Cell::new(None) };
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FixtureCreated {
    Directory,
    File,
}
#[cfg(test)]
pub(super) fn fixture_refuse_created_once(kind: FixtureCreated) {
    REFUSE_CREATED.with(|flag| flag.set(Some(kind)));
}
#[cfg(test)]
fn fixture_created(kind: FixtureCreated) -> Result<()> {
    if REFUSE_CREATED.with(|flag| {
        if flag.get() == Some(kind) {
            flag.set(None);
            true
        } else {
            false
        }
    }) {
        Err(Failure::Io)
    } else {
        Ok(())
    }
}
#[cfg(test)]
pub(super) fn fixture_expire_migration_hash_once() {
    EXPIRE_MIGRATION.with(|flag| flag.set(true));
}
#[cfg(test)]
pub(super) fn fixture_source_extra_before_seal_once() {
    EXTRA_BEFORE_SEAL.with(|flag| flag.set(true));
}

struct AliasPart {
    path: PathBuf,
    stamp: Metadata,
    link: Option<PathBuf>,
}
pub(super) struct Alias {
    original: PathBuf,
    canonical: PathBuf,
    parts: Vec<AliasPart>,
}
impl Alias {
    pub fn open(path: &Path, owner: &mut Owner, end: Instant) -> Result<Self> {
        owner.before(end)?;
        let canonical = path.canonicalize().map_err(|_| Failure::Io)?;
        let mut todo: VecDeque<OsString> = path
            .components()
            .map(|component| component.as_os_str().to_owned())
            .collect();
        let mut current = PathBuf::new();
        let mut parts = Vec::new();
        let mut followed = 0;
        while let Some(component) = todo.pop_front() {
            owner.before(end)?;
            if component == OsStr::new("/") {
                current = PathBuf::from("/");
                continue;
            }
            if component == OsStr::new(".") {
                continue;
            }
            if component == OsStr::new("..") {
                current.pop();
                continue;
            }
            current.push(&component);
            let stamp = current.symlink_metadata().map_err(|_| Failure::Io)?;
            if stamp.file_type().is_symlink() {
                followed += 1;
                // canonicalize above already enforces the native symlink limit.
                // Refuse a concurrent new loop instead of collecting it forever.
                if followed > 40 {
                    return Err(Failure::Changed);
                }
                let target = std::fs::read_link(&current).map_err(|_| Failure::Io)?;
                parts.push(AliasPart {
                    path: current.clone(),
                    stamp,
                    link: Some(target.clone()),
                });
                current.pop();
                for component in target.components().rev() {
                    todo.push_front(component.as_os_str().to_owned());
                }
            } else {
                parts.push(AliasPart {
                    path: current.clone(),
                    stamp,
                    link: None,
                });
            }
        }
        if current != canonical {
            return Err(Failure::Changed);
        }
        let alias = Self {
            original: path.into(),
            canonical,
            parts,
        };
        alias.check()?;
        Ok(alias)
    }
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
    pub fn check(&self) -> Result<()> {
        for part in &self.parts {
            let stamp = part.path.symlink_metadata().map_err(|_| Failure::Changed)?;
            let before = &part.stamp;
            let identity = stamp.file_type() == before.file_type()
                && stamp.dev() == before.dev()
                && stamp.ino() == before.ino()
                && stamp.mode() == before.mode()
                && stamp.uid() == before.uid()
                && stamp.gid() == before.gid();
            let contents = before.is_dir()
                || (stamp.len() == before.len()
                    && stamp.nlink() == before.nlink()
                    && stamp.mtime() == before.mtime()
                    && stamp.mtime_nsec() == before.mtime_nsec()
                    && stamp.ctime() == before.ctime()
                    && stamp.ctime_nsec() == before.ctime_nsec());
            if !identity || !contents {
                return Err(Failure::Changed);
            }
            if let Some(target) = &part.link
                && std::fs::read_link(&part.path).map_err(|_| Failure::Changed)? != *target
            {
                return Err(Failure::Changed);
            }
        }
        if self.original.canonicalize().map_err(|_| Failure::Changed)? != self.canonical {
            return Err(Failure::Changed);
        }
        Ok(())
    }
}

pub(super) struct Tool {
    alias: Alias,
    path: PathBuf,
    parent: Dir,
    file: Held,
}
impl Tool {
    pub fn resolve(
        name: &str,
        root: &Path,
        environment: &Environment,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<Self> {
        let paths = environment.get(OsStr::new("PATH")).ok_or(Failure::Input)?;
        let alias = std::env::split_paths(paths)
            .map(|directory| {
                if directory.is_absolute() {
                    directory.join(name)
                } else {
                    root.join(directory).join(name)
                }
            })
            .find(|path| {
                path.is_file()
                    && rustix::fs::accessat(
                        rustix::fs::CWD,
                        path.as_path(),
                        rustix::fs::Access::EXEC_OK,
                        rustix::fs::AtFlags::EACCESS,
                    )
                    .is_ok()
            })
            .ok_or(Failure::Input)?;
        let alias = if alias.is_absolute() {
            alias
        } else {
            std::env::current_dir()
                .map_err(|_| Failure::Io)?
                .join(alias)
        };
        let alias = Alias::open(&alias, owner, end)?;
        let path = alias.canonical().to_owned();
        let parent = Dir::open(path.parent().ok_or(Failure::Input)?)?;
        let mut file = match parent.read(path.file_name().ok_or(Failure::Input)?) {
            Ok(file) => file,
            Err(error) => {
                let _ = parent.finish();
                return Err(error);
            }
        };
        let admitted = (|| {
            if file.file().metadata().map_err(|_| Failure::Io)?.mode() & 0o111 == 0 {
                return Err(Failure::Input);
            }
            file.seal(owner, end)?;
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = file.finish();
            let _ = parent.finish();
            return Err(error);
        }
        Ok(Self {
            alias,
            path,
            parent,
            file,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn check(&self) -> Result<()> {
        self.alias.check()?;
        self.file.check(false)
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let file = self.file.finish();
        let directory = self.parent.finish();
        checked?;
        file?;
        directory
    }
}

pub(super) fn output_path(value: &Path, root: &Path, environment: &Environment) -> Result<PathBuf> {
    let value = if value == Path::new("~") || value.starts_with("~/") {
        let base = environment.get(OsStr::new("HOME")).ok_or(Failure::Input)?;
        PathBuf::from(base).join(value.strip_prefix("~").map_err(|_| Failure::Input)?)
    } else if value.is_absolute() {
        value.into()
    } else {
        root.join(value)
    };
    if value
        .as_os_str()
        .as_bytes()
        .iter()
        .any(|byte| *byte < 32 || *byte == b',')
        || value.symlink_metadata().is_ok()
    {
        return Err(Failure::Input);
    }
    let parent = value.parent().ok_or(Failure::Input)?;
    if parent.canonicalize().map_err(|_| Failure::Input)? != parent {
        return Err(Failure::Input);
    }
    if value.file_name().is_none() {
        return Err(Failure::Input);
    }
    Ok(value)
}

pub(super) fn temporary(owner: &mut Owner, end: Instant) -> Result<Output> {
    owner.before(end)?;
    let parent_path = std::env::temp_dir()
        .canonicalize()
        .map_err(|_| Failure::Io)?;
    let parent = Dir::open(&parent_path)?;
    let mut entropy = [0; 16];
    let mut random = match File::open("/dev/urandom") {
        Ok(file) => file,
        Err(_) => {
            let _ = parent.finish();
            return Err(Failure::Io);
        }
    };
    let read = random.read_exact(&mut entropy).map_err(|_| Failure::Io);
    let closed = files::close(random);
    if let Err(error) = read.and(closed).and_then(|()| owner.before(end)) {
        let _ = parent.finish();
        return Err(error);
    }
    let suffix: String = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
    Output::create(
        parent,
        OsStr::new(&format!("simplestchat-release-{suffix}")),
    )
}

struct Entry {
    relative: PathBuf,
    stamp: Metadata,
    digest: Option<String>,
    pending: Option<(Held, Dir)>,
}
pub(super) struct Tree {
    stamp: Metadata,
    directory: Dir,
    entries: Vec<Entry>,
}
impl Tree {
    pub fn extract(
        archive: &mut Held,
        parent: &Path,
        revision: &str,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<Self> {
        if !hex(revision, 40) {
            return Err(Failure::Input);
        }
        let parent_dir = Dir::open(parent)?;
        let directory = match Dir::create_directory(&parent_dir, OsStr::new("context"), 0o755) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = parent_dir.finish();
                return Err(error);
            }
        };
        let mut tree = Self {
            stamp: directory.original_metadata(),
            directory,
            entries: Vec::new(),
        };
        if let Err(error) = parent_dir.finish() {
            let _ = tree.remove(owner, owner.end());
            return Err(error);
        }
        let mut input = match archive.file().try_clone() {
            Ok(file) => file,
            Err(_) => {
                let _ = tree.remove(owner, owner.end());
                return Err(Failure::Io);
            }
        };
        use std::io::{Seek, SeekFrom};
        if input.seek(SeekFrom::Start(0)).is_err() {
            let _ = files::close(input);
            let _ = tree.remove(owner, owner.end());
            return Err(Failure::Io);
        }
        let extracted = (|| {
            let mut contents = tar::Archive::new(&mut input);
            let mut names = BTreeSet::new();
            let mut budget = SourceBudget::default();
            let mut global_comment = false;
            for member in contents.entries().map_err(|_| Failure::Archive)? {
                owner.before(end)?;
                archive.check(false)?;
                let mut member = member.map_err(|_| Failure::Archive)?;
                if member.header().entry_type().is_pax_global_extensions() {
                    // Git emits one commit comment before all filesystem members.
                    // Admit that bounded metadata only; it cannot alter paths,
                    // sizes, modes or the exact creation/membership journal.
                    if global_comment
                        || member.raw_header_position() != 0
                        || member.path_bytes().as_ref() != b"pax_global_header"
                        || member.header().size().map_err(|_| Failure::Archive)? != 52
                        || member.size() != 52
                    {
                        return Err(Failure::Archive);
                    }
                    let mut bytes = [0_u8; 52];
                    member
                        .read_exact(&mut bytes)
                        .map_err(|_| Failure::Archive)?;
                    owner.before(end)?;
                    archive.check(false)?;
                    let mut fields = tar::PaxExtensions::new(&bytes);
                    let field = fields
                        .next()
                        .ok_or(Failure::Archive)?
                        .map_err(|_| Failure::Archive)?;
                    if field.key_bytes() != b"comment"
                        || field.value_bytes() != revision.as_bytes()
                        || fields.next().is_some()
                        || bytes.last() != Some(&b'\n')
                    {
                        return Err(Failure::Archive);
                    }
                    global_comment = true;
                    continue;
                }
                let raw = member.path().map_err(|_| Failure::Archive)?.into_owned();
                if raw
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
                {
                    return Err(Failure::Archive);
                }
                let path: PathBuf = raw
                    .components()
                    .filter(|part| !matches!(part, Component::CurDir))
                    .collect();
                if path.as_os_str().is_empty() || !names.insert(path.clone()) {
                    return Err(Failure::Archive);
                }
                budget.admit(&path)?;
                let absolute = tree.directory.path().join(&path);
                let parent = Dir::open(absolute.parent().ok_or(Failure::Archive)?)?;
                let name = absolute.file_name().ok_or(Failure::Archive)?;
                if member.header().entry_type().is_dir() {
                    let created = match Dir::create_directory(&parent, name, 0o755) {
                        Ok(created) => created,
                        Err(error) => {
                            let _ = parent.finish();
                            return Err(error);
                        }
                    };
                    tree.entries.push(Entry {
                        relative: path,
                        stamp: created.original_metadata(),
                        digest: None,
                        pending: None,
                    });
                    #[cfg(test)]
                    let observed = fixture_created(FixtureCreated::Directory);
                    #[cfg(test)]
                    if let Err(error) = observed.as_ref() {
                        owner.record(*error);
                    }
                    let closed = created.finish();
                    let parent_closed = parent.finish();
                    #[cfg(test)]
                    observed?;
                    closed?;
                    parent_closed?;
                } else if member.header().entry_type().is_file() {
                    let mode = match member.header().mode() {
                        Ok(mode) => {
                            if mode & 0o111 != 0 {
                                0o755
                            } else {
                                0o644
                            }
                        }
                        Err(_) => {
                            let _ = parent.finish();
                            return Err(Failure::Archive);
                        }
                    };
                    let output = match parent.create(name, mode) {
                        Ok(output) => output,
                        Err(error) => {
                            let _ = parent.finish();
                            return Err(error);
                        }
                    };
                    let stamp = output.original_metadata();
                    // The created writer and its original parent are journaled before
                    // any fallible copy/readback. Error cleanup never adopts a leaf.
                    tree.entries.push(Entry {
                        relative: path,
                        stamp,
                        digest: None,
                        pending: Some((output, parent)),
                    });
                    #[cfg(test)]
                    if let Err(error) = fixture_created(FixtureCreated::File) {
                        owner.record(error);
                        return Err(error);
                    }
                    let entry = tree.entries.last_mut().unwrap();
                    let output = &mut entry.pending.as_mut().unwrap().0;
                    let mut buffer = [0; 65536];
                    let mut written = Sha256::new();
                    loop {
                        owner.before(end)?;
                        archive.check(false)?;
                        let count = member.read(&mut buffer).map_err(|_| Failure::Archive)?;
                        if count == 0 {
                            break;
                        }
                        written.update(&buffer[..count]);
                        output
                            .file_mut()
                            .write_all(&buffer[..count])
                            .map_err(|_| Failure::Io)?;
                    }
                    #[cfg(test)]
                    files::fixture_after_write(output.file())?;
                    let digest = output.seal(owner, end)?;
                    if digest != files::hex(&written.finalize()) {
                        return Err(Failure::Changed);
                    }
                    entry.stamp = output.file().metadata().map_err(|_| Failure::Io)?;
                    entry.digest = Some(digest);
                    let (output, parent) = entry.pending.take().unwrap();
                    let closed = output.finish();
                    let parent_closed = parent.finish();
                    closed?;
                    parent_closed?;
                } else {
                    let _ = parent.finish();
                    return Err(Failure::Archive);
                }
            }
            Ok(())
        })();
        let closed = files::close(input);
        if let Err(error) = extracted.and(closed) {
            let _ = tree.remove(owner, owner.end());
            return Err(error);
        }
        let checked = (|| {
            archive.check(false)?;
            #[cfg(test)]
            if EXTRA_BEFORE_SEAL.with(|flag| flag.replace(false)) {
                let mut file = tree
                    .directory
                    .create(OsStr::new("unrecorded-before-seal.txt"), 0o600)?;
                let written = file.write(b"unrecorded fixture", owner, end);
                let closed = file.finish();
                written.and(closed)?;
            }
            tree.seal_directories(owner, end)?;
            tree.check(owner, end)
        })();
        if let Err(error) = checked {
            owner.record(error);
            let _ = tree.remove(owner, owner.end());
            return Err(error);
        }
        Ok(tree)
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    #[cfg(test)]
    pub fn fixture_descriptors(&self) -> Vec<files::OriginalDescriptor> {
        self.directory.fixture_descriptors()
    }
    #[cfg(test)]
    pub fn fixture_refresh_directory_metadata(
        &mut self,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<()> {
        self.seal_directories(owner, end)
    }
    fn directory_identity(before: &Metadata, after: &Metadata) -> bool {
        before.is_dir()
            && after.is_dir()
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mode() == after.mode()
            && before.uid() == after.uid()
            && before.gid() == after.gid()
    }
    fn directory_seal(before: &Metadata, after: &Metadata) -> bool {
        Self::directory_identity(before, after)
            && before.len() == after.len()
            && before.nlink() == after.nlink()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }
    /// Only our journaled extraction creates may refresh directory growth fields.
    /// Identity, owner and mode never change. Later namespace changes are negative;
    /// these observations do not lock the filesystem against its owner.
    fn seal_directories(&mut self, owner: &mut Owner, end: Instant) -> Result<()> {
        self.directory.check()?;
        for entry in self.entries.iter_mut().filter(|entry| entry.stamp.is_dir()) {
            owner.before(end)?;
            let directory = Dir::open(&self.directory.path().join(&entry.relative))?;
            let observed = directory.descriptor().metadata().map_err(|_| Failure::Io);
            let valid = observed
                .as_ref()
                .is_ok_and(|stamp| Self::directory_identity(&entry.stamp, stamp));
            if let Err(error) = observed.as_ref() {
                owner.record(*error);
            } else if !valid {
                owner.record(Failure::Changed);
            }
            let closed = directory.finish();
            let observed = observed?;
            closed?;
            if !valid {
                return Err(Failure::Changed);
            }
            entry.stamp = observed;
        }
        owner.before(end)?;
        let observed = self
            .directory
            .descriptor()
            .metadata()
            .map_err(|_| Failure::Io)?;
        if !Self::directory_identity(&self.stamp, &observed) {
            return Err(Failure::Changed);
        }
        self.stamp = observed;
        self.directory.check()
    }
    pub fn check(&self, owner: &mut Owner, end: Instant) -> Result<()> {
        self.directory.check()?;
        if !Self::directory_seal(
            &self.stamp,
            &self
                .directory
                .descriptor()
                .metadata()
                .map_err(|_| Failure::Io)?,
        ) {
            return Err(Failure::Changed);
        }
        let mut expected = BTreeMap::<PathBuf, BTreeSet<OsString>>::new();
        expected.insert(PathBuf::new(), BTreeSet::new());
        for entry in &self.entries {
            owner.before(end)?;
            expected
                .entry(entry.relative.parent().ok_or(Failure::Changed)?.to_owned())
                .or_default()
                .insert(
                    entry
                        .relative
                        .file_name()
                        .ok_or(Failure::Changed)?
                        .to_owned(),
                );
            if entry.stamp.is_dir() {
                expected.entry(entry.relative.clone()).or_default();
            }
        }
        for (relative, names) in expected {
            owner.before(end)?;
            if relative.as_os_str().is_empty() {
                self.directory.members(&names, owner, end)?;
            } else {
                let directory = Dir::open(&self.directory.path().join(relative))?;
                let checked = directory.members(&names, owner, end);
                if let Err(error) = checked.as_ref() {
                    owner.record(*error);
                }
                let closed = directory.finish();
                checked?;
                closed?;
            }
        }
        for entry in &self.entries {
            owner.before(end)?;
            let path = self.directory.path().join(&entry.relative);
            if entry.stamp.is_dir() {
                let directory = Dir::open(&path)?;
                let observed = directory.descriptor().metadata().map_err(|_| Failure::Io);
                let matches = observed
                    .as_ref()
                    .is_ok_and(|observed| Self::directory_seal(&entry.stamp, observed));
                if let Err(error) = observed.as_ref() {
                    owner.record(*error);
                } else if !matches {
                    owner.record(Failure::Changed);
                }
                let closed = directory.finish();
                observed?;
                closed?;
                if !matches {
                    return Err(Failure::Changed);
                }
            } else {
                let parent = Dir::open(path.parent().unwrap())?;
                let mut file = match parent.read(path.file_name().unwrap()) {
                    Ok(file) => file,
                    Err(error) => {
                        let _ = parent.finish();
                        return Err(error);
                    }
                };
                let observed = file.file().metadata().map_err(|_| Failure::Io);
                let matches = observed
                    .as_ref()
                    .is_ok_and(|stamp| same(&entry.stamp, stamp));
                if !matches {
                    let failure = observed.as_ref().err().copied().unwrap_or(Failure::Changed);
                    owner.record(failure);
                    let _ = file.finish();
                    let _ = parent.finish();
                    return Err(failure);
                }
                let digest = file.seal(owner, end);
                if let Err(error) = digest.as_ref() {
                    owner.record(*error);
                }
                let closed = file.finish();
                let parent_closed = parent.finish();
                if digest? != *entry.digest.as_ref().unwrap() {
                    owner.record(Failure::Changed);
                    return Err(Failure::Changed);
                }
                closed?;
                parent_closed?;
            }
        }
        Ok(())
    }
    pub fn metadata(
        &self,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
        let mut inputs = BTreeMap::new();
        for name in [
            "Dockerfile",
            ".dockerignore",
            "Cargo.lock",
            "web/package-lock.json",
            "build/pip-constraints.txt",
        ] {
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.relative == Path::new(name))
                .and_then(|entry| entry.digest.as_ref())
                .ok_or(Failure::Source)?;
            inputs.insert(name.into(), entry.clone());
        }
        let mut migrations = BTreeMap::new();
        let mut files: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.relative.parent() == Some(Path::new("migrations"))
                    && entry.relative.extension() == Some(OsStr::new("sql"))
            })
            .collect();
        files.sort_by(|a, b| a.relative.cmp(&b.relative));
        for entry in files {
            owner.before(end)?;
            let name = entry
                .relative
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or(Failure::Source)?;
            let (version, suffix) = name
                .strip_suffix(".sql")
                .and_then(|name| name.split_once('_'))
                .ok_or(Failure::Source)?;
            if version.is_empty()
                || !version.bytes().all(|byte| byte.is_ascii_digit())
                || suffix.is_empty()
                || !suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(Failure::Source);
            }
            let version: i64 = version.parse().map_err(|_| Failure::Source)?;
            if version <= 0
                || migrations.contains_key(&version.to_string())
                || entry.digest.is_none()
            {
                return Err(Failure::Source);
            }
            let parent = Dir::open(&self.directory.path().join("migrations"))?;
            let mut file = match parent.read(OsStr::new(name)) {
                Ok(file) => file,
                Err(error) => {
                    let _ = parent.finish();
                    return Err(error);
                }
            };
            let collected = (|| {
                let stamp = file.file().metadata().map_err(|_| Failure::Io)?;
                if !same(&entry.stamp, &stamp) {
                    return Err(Failure::Changed);
                }
                #[cfg(test)]
                if EXPIRE_MIGRATION.with(|flag| flag.replace(false)) {
                    owner.expire();
                }
                if file.seal(owner, end)? != *entry.digest.as_ref().unwrap() {
                    return Err(Failure::Changed);
                }
                file.sha384(owner, end)
            })();
            if let Err(error) = collected.as_ref() {
                owner.record(*error);
            }
            let closed = file.finish();
            if let Err(error) = closed.as_ref() {
                owner.record(*error);
            }
            let parent_closed = parent.finish();
            if let Err(error) = parent_closed.as_ref() {
                owner.record(*error);
            }
            let digest = collected?;
            closed?;
            parent_closed?;
            migrations.insert(version.to_string(), digest);
        }
        if migrations.is_empty() || migrations.len() > 1000 {
            return Err(Failure::Source);
        }
        Ok((inputs, migrations))
    }
    pub fn remove(self, owner: &Owner, end: Instant) -> Result<()> {
        let mut failure = None;
        for mut entry in self.entries.into_iter().rev() {
            if let Some((file, parent)) = entry.pending.take() {
                let result = if owner.cleanup_before(end).is_ok() {
                    file.remove()
                } else {
                    file.finish()
                };
                let closed = parent.finish();
                if let Err(error) = result.and(closed) {
                    failure.get_or_insert(error);
                }
                if let Err(error) = owner.cleanup_before(end) {
                    failure.get_or_insert(error);
                }
                continue;
            }
            if let Err(error) = owner.cleanup_before(end) {
                failure.get_or_insert(error);
                continue;
            }
            let path = self.directory.path().join(entry.relative);
            let result = (|| {
                self.directory.check()?;
                let parent = Dir::open(path.parent().ok_or(Failure::Changed)?)?;
                let removed = if entry.stamp.is_dir() {
                    let directory = match Dir::open(&path) {
                        Ok(directory) => directory,
                        Err(error) => {
                            let _ = parent.finish();
                            return Err(error);
                        }
                    };
                    let observed = directory.descriptor().metadata().map_err(|_| Failure::Io);
                    let matches = observed.as_ref().is_ok_and(|stamp| {
                        stamp.dev() == entry.stamp.dev()
                            && stamp.ino() == entry.stamp.ino()
                            && stamp.mode() == entry.stamp.mode()
                            && stamp.uid() == entry.stamp.uid()
                            && stamp.gid() == entry.stamp.gid()
                    });
                    if !matches {
                        let _ = directory.finish();
                        Err(observed.err().unwrap_or(Failure::Changed))
                    } else {
                        directory.remove_directory(&parent, path.file_name().unwrap())
                    }
                } else {
                    let file = match parent.read(path.file_name().unwrap()) {
                        Ok(file) => file,
                        Err(error) => {
                            let _ = parent.finish();
                            return Err(error);
                        }
                    };
                    let observed = file.file().metadata().map_err(|_| Failure::Io);
                    let matches = observed
                        .as_ref()
                        .is_ok_and(|stamp| same(&entry.stamp, stamp));
                    if !matches {
                        let _ = file.finish();
                        Err(observed.err().unwrap_or(Failure::Changed))
                    } else {
                        file.remove()
                    }
                };
                let closed = parent.finish();
                removed.and(closed)
            })();
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        let removed = if owner.cleanup_before(end).is_ok() {
            let parent = Dir::open(self.directory.path().parent().ok_or(Failure::Changed)?);
            match parent {
                Ok(parent) => {
                    let name = self.directory.path().file_name().unwrap().to_owned();
                    let removed = self.directory.remove_directory(&parent, &name);
                    removed.and(parent.finish())
                }
                Err(error) => {
                    let _ = self.directory.finish();
                    Err(error)
                }
            }
        } else {
            let closed = self.directory.finish();
            closed.and(Err(Failure::Deadline))
        };
        if let Err(error) = removed {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }
}

pub(super) fn revision(bytes: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(bytes).map_err(|_| Failure::Utf8)?;
    if !hex(value, 40) {
        return Err(Failure::Source);
    }
    Ok(value.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_ledger_count_and_name_memory_are_bounded_before_file_creation() {
        let mut count = SourceBudget::default();
        for _ in 0..SOURCE_ENTRIES {
            count.admit(Path::new("x")).unwrap();
        }
        assert_eq!(count.admit(Path::new("x")), Err(Failure::Bounds));
        assert_eq!(count.entries, SOURCE_ENTRIES);
        let name = "x".repeat(200);
        let mut names = SourceBudget::default();
        for _ in 0..SOURCE_NAMES / name.len() {
            names.admit(Path::new(&name)).unwrap();
        }
        let before = names.names;
        assert_eq!(names.admit(Path::new(&name)), Err(Failure::Bounds));
        assert_eq!(names.names, before);
        assert!(names.entries < SOURCE_ENTRIES);
    }
}
