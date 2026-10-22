//! A selected original Git executable and a config-free view of local SHA-1 objects.
//! Git/runtime/object database authority is caller-owned, not upstream authentication.

use super::*;
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) struct Git {
    parent: Chain,
    leaf: Held,
    stamp: Metadata,
    path: PathBuf,
    budget: Budget,
}
impl std::fmt::Debug for Git {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedOpsSourceGit")
    }
}
impl Git {
    pub(super) async fn open<E: Executor, C: Clock>(
        selected: &request::GitSelection,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Self> {
        let parent = Chain::open(selected.path.parent().ok_or_else(|| failure(1))?, budget)?;
        let leaf = Held::open(
            parent.file(),
            selected.path.file_name().unwrap(),
            namespace::FLAGS,
            budget,
        )?;
        let stamp = io(leaf.file().metadata())?;
        if !stamp.is_file()
            || stamp.mode() & 0o6000 != 0
            || stamp.mode() & 0o022 != 0
            || ![0, rustix::process::getuid().as_raw()].contains(&stamp.uid())
            || stamp.mode() & 0o111 == 0
            || stamp.len() != selected.bytes
        {
            return Err(failure(1));
        }
        let mut digest = Sha256::new();
        let mut count = 0;
        let mut buffer = [0u8; 64 * 1024];
        let mut reader = leaf.file();
        loop {
            executor.ready().await?;
            deadline.check(false)?;
            let n = io(reader.read(&mut buffer))?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > selected.bytes {
                return Err(failure(1));
            }
            digest.update(&buffer[..n]);
        }
        if count != selected.bytes || hex(&digest.finalize()) != selected.sha256 {
            return Err(failure(1));
        }
        let value = Self {
            parent,
            leaf,
            stamp,
            path: selected.path.clone(),
            budget: budget.clone(),
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn validate(&self) -> Result<&Path> {
        self.parent.verify()?;
        if !namespace::same(&self.stamp, &io(self.leaf.file().metadata())?) {
            return Err(failure(1));
        }
        let visible = Held::open(
            self.parent.file(),
            self.path.file_name().unwrap(),
            namespace::FLAGS,
            &self.budget,
        )?;
        let matched = namespace::same(&self.stamp, &io(visible.file().metadata())?);
        visible.finish()?;
        if matched {
            Ok(&self.path)
        } else {
            Err(failure(1))
        }
    }
    pub(super) fn finish(self) -> Result<()> {
        let checked = self.validate().map(|_| ());
        let leaf = self.leaf.finish();
        let parent = self.parent.finish();
        checked.and(leaf).and(parent)
    }
}

pub(super) struct Repository {
    root: Chain,
    objects: Chain,
    budget: Budget,
}
fn absent(parent: &File, name: &str, budget: &Budget) -> Result<()> {
    let _permit = budget.permit()?;
    match rustix::fs::openat(parent, name, namespace::FLAGS, Mode::empty()) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Ok(fd) => {
            let closed = nix::unistd::close(File::from(fd));
            closed.map_err(|_| failure(1))?;
            Err(failure(1))
        }
        Err(_) => Err(failure(1)),
    }
}
impl Repository {
    pub fn open(path: &Path, budget: &Budget) -> Result<Self> {
        let root = Chain::open(path, budget)?;
        let objects = Chain::open(&path.join(".git/objects"), budget)?;
        let value = Self {
            root,
            objects,
            budget: budget.clone(),
        };
        value.check()?;
        Ok(value)
    }
    pub fn check(&self) -> Result<()> {
        self.root.verify()?;
        self.objects.verify()?;
        let git = Chain::open(&self.root.path.join(".git"), &self.budget)?;
        let checked = (|| {
            absent(git.file(), "commondir", &self.budget)?;
            absent(git.file(), "gitdir", &self.budget)?;
            let _permit = self.budget.permit()?;
            let info = rustix::fs::openat(
                self.objects.file(),
                "info",
                namespace::FLAGS | OFlags::DIRECTORY,
                Mode::empty(),
            );
            match info {
                Err(rustix::io::Errno::NOENT) => Ok(()),
                Err(_) => Err(failure(1)),
                Ok(info) => {
                    let info = File::from(info);
                    let checked = absent(&info, "alternates", &self.budget)
                        .and_then(|()| absent(&info, "http-alternates", &self.budget));
                    let closed = nix::unistd::close(info).map_err(|_| failure(1));
                    checked.and(closed)
                }
            }
        })();
        let closed = git.finish();
        checked.and(closed)
    }
    pub fn objects(&self) -> &Path {
        &self.objects.path
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let objects = self.objects.finish();
        let root = self.root.finish();
        checked.and(objects).and(root)
    }
}

pub(super) fn command(git: &Arc<Git>, args: &[&str], maximum: u64) -> Command {
    let mut command = Command::capture(
        Program::OpsGit,
        &[
            "--no-pager",
            "--no-replace-objects",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "--git-dir",
            "git-view",
        ],
    );
    command
        .arguments
        .extend(args.iter().map(std::ffi::OsString::from));
    command.capture = Some(maximum);
    command.source_git = Some(git.clone());
    command
}
