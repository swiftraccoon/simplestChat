//! Retained nofollow namespaces and atomically published private bounded records.

use super::{
    plan::{self, Context},
    protocol::{Binding, Intent},
};
use crate::tooling::Result;
use rustix::fs::{Mode, OFlags};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const RECORD: u64 = 16 * 1024;
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| plan::failure())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
pub(super) fn identity(file: &File) -> Result<Binding> {
    let value = file.metadata().map_err(|_| plan::failure())?;
    Ok(Binding {
        device: value.dev(),
        inode: value.ino(),
    })
}
pub(in crate::ci) struct Directory {
    owners: Vec<File>,
    before: Vec<Metadata>,
    names: Vec<OsString>,
}
/// Opaque identity witness; reopening grants access only after every original
/// ancestor name/device/inode/mode/owner binding has been re-admitted.
pub(in crate::ci) struct NamespaceBinding {
    before: Vec<Metadata>,
    names: Vec<OsString>,
}
impl NamespaceBinding {
    pub fn reopen(&self, path: &Path) -> Result<Directory> {
        let directory = Directory::open(path)?;
        if directory.names != self.names
            || directory.before.len() != self.before.len()
            || directory
                .before
                .iter()
                .zip(&self.before)
                .any(|(a, b)| !binding(a, b))
        {
            let _ = directory.finish();
            return Err(plan::failure());
        }
        Ok(directory)
    }
}
impl Directory {
    pub fn binding(&self) -> NamespaceBinding {
        NamespaceBinding {
            before: self.before.clone(),
            names: self.names.clone(),
        }
    }
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(plan::failure());
        }
        let first = rustix::fs::open("/", FLAGS | OFlags::DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| plan::failure())?;
        let before = match first.metadata() {
            Ok(before) => before,
            Err(_) => {
                close(first)?;
                return Err(plan::failure());
            }
        };
        let mut value = Self {
            owners: vec![first],
            before: vec![before],
            names: Vec::new(),
        };
        let result = (|| {
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) if value.names.len() < 24 => {
                        value.child(name.to_owned())?;
                    }
                    _ => return Err(plan::failure()),
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    fn child(&mut self, name: OsString) -> Result<()> {
        if self.names.len() >= 24 {
            return Err(plan::failure());
        }
        let file = rustix::fs::openat(self.file(), &name, FLAGS | OFlags::DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| plan::failure())?;
        let before = match file.metadata() {
            Ok(before) => before,
            Err(_) => {
                close(file)?;
                return Err(plan::failure());
            }
        };
        self.owners.push(file);
        self.before.push(before);
        self.names.push(name);
        Ok(())
    }
    pub fn file(&self) -> &File {
        self.owners.last().unwrap()
    }
    pub fn descend(mut self, name: &str) -> Result<Self> {
        if let Err(error) = self.child(name.into()) {
            let _ = self.finish();
            return Err(error);
        }
        Ok(self)
    }
    pub fn verify(&self) -> Result<()> {
        for (index, file) in self.owners.iter().enumerate() {
            if !binding(
                &self.before[index],
                &file.metadata().map_err(|_| plan::failure())?,
            ) {
                return Err(plan::failure());
            }
        }
        for (index, name) in self.names.iter().enumerate() {
            let visible = rustix::fs::openat(
                &self.owners[index],
                name,
                FLAGS | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| plan::failure())?;
            let observed = visible.metadata().map_err(|_| plan::failure());
            let closed = close(visible);
            if !binding(&observed?, &self.before[index + 1]) {
                return Err(plan::failure());
            }
            closed?;
        }
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        let verified = self.verify();
        let mut failed = false;
        for file in self.owners.into_iter().rev() {
            failed |= close(file).is_err();
        }
        verified?;
        if failed { Err(plan::failure()) } else { Ok(()) }
    }
    pub fn read(&self, name: &str, maximum: u64) -> Result<Option<Vec<u8>>> {
        self.read_regular(name, maximum, true)
    }
    fn read_regular(&self, name: &str, maximum: u64, private: bool) -> Result<Option<Vec<u8>>> {
        self.verify()?;
        let mut file = match rustix::fs::openat(self.file(), name, FLAGS, Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(plan::failure()),
        };
        let result = (|| {
            let before = file.metadata().map_err(|_| plan::failure())?;
            if !before.is_file()
                || before.len() > maximum
                || (private
                    && (before.mode() & 0o777 != 0o600
                        || before.uid() != rustix::process::getuid().as_raw()))
            {
                return Err(plan::failure());
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(maximum + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| plan::failure())?;
            if bytes.len() as u64 != before.len() {
                return Err(plan::failure());
            }
            self.check(name, &before)?;
            if !same(&before, &file.metadata().map_err(|_| plan::failure())?) {
                return Err(plan::failure());
            }
            Ok(bytes)
        })();
        let closed = close(file);
        let bytes = result?;
        closed?;
        Ok(Some(bytes))
    }
    pub fn json<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        self.json_limit(name, RECORD)
    }
    pub fn json_limit<T: DeserializeOwned>(&self, name: &str, maximum: u64) -> Result<Option<T>> {
        self.read(name, maximum)?
            .map(|v| serde_json::from_slice(&v).map_err(|_| plan::failure()))
            .transpose()
    }
    pub fn check(&self, name: &str, before: &Metadata) -> Result<()> {
        self.verify()?;
        let file = rustix::fs::openat(self.file(), name, FLAGS, Mode::empty())
            .map(File::from)
            .map_err(|_| plan::failure())?;
        let observed = file.metadata().map_err(|_| plan::failure());
        let closed = close(file);
        if !same(before, &observed?) {
            return Err(plan::failure());
        }
        closed
    }
    pub fn create(&self, name: &str) -> Result<File> {
        self.verify()?;
        rustix::fs::openat(
            self.file(),
            name,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map(File::from)
        .map_err(|_| plan::failure())
    }
    pub fn atomic(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.atomic_after(name, bytes, || {})
    }
    pub(super) fn atomic_after(
        &self,
        name: &str,
        bytes: &[u8],
        after: impl FnOnce(),
    ) -> Result<()> {
        if bytes.len() as u64 > RECORD {
            return Err(plan::failure());
        }
        let temporary = format!("{name}.part");
        let mut file = self.create(&temporary)?;
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| plan::failure());
        let metadata = file.metadata().map_err(|_| plan::failure());
        let closed = close(file);
        written?;
        closed?;
        self.check(&temporary, &metadata?)?;
        // No inode mutation follows visibility: link+unlink would change ctime
        // after a concurrent stable reader's initial snapshot.
        rustix::fs::renameat_with(
            self.file(),
            temporary.as_str(),
            self.file(),
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| plan::failure())?;
        after();
        self.file().sync_all().map_err(|_| plan::failure())?;
        self.verify()
    }
    pub fn publish<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        self.atomic(
            name,
            &serde_json::to_vec(value).map_err(|_| plan::failure())?,
        )
    }
}
pub(super) struct Owned {
    pub parent: Directory,
    pub spool: Directory,
}
impl Owned {
    pub fn open(context: &Context) -> Result<Self> {
        let parent = Directory::open(&context.temporary)?;
        let spool = match Directory::open(&context.temporary)
            .and_then(|directory| directory.descend(plan::SPOOL))
        {
            Ok(spool) => spool,
            Err(error) => {
                let _ = parent.finish();
                return Err(error);
            }
        };
        let metadata = spool.file().metadata().map_err(|_| plan::failure());
        let owned = Self { parent, spool };
        if !metadata.is_ok_and(|value| {
            value.mode() & 0o777 == 0o700 && value.uid() == rustix::process::getuid().as_raw()
        }) {
            let _ = owned.finish();
            return Err(plan::failure());
        }
        Ok(owned)
    }
    pub fn create(context: &Context) -> Result<(Self, Intent, String)> {
        let parent = Directory::open(&context.temporary)?;
        let created =
            rustix::fs::mkdirat(parent.file(), plan::SPOOL, Mode::from_bits_truncate(0o700))
                .map_err(|_| plan::failure());
        let closed = parent.finish();
        created?;
        closed?;
        let owned = Self::open(context)?;
        let result = (|| {
            let log = owned.parent.create(plan::LOG)?;
            let log_identity = identity(&log);
            let closed = close(log);
            let log = log_identity?;
            closed?;
            rustix::fs::mkdirat(
                owned.parent.file(),
                plan::PREFIX,
                Mode::from_bits_truncate(0o700),
            )
            .map_err(|_| plan::failure())?;
            let prefix = Directory::open(&context.temporary)?.descend(plan::PREFIX)?;
            let prefix_identity = identity(prefix.file());
            let closed = prefix.finish();
            let prefix = prefix_identity?;
            closed?;
            let mut random = File::open("/dev/urandom").map_err(|_| plan::failure())?;
            let mut bytes = [0_u8; 16];
            let read = random.read_exact(&mut bytes).map_err(|_| plan::failure());
            let closed = close(random);
            read?;
            closed?;
            let capability = bytes.iter().map(|v| format!("{v:02x}")).collect();
            let intent = Intent {
                origin: context.origin.clone(),
                capability,
                log,
                prefix,
            };
            owned.spool.publish("intent.json", &intent)?;
            let digest = owned.intent(context)?.1;
            Ok((intent, digest))
        })();
        match result {
            Ok((intent, digest)) => Ok((owned, intent, digest)),
            Err(error) => {
                let _ = owned.finish();
                Err(error)
            }
        }
    }
    pub fn intent(&self, context: &Context) -> Result<(Intent, String)> {
        let bytes = self
            .spool
            .read("intent.json", RECORD)?
            .ok_or_else(plan::failure)?;
        let intent: Intent = serde_json::from_slice(&bytes).map_err(|_| plan::failure())?;
        if intent.origin != context.origin
            || intent.capability.len() != 32
            || !intent.capability.bytes().all(|v| v.is_ascii_hexdigit())
        {
            return Err(plan::failure());
        }
        Ok((intent, plan::digest(&bytes)))
    }
    pub fn finish(self) -> Result<()> {
        let spool = self.spool.finish();
        let parent = self.parent.finish();
        spool.and(parent)
    }
}
