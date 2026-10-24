//! Original directory/file custody for the release controller's private evidence.

use super::control::{Failure, Owner, Result};
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::{Mode, fchmod},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::Write,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::FileExt,
        fs::MetadataExt,
    },
    path::{Component, Path, PathBuf},
    rc::Rc,
    time::Instant,
};

const READ: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_CLOEXEC)
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK);

pub(super) fn close(file: File) -> Result<()> {
    #[cfg(test)]
    let observation = {
        use std::os::fd::AsRawFd;
        let metadata = file.metadata().ok();
        CloseObservation {
            descriptor: file.as_raw_fd(),
            identity: metadata.as_ref().map(|value| (value.dev(), value.ino())),
            bytes: metadata.as_ref().map(Metadata::len),
            regular: metadata.as_ref().is_some_and(Metadata::is_file),
            succeeded: false,
        }
    };
    let result = nix::unistd::close(file).map_err(|_| Failure::Close);
    #[cfg(test)]
    {
        CLOSE_PROBE.with(|probe| {
            let mut probe = probe.borrow_mut();
            if !probe.enabled {
                return result;
            }
            let descriptor = observation.descriptor;
            probe.observations.push(CloseObservation {
                succeeded: result.is_ok(),
                ..observation
            });
            if probe.fail == Some(descriptor) {
                probe.fail = None;
                result.and(Err(Failure::Close))
            } else {
                result
            }
        })
    }
    #[cfg(not(test))]
    result
}
#[cfg(test)]
pub(super) struct CloseObservation {
    pub descriptor: i32,
    pub identity: Option<(u64, u64)>,
    pub bytes: Option<u64>,
    pub regular: bool,
    pub succeeded: bool,
}
#[cfg(test)]
#[derive(Default)]
struct CloseProbe {
    enabled: bool,
    fail: Option<i32>,
    observations: Vec<CloseObservation>,
    flip: bool,
}
#[cfg(test)]
thread_local! {
    static CLOSE_PROBE:std::cell::RefCell<CloseProbe>=const {
        std::cell::RefCell::new(CloseProbe {
            enabled:false,fail:None,observations:Vec::new(),flip:false,
        })
    };
}
#[cfg(test)]
pub(super) fn fixture_observe_closes(fail: Option<i32>) {
    CLOSE_PROBE.with(|probe| {
        *probe.borrow_mut() = CloseProbe {
            enabled: true,
            fail,
            ..CloseProbe::default()
        };
    });
}
#[cfg(test)]
pub(super) fn fixture_take_closes() -> Vec<CloseObservation> {
    CLOSE_PROBE.with(|probe| {
        let mut probe = probe.borrow_mut();
        probe.enabled = false;
        probe.fail = None;
        std::mem::take(&mut probe.observations)
    })
}
#[cfg(test)]
pub(super) fn fixture_after_write_flip_once() {
    CLOSE_PROBE.with(|probe| probe.borrow_mut().flip = true);
}
#[cfg(test)]
pub(super) fn fixture_after_write(file: &File) -> Result<()> {
    if CLOSE_PROBE.with(|probe| std::mem::take(&mut probe.borrow_mut().flip)) {
        let mut byte = [0];
        if file.read_at(&mut byte, 0).map_err(|_| Failure::Io)? != 1 {
            return Err(Failure::Io);
        }
        byte[0] ^= 1;
        if file.write_at(&byte, 0).map_err(|_| Failure::Io)? != 1 {
            return Err(Failure::Io);
        }
    }
    Ok(())
}
fn mode_bits(value: u32) -> Result<Mode> {
    #[cfg(target_os = "macos")]
    let value = value.try_into().map_err(|_| Failure::Input)?;
    Ok(Mode::from_bits_truncate(value))
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn directory_same(a: &Metadata, b: &Metadata) -> bool {
    a.is_dir()
        && b.is_dir()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn inode_same(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    inode_same(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn leaf(name: &OsStr) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        Err(Failure::Input)
    } else {
        Ok(())
    }
}
struct Edge {
    file: File,
    stamp: Metadata,
    name: Option<OsString>,
}
struct Directory {
    path: PathBuf,
    edges: Vec<Edge>,
}
#[derive(Clone)]
pub(super) struct Dir(Rc<Directory>);
impl Dir {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Failure::Input);
        }
        let root = File::from(
            open(Path::new("/"), READ | OFlag::O_DIRECTORY, Mode::empty())
                .map_err(|_| Failure::Io)?,
        );
        let root_stamp = match root.metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                let _ = close(root);
                return Err(Failure::Io);
            }
        };
        let mut edges = vec![Edge {
            stamp: root_stamp,
            file: root,
            name: None,
        }];
        let admitted = (|| {
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        let file = File::from(
                            openat(
                                &edges.last().unwrap().file,
                                name,
                                READ | OFlag::O_DIRECTORY,
                                Mode::empty(),
                            )
                            .map_err(|_| Failure::Io)?,
                        );
                        let stamp = match file.metadata() {
                            Ok(stamp) => stamp,
                            Err(_) => {
                                let _ = close(file);
                                return Err(Failure::Io);
                            }
                        };
                        edges.push(Edge {
                            file,
                            stamp,
                            name: Some(name.to_owned()),
                        });
                    }
                    _ => return Err(Failure::Input),
                }
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            for edge in edges.into_iter().rev() {
                let _ = close(edge.file);
            }
            return Err(error);
        }
        Ok(Self(Rc::new(Directory {
            path: path.into(),
            edges,
        })))
    }
    pub fn create_directory(parent: &Dir, name: &OsStr, mode: u32) -> Result<Self> {
        leaf(name)?;
        parent.check()?;
        let bits = mode_bits(mode)?;
        nix::sys::stat::mkdirat(parent.descriptor(), name, bits).map_err(|_| Failure::Io)?;
        let child = File::from(
            openat(
                parent.descriptor(),
                name,
                READ | OFlag::O_DIRECTORY,
                Mode::empty(),
            )
            .map_err(|_| Failure::Io)?,
        );
        let configured = (|| {
            let original = child.metadata().map_err(|_| Failure::Io)?;
            fchmod(&child, bits).map_err(|_| Failure::Io)?;
            let observed = child.metadata().map_err(|_| Failure::Io)?;
            if original.dev() != observed.dev()
                || original.ino() != observed.ino()
                || original.uid() != observed.uid()
                || original.gid() != observed.gid()
                || observed.mode() & 0o7777 != mode
            {
                return Err(Failure::Changed);
            }
            Ok(observed)
        })();
        let stamp = match configured {
            Ok(stamp) => stamp,
            Err(error) => {
                let _ = close(child);
                return Err(error);
            }
        };
        let mut edges = Vec::new();
        for edge in &parent.0.edges {
            match edge.file.try_clone() {
                Ok(file) => edges.push(Edge {
                    file,
                    stamp: edge.stamp.clone(),
                    name: edge.name.clone(),
                }),
                Err(_) => {
                    for edge in edges.into_iter().rev() {
                        let _ = close(edge.file);
                    }
                    let _ = close(child);
                    return Err(Failure::Io);
                }
            }
        }
        edges.push(Edge {
            file: child,
            stamp,
            name: Some(name.to_owned()),
        });
        let directory = Self(Rc::new(Directory {
            path: parent.path().join(name),
            edges,
        }));
        if let Err(error) = directory.check() {
            let _ = directory.close();
            return Err(error);
        }
        Ok(directory)
    }
    pub fn path(&self) -> &Path {
        &self.0.path
    }
    pub fn descriptor(&self) -> &File {
        &self.0.edges.last().unwrap().file
    }
    pub fn original_metadata(&self) -> Metadata {
        self.0.edges.last().unwrap().stamp.clone()
    }
    /// Compare actual names with the extraction journal through this original
    /// directory. The Linux iterator borrows a separately admitted cursor File.
    /// On macOS rustix owns an extra opaque libc cursor: its RAII closedir status
    /// is not exposed. Checked-close claims cover our original File handles only.
    pub fn members(
        &self,
        expected: &BTreeSet<OsString>,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<()> {
        self.check()?;
        owner.before(end)?;
        let cursor = File::from(
            openat(
                self.descriptor(),
                OsStr::new("."),
                READ | OFlag::O_DIRECTORY,
                Mode::empty(),
            )
            .map_err(|_| Failure::Io)?,
        );
        let mut seen = BTreeSet::new();
        let mut visits = 0_usize;
        fn accept(
            name: &[u8],
            expected: &BTreeSet<OsString>,
            seen: &mut BTreeSet<OsString>,
            visits: &mut usize,
        ) -> Result<()> {
            *visits = visits.checked_add(1).ok_or(Failure::Bounds)?;
            if *visits > expected.len().saturating_add(2) {
                return Err(Failure::Changed);
            }
            if name == b"." || name == b".." {
                return Ok(());
            }
            let name = OsStr::from_bytes(name);
            if !expected.contains(name)
                || !seen.insert(OsString::from_vec(name.as_bytes().to_vec()))
            {
                return Err(Failure::Changed);
            }
            Ok(())
        }
        let listed = (|| {
            #[cfg(target_os = "linux")]
            {
                let mut buffer = vec![std::mem::MaybeUninit::<u8>::uninit(); 65536];
                let mut entries = rustix::fs::RawDir::new(&cursor, &mut buffer);
                loop {
                    owner.before(end)?;
                    let Some(entry) = entries.next() else {
                        break;
                    };
                    let entry = entry.map_err(|_| Failure::Io)?;
                    accept(
                        entry.file_name().to_bytes(),
                        expected,
                        &mut seen,
                        &mut visits,
                    )?;
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let mut entries = rustix::fs::Dir::read_from(&cursor).map_err(|_| Failure::Io)?;
                loop {
                    owner.before(end)?;
                    let Some(entry) = entries.next() else {
                        break;
                    };
                    let entry = entry.map_err(|_| Failure::Io)?;
                    accept(
                        entry.file_name().to_bytes(),
                        expected,
                        &mut seen,
                        &mut visits,
                    )?;
                }
            }
            if &seen != expected {
                return Err(Failure::Changed);
            }
            self.check()?;
            owner.before(end)
        })();
        if let Err(error) = listed.as_ref() {
            owner.record(*error);
        }
        let closed = close(cursor);
        if let Err(error) = closed.as_ref() {
            owner.record(*error);
        }
        listed.and(closed)
    }
    #[cfg(test)]
    pub fn fixture_descriptors(&self) -> Vec<(i32, u64, u64)> {
        use std::os::fd::AsRawFd;
        self.0
            .edges
            .iter()
            .map(|edge| {
                let stamp = edge.file.metadata().unwrap();
                (edge.file.as_raw_fd(), stamp.dev(), stamp.ino())
            })
            .collect()
    }
    pub fn check(&self) -> Result<()> {
        for (index, edge) in self.0.edges.iter().enumerate() {
            if !directory_same(&edge.stamp, &edge.file.metadata().map_err(|_| Failure::Io)?) {
                return Err(Failure::Changed);
            }
            if let Some(name) = &edge.name {
                let visible = File::from(
                    openat(
                        &self.0.edges[index - 1].file,
                        name.as_os_str(),
                        READ | OFlag::O_DIRECTORY,
                        Mode::empty(),
                    )
                    .map_err(|_| Failure::Changed)?,
                );
                let observed = visible.metadata().map_err(|_| Failure::Changed);
                let closed = close(visible);
                let unchanged = directory_same(&edge.stamp, &observed?);
                closed?;
                if !unchanged {
                    return Err(Failure::Changed);
                }
            }
        }
        Ok(())
    }
    pub fn create(&self, name: &OsStr, mode: u32) -> Result<Held> {
        leaf(name)?;
        self.check()?;
        let file = File::from(
            openat(
                self.descriptor(),
                name,
                OFlag::O_RDWR
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL,
                mode_bits(mode)?,
            )
            .map_err(|_| Failure::Io)?,
        );
        let admitted = (|| {
            fchmod(&file, mode_bits(mode)?).map_err(|_| Failure::Io)?;
            let stamp = file.metadata().map_err(|_| Failure::Io)?;
            if !stamp.is_file()
                || stamp.mode() & 0o7777 != mode
                || stamp.uid() != rustix::process::getuid().as_raw()
                || stamp.nlink() != 1
                || stamp.len() != 0
            {
                return Err(Failure::Changed);
            }
            Ok(stamp)
        })();
        match admitted {
            Ok(stamp) => Ok(Held {
                directory: self.clone(),
                name: name.to_owned(),
                aliases: Vec::new(),
                file: Some(file),
                stamp,
                digest: None,
            }),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    pub fn read(&self, name: &OsStr) -> Result<Held> {
        leaf(name)?;
        self.check()?;
        let file = File::from(
            openat(self.descriptor(), name, READ, Mode::empty()).map_err(|_| Failure::Io)?,
        );
        let stamp = match file.metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                let _ = close(file);
                return Err(Failure::Io);
            }
        };
        if !stamp.is_file() {
            let _ = close(file);
            return Err(Failure::Input);
        }
        Ok(Held {
            directory: self.clone(),
            name: name.to_owned(),
            aliases: Vec::new(),
            file: Some(file),
            stamp,
            digest: None,
        })
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let closed = self.close();
        checked?;
        closed
    }
    fn close(self) -> Result<()> {
        let directory = Rc::try_unwrap(self.0).map_err(|_| Failure::Close)?;
        let mut failed = false;
        for edge in directory.edges.into_iter().rev() {
            failed |= close(edge.file).is_err();
        }
        if failed { Err(Failure::Close) } else { Ok(()) }
    }
    pub fn remove_directory(self, parent: &Dir, name: &OsStr) -> Result<()> {
        let checked = self.check().and_then(|()| parent.check());
        let removed = checked.and_then(|()| {
            nix::unistd::unlinkat(
                parent.descriptor(),
                name,
                nix::unistd::UnlinkatFlags::RemoveDir,
            )
            .map_err(|_| Failure::Changed)
        });
        let closed = self.close();
        removed?;
        closed
    }
}

pub(super) struct Held {
    directory: Dir,
    name: OsString,
    aliases: Vec<OsString>,
    file: Option<File>,
    stamp: Metadata,
    digest: Option<String>,
}
impl Held {
    pub fn original_metadata(&self) -> Metadata {
        self.stamp.clone()
    }
    pub fn path(&self) -> PathBuf {
        self.directory.path().join(&self.name)
    }
    pub fn file(&self) -> &File {
        self.file
            .as_ref()
            .expect("retained file before explicit finish")
    }
    pub fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("retained file before explicit finish")
    }
    pub fn writer(&self) -> Result<File> {
        self.check(false)?;
        self.file().try_clone().map_err(|_| Failure::Io)
    }
    pub fn check(&self, growing: bool) -> Result<()> {
        self.directory.check()?;
        let original = self.file().metadata().map_err(|_| Failure::Io)?;
        let stamp_matches = if growing && self.digest.is_none() {
            inode_same(&self.stamp, &original)
        } else {
            same(&self.stamp, &original)
        };
        if !stamp_matches {
            return Err(Failure::Changed);
        }
        for name in std::iter::once(&self.name).chain(&self.aliases) {
            let visible = File::from(
                openat(
                    self.directory.descriptor(),
                    name.as_os_str(),
                    READ,
                    Mode::empty(),
                )
                .map_err(|_| Failure::Changed)?,
            );
            let observed = visible.metadata().map_err(|_| Failure::Changed);
            let closed = close(visible);
            let matches = same(&original, &observed?);
            closed?;
            if !matches {
                return Err(Failure::Changed);
            }
        }
        Ok(())
    }
    pub fn seal(&mut self, owner: &mut Owner, end: Instant) -> Result<String> {
        self.seal_with(|| owner.before(end))
    }
    pub fn seal_cleanup(&mut self, owner: &Owner, end: Instant) -> Result<String> {
        self.seal_with(|| owner.cleanup_before(end))
    }
    fn seal_with(&mut self, mut check: impl FnMut() -> Result<()>) -> Result<String> {
        self.check(true)?;
        let before = self.file().metadata().map_err(|_| Failure::Io)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        let mut offset = 0;
        loop {
            check()?;
            let count = self
                .file()
                .read_at(&mut buffer, offset)
                .map_err(|_| Failure::Io)?;
            if count == 0 {
                break;
            }
            offset += count as u64;
            if offset > before.len() {
                return Err(Failure::Changed);
            }
            hash.update(&buffer[..count]);
        }
        if offset != before.len()
            || !same(&before, &self.file().metadata().map_err(|_| Failure::Io)?)
        {
            return Err(Failure::Changed);
        }
        self.stamp = before;
        let digest = hex(&hash.finalize());
        if self
            .digest
            .as_ref()
            .is_some_and(|original| original != &digest)
        {
            return Err(Failure::Changed);
        }
        self.digest = Some(digest.clone());
        self.check(false)?;
        Ok(digest)
    }
    pub fn bytes(&self, maximum: usize, owner: &mut Owner, end: Instant) -> Result<Vec<u8>> {
        self.check(false)?;
        if self.stamp.len() > maximum as u64 {
            return Err(Failure::Bounds);
        }
        let mut bytes = vec![0; self.stamp.len() as usize];
        let mut offset = 0;
        while offset < bytes.len() {
            owner.before(end)?;
            let maximum = (bytes.len() - offset).min(65536);
            let count = self
                .file()
                .read_at(&mut bytes[offset..offset + maximum], offset as u64)
                .map_err(|_| Failure::Io)?;
            if count == 0 {
                return Err(Failure::Changed);
            }
            offset += count;
        }
        self.check(false)?;
        if self
            .digest
            .as_ref()
            .is_some_and(|expected| expected != &hex(&Sha256::digest(&bytes)))
        {
            return Err(Failure::Changed);
        }
        Ok(bytes)
    }
    pub fn write(&mut self, bytes: &[u8], owner: &mut Owner, end: Instant) -> Result<()> {
        self.check(false)?;
        if self.stamp.len() != 0 || self.digest.is_some() {
            return Err(Failure::Changed);
        }
        for chunk in bytes.chunks(65536) {
            owner.before(end)?;
            self.file_mut().write_all(chunk).map_err(|_| Failure::Io)?;
        }
        self.file().sync_all().map_err(|_| Failure::Io)?;
        #[cfg(test)]
        fixture_after_write(self.file())?;
        if self.seal(owner, end)? != hex(&Sha256::digest(bytes)) {
            return Err(Failure::Changed);
        }
        Ok(())
    }
    pub fn write_cleanup(&mut self, bytes: &[u8], owner: &Owner, end: Instant) -> Result<()> {
        self.check(false)?;
        if self.stamp.len() != 0 || self.digest.is_some() {
            return Err(Failure::Changed);
        }
        for chunk in bytes.chunks(65536) {
            owner.cleanup_before(end)?;
            self.file_mut().write_all(chunk).map_err(|_| Failure::Io)?;
        }
        self.file().sync_all().map_err(|_| Failure::Io)?;
        #[cfg(test)]
        fixture_after_write(self.file())?;
        if self.seal_cleanup(owner, end)? != hex(&Sha256::digest(bytes)) {
            return Err(Failure::Changed);
        }
        Ok(())
    }
    pub fn sha384(&self, owner: &mut Owner, end: Instant) -> Result<String> {
        use sha2::Sha384;
        self.check(false)?;
        let mut digest = Sha384::new();
        let mut offset = 0;
        let mut buffer = [0; 65536];
        loop {
            owner.before(end)?;
            let count = self
                .file()
                .read_at(&mut buffer, offset)
                .map_err(|_| Failure::Io)?;
            if count == 0 {
                break;
            }
            offset += count as u64;
            if offset > self.stamp.len() {
                return Err(Failure::Changed);
            }
            digest.update(&buffer[..count]);
        }
        self.check(false)?;
        if offset != self.stamp.len() {
            return Err(Failure::Changed);
        }
        Ok(hex(&digest.finalize()))
    }
    pub fn length(&self) -> u64 {
        self.stamp.len()
    }
    pub fn finish(mut self) -> Result<()> {
        let checked = self.check(self.digest.is_none());
        let closed = close(self.file.take().unwrap());
        checked?;
        closed
    }
    pub fn remove(mut self) -> Result<()> {
        let checked = self.check(self.digest.is_none()).and({
            if self.aliases.is_empty() {
                Ok(())
            } else {
                Err(Failure::Changed)
            }
        });
        let removed = checked.and_then(|()| {
            nix::unistd::unlinkat(
                self.directory.descriptor(),
                self.name.as_os_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            )
            .map_err(|_| Failure::Changed)
        });
        let closed = close(self.file.take().unwrap());
        removed?;
        closed
    }
    pub fn linked(&mut self, name: &OsStr, owner: &mut Owner, end: Instant) -> Result<()> {
        leaf(name)?;
        self.seal(owner, end)?;
        let before = self.stamp.clone();
        nix::unistd::linkat(
            self.directory.descriptor(),
            self.name.as_os_str(),
            self.directory.descriptor(),
            name,
            nix::fcntl::AtFlags::empty(),
        )
        .map_err(|_| Failure::Io)?;
        let after = self.file().metadata().map_err(|_| Failure::Io)?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.uid() != after.uid()
            || before.gid() != after.gid()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || after.nlink() != before.nlink() + 1
        {
            return Err(Failure::Changed);
        }
        self.stamp = after;
        self.aliases.push(name.to_owned());
        self.seal(owner, end)?;
        let visible = self.directory.read(name)?;
        let matches = same(&self.stamp, &visible.stamp);
        let closed = visible.finish();
        if !matches {
            return Err(Failure::Changed);
        }
        closed
    }
}

