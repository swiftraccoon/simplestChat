//! Private exported namespace. Exact writer/hash stamps are retained, not resampled.

use super::*;
use rustix::fs::{Mode, OFlags};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{File, Metadata},
};

pub(super) struct Tree {
    root: Held,
    stamps: BTreeMap<String, Metadata>,
    budget: Budget,
}
impl Tree {
    pub fn create(run: &OwnedRun) -> Result<Self> {
        let root = run.directory("source")?;
        let stamp = namespace::private_directory(&root)?;
        Ok(Self {
            root,
            stamps: BTreeMap::from([(String::new(), stamp)]),
            budget: run.budget().clone(),
        })
    }
    fn walk(&self, path: &str) -> Result<Vec<Held>> {
        let mut result = Vec::new();
        let mut prefix = String::new();
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let parent = result.last().map_or(self.root.file(), Held::file);
            let directory = Held::open(
                parent,
                OsStr::new(part),
                namespace::FLAGS | OFlags::DIRECTORY,
                &self.budget,
            )?;
            let stamp = namespace::private_directory(&directory)?;
            if !namespace::binding(self.stamps.get(&prefix).ok_or_else(|| failure(1))?, &stamp) {
                return Err(failure(1));
            }
            result.push(directory);
        }
        Ok(result)
    }
    fn parent(&self, name: &str) -> Result<Vec<Held>> {
        self.walk(name.rsplit_once('/').map_or("", |(parent, _)| parent))
    }
    pub fn directory(&mut self, name: &str) -> Result<()> {
        let parents = self.parent(name)?;
        let parent = parents.last().map_or(self.root.file(), Held::file);
        let basename = name.rsplit('/').next().unwrap();
        let result = (|| {
            rustix::fs::mkdirat(parent, basename, Mode::from_bits_truncate(0o700))
                .map_err(|_| failure(1))?;
            let directory = Held::open(
                parent,
                OsStr::new(basename),
                namespace::FLAGS | OFlags::DIRECTORY,
                &self.budget,
            )?;
            let stamp = namespace::private_directory(&directory);
            let closed = directory.finish();
            let stamp = stamp?;
            closed?;
            self.stamps.insert(name.into(), stamp);
            Ok(())
        })();
        result.and(close(parents))
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], executable: bool) -> Result<()> {
        let parents = self.parent(name)?;
        let parent = parents.last().map_or(self.root.file(), Held::file);
        let stamp = namespace::write_mode(
            parent,
            name.rsplit('/').next().unwrap(),
            bytes,
            executable,
            &self.budget,
        );
        let closed = close(parents);
        let stamp = stamp?;
        closed?;
        self.stamps.insert(name.into(), stamp);
        Ok(())
    }
    pub fn check(&self, run: &OwnedRun) -> Result<()> {
        run.check()?;
        let root = Held::open(
            run.file(),
            OsStr::new("source"),
            namespace::FLAGS | OFlags::DIRECTORY,
            &self.budget,
        )?;
        let matched = namespace::binding(&self.stamps[""], &namespace::private_directory(&root)?);
        root.finish()?;
        if !matched {
            return Err(failure(1));
        }
        for name in self.stamps.keys().filter(|name| !name.is_empty()) {
            self.check_one(name)?;
        }
        Ok(())
    }
    pub fn check_one(&self, name: &str) -> Result<()> {
        let selected = self.stamps.get(name).ok_or_else(|| failure(1))?;
        let parents = self.parent(name)?;
        let parent = parents.last().map_or(self.root.file(), Held::file);
        let observed = Held::open(
            parent,
            OsStr::new(name.rsplit('/').next().unwrap()),
            namespace::FLAGS,
            &self.budget,
        );
        let checked = observed.and_then(|file| {
            let result = io(file.file().metadata()).and_then(|stamp| {
                if if stamp.is_dir() {
                    namespace::binding(selected, &stamp)
                } else {
                    namespace::same(selected, &stamp)
                } {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            result.and(file.finish())
        });
        checked.and(close(parents))
    }
    pub fn finish(self, run: &OwnedRun) -> Result<()> {
        let checked = self.check(run);
        checked.and(self.root.finish())
    }
}
fn close(values: Vec<Held>) -> Result<()> {
    let mut result = Ok(());
    for value in values.into_iter().rev() {
        result = result.and(value.finish());
    }
    result
}

pub(super) fn directory(parent: &File, name: &str, budget: &Budget) -> Result<Held> {
    rustix::fs::mkdirat(parent, name, Mode::from_bits_truncate(0o700)).map_err(|_| failure(1))?;
    let result = Held::open(
        parent,
        OsStr::new(name),
        namespace::FLAGS | OFlags::DIRECTORY,
        budget,
    )?;
    namespace::private_directory(&result)?;
    Ok(result)
}
