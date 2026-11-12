//! Same-process capability for the existing nonblocking workload flock and
//! unfinished-journal gates. No path or owner bypass is public, and the only
//! recovery is a backup, reboot or migration owner's named takeover of its
//! own unfinished record.
use super::{
    Result, fail,
    files::{self, Dir, Entry, Fs, ScopedFs},
    require,
    scope::Scope,
};
use crate::{
    host::boot_identity,
    json::{self, Json},
    release::workload::Lease,
};
use std::fs::File;

/// Unfinished ownership a caller may take over once it holds the lock, in
/// order to resolve it itself. The backup owners take over only the shared
/// runtime journal, the reboot owner only the release record; each leaves
/// the other journal held to a finalized state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Takeover<'a> {
    /// Nothing: an unfinished journal refuses.
    Nothing,
    /// The nightly backup's own record, once its bounded daemon-side work has
    /// aged out on this boot or the host has rebooted since. `boot` is the
    /// current boot identity and `monotonic` the kernel's monotonic seconds.
    ExpiredNightly { boot: &'a str, monotonic: f64 },
    /// The offsite backup's own record.
    Offsite,
    /// No unfinished release record, but the caller writes its own next: a
    /// reboot's preparation.
    RebootPrepare,
    /// A prepared reboot's release record, once the boot identity differs
    /// from the one it recorded. `boot` is the current boot identity.
    RebootResume { boot: &'a str },
    /// A prepared reboot's release record while the recorded boot is still
    /// the current one.
    RebootCancel { boot: &'a str },
    /// The release record of exactly this move between two machines, which
    /// each of the move's steps continues.
    Migration {
        operation: &'a str,
        source: &'a str,
        destination: &'a str,
        revision: &'a str,
    },
}

impl Takeover<'_> {
    /// The part of this takeover that concerns the shared runtime journal.
    fn runtime(self) -> Self {
        match self {
            Self::ExpiredNightly { .. } | Self::Offsite => self,
            _ => Self::Nothing,
        }
    }

    /// The part of this takeover that concerns the release record.
    fn release(self) -> Self {
        match self {
            Self::RebootPrepare
            | Self::RebootResume { .. }
            | Self::RebootCancel { .. }
            | Self::Migration { .. } => self,
            _ => Self::Nothing,
        }
    }

    /// Whether the unfinished `record` is this caller's to resolve.
    fn admits(self, record: &Json) -> bool {
        let operation = record.get("operation").and_then(Json::string);
        let prepared = || {
            let awaiting = record.get("phase").and_then(Json::string) == Some("await_reboot");
            let recorded = record.get("bootId").and_then(Json::string);
            recorded.filter(|recorded| awaiting && boot_identity(recorded))
        };
        match self {
            Self::Nothing | Self::RebootPrepare => false,
            Self::RebootResume { boot } => prepared().is_some_and(|recorded| recorded != boot),
            Self::RebootCancel { boot } => prepared().is_some_and(|recorded| recorded == boot),
            Self::Offsite => operation == Some("offsite_backup"),
            Self::Migration {
                operation: identity,
                source,
                destination,
                revision,
            } => {
                let request = record.get("request").and_then(Json::object);
                let named = |name: &str, expected: &str| {
                    let value = request.and_then(|request| request.get(name));
                    value.and_then(Json::string) == Some(expected)
                };
                operation == Some("server_migration")
                    && request.is_some_and(|request| request.len() == 4)
                    && named("operationId", identity)
                    && named("sourceOrigin", source)
                    && named("destinationOrigin", destination)
                    && named("targetRevision", revision)
            }
            Self::ExpiredNightly { boot, monotonic } => {
                let deadline = match record.get("untilMonotonic") {
                    Some(value @ (Json::Integer(_) | Json::Float(_))) => value.number(),
                    _ => None,
                };
                let recorded = record.get("bootId").and_then(Json::string);
                let recorded = recorded.filter(|recorded| boot_identity(recorded));
                operation == Some("nightly_backup")
                    && deadline.is_some_and(|deadline| deadline > 0.0)
                    && recorded.is_some_and(|recorded| {
                        recorded != boot || deadline.is_some_and(|deadline| monotonic >= deadline)
                    })
            }
        }
    }
}

/// Own the fixed root/config/work directories and one acquired workload lock.
/// Borrow it for Store operations, then call finish to observe closes. Dropping
/// it releases kernel descriptors but provides no successful-close receipt.
pub struct Workload {
    pub(super) fs: Fs,
    pub(super) root: Dir,
    pub(super) store_root: Option<Dir>,
    lock: Option<File>,
    lock_entry: Option<Entry>,
    held: bool,
    borrowed: Option<Lease>,
}
impl Workload {
    pub fn acquire() -> Result<Self> {
        Self::acquire_for(Takeover::Nothing)
    }
    /// Acquire as `acquire` does, admitting the one kind of unfinished
    /// runtime journal that `takeover` names. The caller then owns resolving
    /// it before finalizing the journal.
    pub fn acquire_for(takeover: Takeover<'_>) -> Result<Self> {
        require(
            rustix::process::geteuid().as_raw() == 0,
            "asset_command_owner",
        )?;
        require(
            rustix::system::uname().sysname() == c"Linux",
            "asset_operation_failed",
        )?;
        Self::from_fs(Fs::native()?, takeover)
    }
    fn from_fs(fs: Fs, takeover: Takeover<'_>) -> Result<Self> {
        let mut value = Self {
            fs,
            root: 0,
            store_root: None,
            lock: None,
            lock_entry: None,
            held: false,
            borrowed: None,
        };
        let result = value.setup(takeover);
        if let Err(error) = result {
            let finished = value.finish();
            // Busy is a successful CLI receipt only after owned descriptors
            // have closed; a later close failure is not lock contention.
            if error.busy() {
                finished?;
            }
            return Err(error);
        }
        Ok(value)
    }
    pub(super) fn borrow(file: File, lease: Lease, uid: u32, scope: &Scope<'_>) -> Result<Self> {
        #[cfg(not(test))]
        if uid != 0 {
            let result = Err(fail("asset_command_owner"));
            scope.observed(&result);
            let _ = scope.closed(files::Access::default().close(file));
            return result;
        }
        let access = files::Access {
            #[cfg(test)]
            owner: Some(uid),
            #[cfg(test)]
            hook: None,
        };
        let fs = Fs::open_scoped(file, access, scope)?;
        let value = Self {
            fs,
            root: 0,
            store_root: None,
            lock: None,
            lock_entry: None,
            held: false,
            borrowed: Some(lease),
        };
        let checked = value.check_scoped(scope);
        scope.observed(&checked);
        if let Err(first) = checked {
            let _ = value.finish_scoped(scope);
            return Err(first);
        }
        Ok(value)
    }
    pub(super) fn check_scoped(&self, scope: &Scope<'_>) -> Result<()> {
        scope.check()?;
        let fs = ScopedFs::new(&self.fs, scope);
        if let Some(lease) = &self.borrowed {
            scope.lease(lease.check())?;
            fs.protected_dir(self.root, Some(0o700))?;
            fs.verify()?;
            return scope.result(Ok(()));
        }
        require(self.held && self.lock.is_some(), "asset_operation_failed")?;
        fs.verify()?;
        fs.check_entry(
            self.lock_entry
                .ok_or_else(|| fail("asset_operation_failed"))?,
        )
    }
    pub(super) fn finish_scoped(mut self, scope: &Scope<'_>) -> Result<()> {
        let store = self
            .store_root
            .map(|root| {
                super::store::read_scoped(&mut ScopedFs::new(&mut self.fs, scope), root).map(|_| ())
            })
            .unwrap_or(Ok(()));
        scope.observed(&store);
        let proof = self.check_scoped(scope);
        scope.observed(&proof);
        let finished = self.fs.finish_scoped(scope);
        // The borrower never unlocks. Its retained filesystem owners close
        // before the actual Lease clone is dropped and the sole owner can unlock.
        drop(self.borrowed.take());
        scope.result(store.and(proof).and(finished))
    }
    fn setup(&mut self, takeover: Takeover<'_>) -> Result<()> {
        let srv = self.fs.directory(0, "srv", None)?;
        self.root = self.fs.directory(srv, "simplestchat-public", Some(0o700))?;
        let etc = self.fs.directory(0, "etc", None)?;
        let _ = self.fs.directory(etc, "simplestchat-public", Some(0o700))?;
        let run = self.fs.directory(0, "run", None)?;
        let work = if self.fs.absent(run, "simplestchat-bench")? {
            self.fs.mkdir(run, "simplestchat-bench", 0o700)?
        } else {
            self.fs.directory(run, "simplestchat-bench", Some(0o700))?
        };
        let (file, entry) = self.fs.lock_file(work)?;
        self.lock = Some(file);
        self.lock_entry = Some(entry);
        match rustix::fs::flock(
            self.lock.as_ref().unwrap(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        ) {
            Ok(()) => self.held = true,
            Err(rustix::io::Errno::WOULDBLOCK) => {
                self.fs.access.event("busy", "")?;
                return Err(fail("asset_workload_busy"));
            }
            Err(_) => return Err(fail("asset_operation_failed")),
        }
        self.fs.access.event("locked", "")?;
        self.check()?;
        self.journal(work, "current.json", takeover.runtime())?;
        self.journal(self.root, "release-state.json", takeover.release())?;
        self.check()
    }
    fn journal(&mut self, parent: Dir, name: &str, takeover: Takeover<'_>) -> Result<()> {
        if self.fs.absent(parent, name)? {
            return Ok(());
        }
        let entry = self.fs.metadata(parent, name)?;
        require(
            self.fs.mode(entry) == 0o600 && self.fs.stamp(entry).len() <= 16384,
            "asset_operation_failed",
        )?;
        let (bytes, _) = self.fs.read(parent, name, 16384)?;
        // workload_lock uses read_text() then JSON text parsing on the UTF-8
        // supported host. Do not let the byte decoder reinterpret UTF-16/32;
        // escaped NUL in an otherwise valid string remains supported.
        require(
            std::str::from_utf8(&bytes).is_ok()
                && !bytes.starts_with(b"\xef\xbb\xbf")
                && !bytes.contains(&0),
            "asset_operation_failed",
        )?;
        let value =
            json::decode_limited(&bytes, 16384).map_err(|_| fail("asset_operation_failed"))?;
        let finalized = value.get("finalized").and_then(Json::boolean) == Some(true);
        require(
            value.object().is_some()
                && matches!(value.get("schemaVersion"),Some(Json::Integer(v))if v=="1")
                && (finalized || takeover.admits(&value)),
            "asset_operation_failed",
        )?;
        if takeover == Takeover::Nothing {
            return Ok(());
        }
        // A caller that owns this journal writes its own record next, so
        // the journal is not held to the bytes just validated.
        self.fs.release(entry)
    }
    pub(super) fn check(&self) -> Result<()> {
        require(self.held && self.lock.is_some(), "asset_operation_failed")?;
        self.fs.verify()?;
        self.fs.check_entry(
            self.lock_entry
                .ok_or_else(|| fail("asset_operation_failed"))?,
        )
    }
    /// Revalidate this same-process lock without exposing its descriptor or an
    /// alternate managed root. Installation borrows this existing capability.
    pub(crate) fn validate(&self) -> Result<()> {
        self.check()
    }
    pub fn finish(mut self) -> Result<()> {
        let store = self
            .store_root
            .map(|root| super::store::read(&mut self.fs, root).map(|_| ()))
            .unwrap_or(Ok(()));
        let proof = self.fs.verify();
        let access = self.fs.access.clone();
        let finished = self.fs.finish();
        let unlock = if self.held {
            files::sys(rustix::fs::flock(
                self.lock.as_ref().unwrap(),
                rustix::fs::FlockOperation::Unlock,
            ))
        } else {
            Ok(())
        };
        self.held = false;
        let closed = self
            .lock
            .take()
            .map(|file| access.close(file))
            .unwrap_or(Ok(()));
        store.and(proof).and(finished).and(unlock).and(closed)
    }
    #[cfg(test)]
    pub(super) fn owners(&self) -> Vec<(i32, std::fs::Metadata)> {
        use std::os::fd::AsRawFd;
        let mut owners = self.fs.owners();
        if let Some(file) = &self.lock {
            owners.push((file.as_raw_fd(), file.metadata().unwrap()));
        }
        owners
    }
    #[cfg(test)]
    pub(crate) fn fixture_for(
        path: &std::path::Path,
        owner: u32,
        takeover: Takeover<'_>,
    ) -> Result<Self> {
        use rustix::fs::{Mode, OFlags};
        let file = File::from(files::sys(rustix::fs::open(
            path,
            files::FLAGS | OFlags::DIRECTORY,
            Mode::empty(),
        ))?);
        let access = files::Access {
            owner: Some(owner),
            hook: None,
        };
        Self::from_fs(Fs::open(file, access)?, takeover)
    }
    #[cfg(test)]
    pub(super) fn fixture(
        path: &std::path::Path,
        owner: u32,
        hook: Option<files::Hook>,
    ) -> Result<Self> {
        use rustix::fs::{Mode, OFlags};
        let file = File::from(files::sys(rustix::fs::open(
            path,
            files::FLAGS | OFlags::DIRECTORY,
            Mode::empty(),
        ))?);
        Self::from_fs(
            Fs::open(
                file,
                files::Access {
                    owner: Some(owner),
                    hook,
                },
            )?,
            Takeover::Nothing,
        )
    }
    #[cfg(test)]
    pub(crate) fn install_fixture(path: &std::path::Path, owner: u32) -> Result<Self> {
        Self::fixture(path, owner, None)
    }
}