pub(super) struct Output {
    directory: Dir,
    records: BTreeMap<String, Held>,
}
impl Output {
    pub fn create(parent: Dir, name: &OsStr) -> Result<Self> {
        let created = Dir::create_directory(&parent, name, 0o700);
        let parent_closed = parent.finish();
        let directory = created?;
        if let Err(error) = parent_closed {
            let _ = directory.finish();
            return Err(error);
        }
        Ok(Self {
            directory,
            records: BTreeMap::new(),
        })
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    #[cfg(test)]
    pub fn fixture_descriptors(&self) -> Vec<(i32, u64, u64)> {
        use std::os::fd::AsRawFd;
        let mut descriptors = self.directory.fixture_descriptors();
        descriptors.extend(self.records.values().map(|file| {
            let stamp = file.file().metadata().unwrap();
            (file.file().as_raw_fd(), stamp.dev(), stamp.ino())
        }));
        descriptors
    }
    pub fn create_file(&mut self, name: &str) -> Result<()> {
        if self.records.contains_key(name) {
            return Err(Failure::Changed);
        }
        let file = self.directory.create(OsStr::new(name), 0o600)?;
        self.records.insert(name.into(), file);
        Ok(())
    }
    pub fn get(&self, name: &str) -> Result<&Held> {
        self.records.get(name).ok_or(Failure::Changed)
    }
    pub fn get_mut(&mut self, name: &str) -> Result<&mut Held> {
        self.records.get_mut(name).ok_or(Failure::Changed)
    }
    pub fn json<T: serde::Serialize>(
        &mut self,
        name: &str,
        value: &T,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<()> {
        owner.before(end)?;
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| Failure::Input)?;
        bytes.push(b'\n');
        self.create_file(name)?;
        self.get_mut(name)?.write(&bytes, owner, end)
    }
    pub fn discard(&mut self, name: &str) -> Result<()> {
        self.records.remove(name).ok_or(Failure::Changed)?.remove()
    }
    pub fn json_cleanup<T: serde::Serialize>(
        &mut self,
        name: &str,
        value: &T,
        owner: &Owner,
        end: Instant,
    ) -> Result<()> {
        owner.cleanup_before(end)?;
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| Failure::Input)?;
        bytes.push(b'\n');
        self.create_file(name)?;
        self.get_mut(name)?.write_cleanup(&bytes, owner, end)
    }
    pub fn check(&self) -> Result<()> {
        self.directory.check()?;
        for file in self.records.values() {
            file.check(file.digest.is_none())?;
        }
        Ok(())
    }
    pub fn remove(self, owner: &Owner, end: Instant) -> Result<()> {
        let checked = self.check();
        let mut failure = checked.err();
        for file in self.records.into_values() {
            let result = if owner.cleanup_before(end).is_ok() {
                file.remove()
            } else {
                file.finish()
            };
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
            if let Err(error) = owner.cleanup_before(end) {
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
            self.directory.finish().and(Err(Failure::Deadline))
        };
        if let Err(error) = removed {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let mut failed = false;
        for file in self.records.into_values() {
            failed |= file.finish().is_err();
        }
        let closed = self.directory.finish();
        checked?;
        closed?;
        if failed { Err(Failure::Close) } else { Ok(()) }
    }
}
