//! An unsigned cache marker, with exact-inode rollback after uncertain visibility.

use super::{
    files::{close, io, same},
    plan::failure,
};
use crate::{
    ci::release_controller::files::{Directory, NamespaceBinding},
    tooling::Result,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::Write,
    path::{Path, PathBuf},
};

const FINAL: &str = "native-dtls-verified";
pub(super) struct Marker {
    binding: NamespaceBinding,
    parent: PathBuf,
    temporary: String,
    metadata: Metadata,
}
impl Marker {
    pub fn publish(
        parent: &Directory,
        path: &Path,
        sha: &str,
        token: &str,
    ) -> (Option<Self>, Result<()>) {
        Self::publish_after(parent, path, sha, token, || Ok(()))
    }
    pub(super) fn publish_after(
        parent: &Directory,
        path: &Path,
        sha: &str,
        token: &str,
        after: impl FnOnce() -> Result<()>,
    ) -> (Option<Self>, Result<()>) {
        let temporary = format!("native-dtls-verified.part-{token}");
        let file = match parent.create(&temporary) {
            Ok(file) => file,
            Err(_) => return (None, Err(failure(1))),
        };
        let mut metadata = file.metadata().ok();
        let result = (|| {
            io((&file)
                .write_all(format!("{sha}\n").as_bytes())
                .and_then(|()| file.sync_all()))?;
            metadata = Some(io(file.metadata())?);
            parent
                .check(&temporary, metadata.as_ref().unwrap())
                .map_err(|_| failure(1))?;
            let renamed = rustix::fs::renameat_with(
                parent.file(),
                temporary.as_str(),
                parent.file(),
                FINAL,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| failure(1));
            // Even an uncertain rename/sync result retains the staged inode.
            metadata = Some(io(file.metadata())?);
            renamed?;
            after()?;
            io(parent.file().sync_all())?;
            parent.verify().map_err(|_| failure(1))
        })();
        let closed = close(file);
        let marker = metadata.map(|metadata| Self {
            binding: parent.binding(),
            parent: path.into(),
            temporary,
            metadata,
        });
        (marker, result.and(closed))
    }
    pub fn rollback(self) -> Result<()> {
        let directory = self.binding.reopen(&self.parent).map_err(|_| failure(1))?;
        let mut passed = true;
        for name in [FINAL, self.temporary.as_str()] {
            let opened = rustix::fs::openat(
                directory.file(),
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            );
            let file = match opened {
                Ok(fd) => File::from(fd),
                Err(rustix::io::Errno::NOENT) => continue,
                Err(_) => {
                    passed = false;
                    continue;
                }
            };
            let observed = file.metadata();
            let owned = observed
                .as_ref()
                .is_ok_and(|value| same(value, &self.metadata));
            passed &= close(file).is_ok();
            if owned {
                passed &= rustix::fs::unlinkat(directory.file(), name, AtFlags::empty()).is_ok();
            } else {
                passed = false;
            }
        }
        passed &= directory.finish().is_ok();
        if passed { Ok(()) } else { Err(failure(1)) }
    }
}
