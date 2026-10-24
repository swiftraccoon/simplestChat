//! The reference benchmark lock and independent persistent release guard.

use super::{
    control::{Failure, Owner, Result},
    files::{Dir, Held},
};
use std::{os::unix::fs::MetadataExt, path::Path, time::Instant};

struct Protected {
    file: Held,
    parent: Dir,
}
impl Protected {
    fn open(
        path: &Path,
        maximum: usize,
        expected_uid: u32,
        mask: u32,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<Self> {
        let parent = Dir::open(path.parent().ok_or(Failure::Guard)?)?;
        let directory = match parent.descriptor().metadata() {
            Ok(directory) => directory,
            Err(_) => {
                let _ = parent.finish();
                return Err(Failure::Io);
            }
        };
        if directory.uid() != expected_uid || directory.mode() & mask != 0o700 {
            let _ = parent.finish();
            return Err(Failure::Guard);
        }
        let mut file = match parent.read(path.file_name().ok_or(Failure::Guard)?) {
            Ok(file) => file,
            Err(error) => {
                let _ = parent.finish();
                return Err(error);
            }
        };
        let admitted = (|| {
            let stamp = file.file().metadata().map_err(|_| Failure::Io)?;
            if stamp.uid() != expected_uid
                || stamp.mode() & mask != 0o600
                || stamp.len() > maximum as u64
            {
                return Err(Failure::Guard);
            }
            file.seal(owner, end)?;
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = file.finish();
            let _ = parent.finish();
            return Err(error);
        }
        Ok(Self { file, parent })
    }
    fn finalized(&self, maximum: usize, owner: &mut Owner, end: Instant) -> Result<()> {
        let bytes = self.file.bytes(maximum, owner, end)?;
        let value = super::decode_metadata(&bytes, maximum).map_err(|_| Failure::Guard)?;
        let schema = matches!(
            value.get("schemaVersion"),
            Some(crate::json::Json::Integer(value)) if value == "1"
        );
        if !schema || !matches!(value.get("finalized"), Some(crate::json::Json::Bool(true))) {
            return Err(Failure::Guard);
        }
        Ok(())
    }
    fn finish(self) -> Result<()> {
        let file = self.file.finish();
        let directory = self.parent.finish();
        file.and(directory)
    }
}

pub(super) struct Guards {
    lock: Option<Protected>,
    states: Vec<Protected>,
}
impl Guards {
    pub fn open(owner: &mut Owner, end: Instant) -> Result<Self> {
        Self::at(
            Path::new("/run/simplestchat-bench"),
            Path::new("/srv/simplestchat-public/release-state.json"),
            0,
            owner,
            end,
        )
    }
    fn at(
        benchmark: &Path,
        release: &Path,
        expected_uid: u32,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<Self> {
        let mut guards = Self {
            lock: None,
            states: Vec::new(),
        };
        let admitted = (|| {
            owner.before(end)?;
            if benchmark.symlink_metadata().is_ok() {
                let lock = Protected::open(
                    &benchmark.join("workload.lock"),
                    usize::MAX,
                    expected_uid,
                    0o777,
                    owner,
                    end,
                )?;
                guards.lock = Some(lock);
                rustix::fs::flock(
                    guards.lock.as_ref().unwrap().file.file(),
                    rustix::fs::FlockOperation::NonBlockingLockExclusive,
                )
                .map_err(|_| Failure::Guard)?;
                let current = benchmark.join("current.json");
                if current.symlink_metadata().is_ok() {
                    let state = Protected::open(&current, 65536, expected_uid, 0o777, owner, end)?;
                    guards.states.push(state);
                    guards.states.last().unwrap().finalized(65536, owner, end)?;
                }
            }
            if release.symlink_metadata().is_ok() {
                let state = Protected::open(release, 16384, expected_uid, 0o7777, owner, end)?;
                guards.states.push(state);
                guards.states.last().unwrap().finalized(16384, owner, end)?;
            }
            guards.check()
        })();
        if let Err(error) = admitted {
            let _ = guards.finish();
            return Err(error);
        }
        Ok(guards)
    }
    #[cfg(test)]
    pub fn fixture(
        benchmark: &Path,
        release: &Path,
        owner: &mut Owner,
        end: Instant,
    ) -> Result<Self> {
        Self::at(
            benchmark,
            release,
            rustix::process::getuid().as_raw(),
            owner,
            end,
        )
    }
    pub fn check(&self) -> Result<()> {
        if let Some(lock) = &self.lock {
            lock.file.check(false)?;
        }
        for state in &self.states {
            state.file.check(false)?;
        }
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let mut failure = None;
        for state in self.states {
            if let Err(error) = state.finish() {
                failure.get_or_insert(error);
            }
        }
        if let Some(lock) = self.lock {
            if rustix::fs::flock(lock.file.file(), rustix::fs::FlockOperation::Unlock).is_err() {
                failure.get_or_insert(Failure::Close);
            }
            if let Err(error) = lock.finish() {
                failure.get_or_insert(error);
            }
        }
        checked?;
        failure.map_or(Ok(()), Err)
    }
}
