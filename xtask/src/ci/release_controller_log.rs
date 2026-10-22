//! Broker namespace admission around the shared bounded combined output pipe.

use super::{
    files::{self, Directory},
    plan,
    protocol::Intent,
};
use crate::{ci::bounded_log, tooling::Result};
use std::{fs::File, os::unix::fs::MetadataExt};

pub(super) struct Log(bounded_log::Log);
impl Log {
    pub fn start(parent: &Directory, intent: &Intent, maximum: u64) -> Result<Self> {
        let output = rustix::fs::openat(
            parent.file(),
            plan::LOG,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| plan::failure())?;
        let admission = (|| {
            let metadata = output.metadata().map_err(|_| plan::failure())?;
            if !metadata.is_file()
                || metadata.len() != 0
                || files::identity(&output)? != intent.log
                || metadata.mode() & 0o777 != 0o600
                || metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err(plan::failure());
            }
            Ok(())
        })();
        if let Err(error) = admission {
            files::close(output)?;
            return Err(error);
        }
        bounded_log::Log::start(output, maximum)
            .map(Self)
            .map_err(|_| plan::failure())
    }
    pub fn writer(&self) -> Result<File> {
        self.0.writer().map_err(|_| plan::failure())
    }
    pub fn failed(&self) -> bool {
        self.0.failed()
    }
    #[cfg(test)]
    pub fn pipe_fds(&self) -> [i32; 2] {
        self.0.pipe_fds()
    }
    /// CLI groups must be retired before the core finishes its bounded EOF wait.
    pub async fn finish(self) -> Result<bounded_log::Finished> {
        self.0.finish().await.map_err(|_| plan::failure())
    }
}
