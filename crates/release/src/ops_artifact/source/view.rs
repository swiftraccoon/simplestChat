//! Retained literal bare configuration. No caller repository configuration is loaded.

use super::*;
use rustix::fs::{Dir, OFlags};
use std::{collections::BTreeSet, ffi::OsStr, fs::Metadata};

pub(super) struct View {
    root: Held,
    stamp: Metadata,
    children: Vec<(&'static str, Metadata)>,
    budget: Budget,
}
impl View {
    pub fn create(run: &OwnedRun) -> Result<Self> {
        let root = run.directory("git-view")?;
        let stamp = namespace::private_directory(&root)?;
        let mut value = Self {
            root,
            stamp,
            children: vec![],
            budget: run.budget().clone(),
        };
        for name in ["objects", "refs"] {
            let directory = tree::directory(value.root.file(), name, &value.budget)?;
            let stamp = namespace::private_directory(&directory);
            let closed = directory.finish();
            value.children.push((name, stamp?));
            closed?;
        }
        for (name, bytes) in [
            (
                "config",
                b"[core]\nrepositoryformatversion = 0\nbare = true\n".as_slice(),
            ),
            ("HEAD", b"ref: refs/heads/unborn\n".as_slice()),
        ] {
            value.children.push((
                name,
                namespace::write(value.root.file(), name, bytes, &value.budget)?,
            ));
        }
        value.check(run)?;
        Ok(value)
    }
    pub fn check(&self, run: &OwnedRun) -> Result<()> {
        run.check()?;
        let visible = Held::open(
            run.file(),
            OsStr::new("git-view"),
            namespace::FLAGS | OFlags::DIRECTORY,
            &self.budget,
        )?;
        let matched = namespace::binding(&self.stamp, &namespace::private_directory(&visible)?)
            && namespace::binding(&self.stamp, &namespace::private_directory(&self.root)?);
        visible.finish()?;
        if !matched {
            return Err(failure(1));
        }
        let _permit = self.budget.permit()?;
        let mut names = BTreeSet::new();
        for entry in Dir::read_from(self.root.file()).map_err(|_| failure(1))? {
            let entry = entry.map_err(|_| failure(1))?;
            let name = entry.file_name().to_bytes();
            if [b".".as_slice(), b".."].contains(&name) {
                continue;
            }
            if ![b"config".as_slice(), b"HEAD", b"objects", b"refs"].contains(&name)
                || !names.insert(name.to_vec())
            {
                return Err(failure(1));
            }
        }
        if names.len() != 4 {
            return Err(failure(1));
        }
        for (name, stamp) in &self.children {
            let child = Held::open(
                self.root.file(),
                OsStr::new(name),
                namespace::FLAGS,
                &self.budget,
            )?;
            let matched =
                io(child.file().metadata()).is_ok_and(|value| namespace::same(stamp, &value));
            let closed = child.finish();
            closed?;
            if !matched {
                return Err(failure(1));
            }
        }
        Ok(())
    }
    pub fn finish(self, run: &OwnedRun) -> Result<()> {
        let checked = self.check(run);
        checked.and(self.root.finish())
    }
}
