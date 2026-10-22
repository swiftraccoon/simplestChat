//! Same-process capability for the existing nonblocking workload flock and
//! unfinished-journal gates. No path, owner or recovery bypass is public.
use super::{
    Result, fail,
    files::{self, Dir, Entry, Fs},
    require,
};
use crate::json::{self, Json};
use std::fs::File;

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
}
impl Workload {
    pub fn acquire() -> Result<Self> {
        require(
            rustix::process::geteuid().as_raw() == 0,
            "asset_command_owner",
        )?;
        require(
            rustix::system::uname().sysname() == c"Linux",
            "asset_operation_failed",
        )?;
        Self::from_fs(Fs::native()?)
    }
    fn from_fs(fs: Fs) -> Result<Self> {
        let mut value = Self {
            fs,
            root: 0,
            store_root: None,
            lock: None,
            lock_entry: None,
            held: false,
        };
        let result = value.setup();
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
    fn setup(&mut self) -> Result<()> {
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
        self.journal(work, "current.json")?;
        self.journal(self.root, "release-state.json")?;
        self.check()
    }
    fn journal(&mut self, parent: Dir, name: &str) -> Result<()> {
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
        require(
            value.object().is_some()
                && matches!(value.get("schemaVersion"),Some(Json::Integer(v))if v=="1")
                && value.get("finalized").and_then(Json::boolean) == Some(true),
            "asset_operation_failed",
        )
    }
    pub(super) fn check(&self) -> Result<()> {
        require(self.held && self.lock.is_some(), "asset_operation_failed")?;
        self.fs.verify()?;
        self.fs.check_entry(
            self.lock_entry
                .ok_or_else(|| fail("asset_operation_failed"))?,
        )
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
        Self::from_fs(Fs::open(
            file,
            files::Access {
                owner: Some(owner),
                hook,
            },
        )?)
    }
}
