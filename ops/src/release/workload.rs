//! One actual acquired flock shared only by fixed same-process release owners.

use std::{fs::File, os::unix::fs::MetadataExt, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Busy,
    Io,
    Changed,
    Close,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};

    #[test]
    fn actual_busy_flock_remains_first_when_its_rejected_original_close_also_refuses() {
        let fixture = crate::test_support::Fixture::new();
        let path = fixture.0.join("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let held = Lease::acquire(file).ok();
        let mut close_attempted = false;
        let acquired = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map(|file| {
                Lease::acquire_with(file, |file| {
                    close_attempted = true;
                    let _ = nix::unistd::close(file);
                    Err(nix::errno::Errno::EIO)
                })
            });
        let facts = match acquired {
            Ok(Err(failed)) => Some((failed.first, failed.close_failed)),
            Ok(Ok(extra)) => {
                let (_, file) = extra.release();
                if let Some(file) = file {
                    let _ = nix::unistd::close(file);
                }
                None
            }
            Err(_) => None,
        };
        let released = held.map(|lease| {
            let (unlocked, original) = lease.release();
            let closed = original.is_some_and(|file| nix::unistd::close(file).is_ok());
            unlocked.is_ok() && closed
        });
        assert_eq!(facts, Some((Failure::Busy, true)));
        assert!(close_attempted);
        assert_eq!(released, Some(true));
    }
}
#[derive(Debug)]
pub(super) struct AcquisitionFailure {
    pub first: Failure,
    pub close_failed: bool,
}
struct Held {
    file: File,
    original: std::fs::Metadata,
}
/// This capability exists only after a successful flock on its original file.
/// A borrowed clone cannot unlock it; the sole owner must settle every borrower.
#[derive(Clone)]
pub(super) struct Lease(Arc<Held>);
impl Lease {
    pub fn acquire(file: File) -> Result<Self, AcquisitionFailure> {
        Self::acquire_with(file, nix::unistd::close)
    }
    fn acquire_with(
        file: File,
        close: impl FnOnce(File) -> nix::Result<()>,
    ) -> Result<Self, AcquisitionFailure> {
        let acquired = (|| {
            let original = file.metadata().map_err(|_| Failure::Io)?;
            if !original.is_file() {
                return Err(Failure::Changed);
            }
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .map_err(|error| {
                    if error == rustix::io::Errno::WOULDBLOCK {
                        Failure::Busy
                    } else {
                        Failure::Io
                    }
                })?;
            Ok(original)
        })();
        match acquired {
            Ok(original) => Ok(Self(Arc::new(Held { file, original }))),
            Err(first) => {
                let close_failed = close(file).is_err();
                Err(AcquisitionFailure {
                    first,
                    close_failed,
                })
            }
        }
    }
    pub fn check(&self) -> Result<(), Failure> {
        let now = self.0.file.metadata().map_err(|_| Failure::Io)?;
        let first = &self.0.original;
        if !now.is_file()
            || first.dev() != now.dev()
            || first.ino() != now.ino()
            || first.mode() != now.mode()
            || first.uid() != now.uid()
            || first.gid() != now.gid()
            || first.nlink() != now.nlink()
            || first.len() != now.len()
            || (
                first.mtime(),
                first.mtime_nsec(),
                first.ctime(),
                first.ctime_nsec(),
            ) != (now.mtime(), now.mtime_nsec(), now.ctime(), now.ctime_nsec())
        {
            Err(Failure::Changed)
        } else {
            Ok(())
        }
    }
    /// Compare an admitted namespace probe with the already held original.
    pub fn matches(&self, file: &File) -> Result<bool, Failure> {
        self.check()?;
        let now = file.metadata().map_err(|_| Failure::Io)?;
        let first = &self.0.original;
        Ok(first.dev() == now.dev()
            && first.ino() == now.ino()
            && first.mode() == now.mode()
            && first.uid() == now.uid()
            && first.gid() == now.gid()
            && first.nlink() == now.nlink()
            && first.len() == now.len())
    }
    /// Return the original file for the caller's checked close after unlock.
    /// Even an unlock error returns the file so that close is still attempted.
    pub fn release(self) -> (Result<(), Failure>, Option<File>) {
        match Arc::try_unwrap(self.0) {
            Ok(held) => {
                let result = rustix::fs::flock(&held.file, rustix::fs::FlockOperation::Unlock)
                    .map_err(|_| Failure::Io);
                (result, Some(held.file))
            }
            Err(_) => (Err(Failure::Close), None),
        }
    }
}
