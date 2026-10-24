//! The fixed verifier executable and restricted original calling environment.

use super::super::{
    control::{self, Cause, Owner, Result, Window},
    files::{Dir, Retained},
};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{OsStr, OsString},
    fs::Metadata,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) type Environment = BTreeMap<OsString, OsString>;
pub(super) fn restricted(source: impl IntoIterator<Item = (OsString, OsString)>) -> Environment {
    let mut environment: Environment = source
        .into_iter()
        .filter(|(key, _)| {
            [
                "PATH",
                "HOME",
                "USER",
                "LOGNAME",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "XDG_CONFIG_HOME",
            ]
            .iter()
            .any(|name| key == OsStr::new(name))
        })
        .collect();
    environment.insert("LC_ALL".into(), "C".into());
    environment
}
struct Part {
    path: PathBuf,
    stamp: Metadata,
    link: Option<PathBuf>,
}
struct Alias {
    original: PathBuf,
    canonical: PathBuf,
    parts: Vec<Part>,
}
impl Alias {
    fn open(path: PathBuf, owner: &mut Owner) -> Result<Self> {
        owner.check(Window::Body)?;
        let canonical = control::io(owner, path.canonicalize())?;
        let mut todo: VecDeque<OsString> = path
            .components()
            .map(|part| part.as_os_str().to_owned())
            .collect();
        let mut current = PathBuf::new();
        let mut parts = Vec::new();
        let mut followed = 0;
        while let Some(part) = todo.pop_front() {
            owner.check(Window::Body)?;
            if part == OsStr::new("/") {
                current = PathBuf::from("/");
                continue;
            }
            if part == OsStr::new(".") {
                continue;
            }
            if part == OsStr::new("..") {
                current.pop();
                continue;
            }
            current.push(part);
            let stamp = control::io(owner, current.symlink_metadata())?;
            if stamp.file_type().is_symlink() {
                followed += 1;
                // Native canonicalization already applies the symlink limit;
                // this detects a concurrent new loop during witness collection.
                if followed > 40 {
                    return control::failed(owner, Cause::Changed);
                }
                let target = control::io(owner, std::fs::read_link(&current))?;
                parts.push(Part {
                    path: current.clone(),
                    stamp,
                    link: Some(target.clone()),
                });
                current.pop();
                for part in target.components().rev() {
                    todo.push_front(part.as_os_str().to_owned());
                }
            } else {
                parts.push(Part {
                    path: current.clone(),
                    stamp,
                    link: None,
                });
            }
        }
        if current != canonical {
            return control::failed(owner, Cause::Changed);
        }
        let alias = Self {
            original: path,
            canonical,
            parts,
        };
        alias.check(owner, Window::Body)?;
        Ok(alias)
    }
    fn check(&self, owner: &mut Owner, window: Window) -> Result<()> {
        for part in &self.parts {
            owner.check(window)?;
            let now = control::io(owner, part.path.symlink_metadata())?;
            let old = &part.stamp;
            let same = now.file_type() == old.file_type()
                && now.dev() == old.dev()
                && now.ino() == old.ino()
                && now.mode() == old.mode()
                && now.uid() == old.uid()
                && now.gid() == old.gid()
                && (old.is_dir()
                    || (now.len() == old.len()
                        && now.nlink() == old.nlink()
                        && (now.mtime(), now.mtime_nsec(), now.ctime(), now.ctime_nsec())
                            == (old.mtime(), old.mtime_nsec(), old.ctime(), old.ctime_nsec())));
            if !same {
                return control::failed(owner, Cause::Changed);
            }
            if let Some(target) = &part.link
                && control::io(owner, std::fs::read_link(&part.path))? != *target
            {
                return control::failed(owner, Cause::Changed);
            }
        }
        if control::io(owner, self.original.canonicalize())? != self.canonical {
            return control::failed(owner, Cause::Changed);
        }
        Ok(())
    }
}
pub(super) struct Tool {
    alias: Alias,
    directory: Dir,
    file: Retained,
    sha256: String,
}
impl Tool {
    pub fn resolve(environment: &Environment, owner: &mut Owner) -> Result<Self> {
        let path = environment.get(OsStr::new("PATH")).ok_or(Cause::Command)?;
        let root = control::io(owner, std::env::current_dir())?;
        let mut selected = None;
        for path in std::env::split_paths(path) {
            owner.check(Window::Body)?;
            let path = if path.is_absolute() {
                path.join("gh")
            } else {
                root.join(path).join("gh")
            };
            if path.is_file()
                && rustix::fs::accessat(
                    rustix::fs::CWD,
                    &path,
                    rustix::fs::Access::EXEC_OK,
                    rustix::fs::AtFlags::EACCESS,
                )
                .is_ok()
            {
                selected = Some(path);
                break;
            }
        }
        let selected = selected.ok_or(Cause::Command)?;
        let alias = Alias::open(selected, owner)?;
        let parent = alias.canonical.parent().ok_or(Cause::Command)?;
        let name = alias.canonical.file_name().ok_or(Cause::Command)?;
        let directory = Dir::open(parent, owner)?;
        let file = match Retained::open(directory.clone(), name, owner) {
            Ok(file) => file,
            Err(error) => {
                directory.finish(owner);
                return Err(error);
            }
        };
        let admitted = (|| {
            let stamp = control::io(owner, file.file().metadata())?;
            if stamp.mode() & 0o111 == 0 {
                return control::failed(owner, Cause::Command);
            }
            file.hash(owner)
        })();
        match admitted {
            Ok(sha256) => Ok(Self {
                alias,
                directory,
                file,
                sha256,
            }),
            Err(error) => {
                file.finish(owner);
                directory.finish(owner);
                Err(error)
            }
        }
    }
    pub fn path(&self) -> &Path {
        &self.alias.canonical
    }
    pub fn check(&self, owner: &mut Owner) -> Result<()> {
        self.alias.check(owner, Window::Body)?;
        self.file.check(owner, Window::Body)
    }
    pub fn bytes(&self, owner: &mut Owner) -> Result<()> {
        self.check(owner)?;
        let actual = self.file.hash(owner)?;
        if actual == self.sha256 {
            Ok(())
        } else {
            control::failed(owner, Cause::Changed)
        }
    }
    pub fn finish(self, owner: &mut Owner) {
        if let Err(cause) = self.alias.check(owner, Window::Final) {
            owner.record(cause);
        }
        self.file.finish(owner);
        self.directory.finish(owner);
    }
}
