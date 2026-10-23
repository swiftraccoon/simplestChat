//! Bounded descriptive host/cgroup observations; no sampling subprocess exists.
use super::{
    failure,
    io::{self, Budget, Permit, Process, Role},
    namespace::{self, ProcessFiles, Roots},
};
use crate::{ci::release_controller::files::Directory, tooling::Result};
use rustix::fs::{Mode, OFlags};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    path::Path,
};

pub(super) const MAX_SAMPLES: usize = 512;
pub(super) const MAX_BYTES: usize = 64 * 1024 * 1024;
const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure(1))
}
struct Cgroup {
    parent: Option<Directory>,
    file: Option<File>,
    name: String,
    stamp: Metadata,
    _permit: Permit,
}
impl Cgroup {
    fn open(path: &Path, budget: &Budget) -> Result<Self> {
        namespace::count(path)?;
        let parent = path.parent().ok_or_else(|| failure(1))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| failure(1))?
            .to_owned();
        let permit = budget.reserve(namespace::count(parent)? + 2)?;
        let parent = Directory::open(parent).map_err(|_| failure(1))?;
        let file = match rustix::fs::openat(parent.file(), name.as_str(), FLAGS, Mode::empty())
            .map(File::from)
        {
            Ok(file) => file,
            Err(_) => {
                let _ = parent.finish();
                return Err(failure(1));
            }
        };
        let stamp = match file.metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                let _ = close(file);
                let _ = parent.finish();
                return Err(failure(1));
            }
        };
        let value = Self {
            parent: Some(parent),
            file: Some(file),
            name,
            stamp,
            _permit: permit,
        };
        if let Err(error) = value.check(false) {
            let _ = value.finish(true);
            return Err(error);
        }
        Ok(value)
    }
    /// None is a witnessed missing original basename, admitted only after the
    /// controller has independently observed this exact container terminal.
    fn check(&self, terminal: bool) -> Result<bool> {
        let parent = self.parent.as_ref().ok_or_else(|| failure(1))?;
        let file = self.file.as_ref().ok_or_else(|| failure(1))?;
        parent.verify().map_err(|_| failure(1))?;
        if !namespace::binding(&file.metadata().map_err(|_| failure(1))?, &self.stamp) {
            return Err(failure(1));
        }
        let visible =
            match rustix::fs::openat(parent.file(), self.name.as_str(), FLAGS, Mode::empty()) {
                Ok(fd) => File::from(fd),
                Err(rustix::io::Errno::NOENT) if terminal => return Ok(false),
                Err(_) => return Err(failure(1)),
            };
        let metadata = visible.metadata().map_err(|_| failure(1));
        let closed = close(visible);
        if !namespace::binding(&metadata?, &self.stamp) {
            return Err(failure(1));
        }
        closed?;
        Ok(true)
    }
    fn read(&self, terminal: bool) -> Result<Option<[String; 3]>> {
        if !self.check(terminal)? {
            return Ok(None);
        }
        let file = self.file.as_ref().ok_or_else(|| failure(1))?;
        let mut values = Vec::new();
        for name in ["cpu.stat", "memory.current", "memory.events"] {
            let Some(bytes) = namespace::read(file, name, namespace::LIMIT)? else {
                if terminal && !self.check(true)? {
                    return Ok(None);
                }
                return Err(failure(1));
            };
            values.push(String::from_utf8(bytes).map_err(|_| failure(1))?);
        }
        if !self.check(terminal)? {
            return Ok(None);
        }
        values.try_into().map(Some).map_err(|_| failure(1))
    }
    fn finish(mut self, terminal: bool) -> Result<()> {
        let checked = self.check(terminal).map(|_| ());
        let file = self.file.take().map(close).unwrap_or(Ok(()));
        let parent = self
            .parent
            .take()
            .map(|d| d.finish().map_err(|_| failure(1)))
            .unwrap_or(Ok(()));
        checked.and(file).and(parent)
    }
}
struct Group {
    process: ProcessFiles,
    cgroup: Cgroup,
}
impl Group {
    fn open(roots: &Roots, process: &Process, budget: &Budget) -> Result<Self> {
        let process = ProcessFiles::open(roots, process, budget)?;
        let relative = match process.cgroup.strip_prefix("/") {
            Ok(value) => value,
            Err(_) => {
                let _ = process.finish(true);
                return Err(failure(1));
            }
        };
        let path = roots.cgroup.join(relative);
        let cgroup = match Cgroup::open(&path, budget) {
            Ok(value) => value,
            Err(error) => {
                let _ = process.finish(true);
                return Err(error);
            }
        };
        let value = Self { process, cgroup };
        if let Err(error) = value.process.check() {
            let _ = value.finish(true);
            return Err(error);
        }
        Ok(value)
    }
    fn finish(self, terminal: bool) -> Result<()> {
        let cgroup = self.cgroup.finish(terminal);
        let process = self.process.finish(terminal);
        cgroup.and(process)
    }
}
pub(super) struct Groups {
    groups: Vec<Group>,
    proc_root: Option<Directory>,
    terminal: [bool; 2],
    samples: usize,
    bytes: usize,
    _permit: Permit,
}
impl Groups {
    pub fn new(roots: &Roots, processes: &[Process; 2], budget: &Budget) -> Result<Self> {
        if processes[0].id == processes[1].id || processes[0].pid == processes[1].pid {
            return Err(failure(1));
        }
        let permit = budget.reserve(namespace::count(&roots.proc)? + 1)?;
        let root = Directory::open(&roots.proc).map_err(|_| failure(1))?;
        let mut groups = Vec::new();
        for process in processes {
            match Group::open(roots, process, budget) {
                Ok(value) => groups.push(value),
                Err(error) => {
                    for group in groups {
                        let _ = group.finish(true);
                    }
                    let _ = root.finish();
                    return Err(error);
                }
            }
        }
        Ok(Self {
            groups,
            proc_root: Some(root),
            terminal: [false; 2],
            samples: 0,
            bytes: 0,
            _permit: permit,
        })
    }
}
impl io::Groups for Groups {
    fn sample(&mut self, at: &str, terminal: [bool; 2]) -> Result<Vec<Value>> {
        if self.samples >= MAX_SAMPLES
            || at.len() > 64
            || chrono::DateTime::parse_from_rfc3339(at).is_err()
            || self
                .terminal
                .iter()
                .zip(terminal)
                .any(|(old, new)| *old && !new)
        {
            return Err(failure(1));
        }
        self.samples += 1;
        self.terminal = terminal;
        let root = self.proc_root.as_ref().ok_or_else(|| failure(1))?;
        root.verify().map_err(|_| failure(1))?;
        let uptime = String::from_utf8(namespace::required(
            root.file(),
            "uptime",
            namespace::LIMIT,
        )?)
        .map_err(|_| failure(1))?;
        let mut rows = Vec::new();
        for role in Role::ALL {
            let group = &self.groups[role.index()];
            if terminal[role.index()] {
                group.process.retained()?;
            } else {
                group.process.check()?;
            }
            if let Some([cpu, memory, events]) = group.cgroup.read(terminal[role.index()])? {
                let value = json!({"at":at,"role":role.name(),"uptime":uptime,"cpu":cpu,
                    "memoryCurrent":memory,"memoryEvents":events});
                let size = serde_json::to_vec(&value).map_err(|_| failure(1))?.len() + 1;
                self.bytes = self
                    .bytes
                    .checked_add(size)
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or_else(|| failure(1))?;
                rows.push(value);
            }
            if !terminal[role.index()] {
                group.process.check()?;
            }
        }
        root.verify().map_err(|_| failure(1))?;
        Ok(rows)
    }
    fn finish(mut self) -> Result<()> {
        let mut result = Ok(());
        for (index, group) in self.groups.drain(..).enumerate() {
            let closed = group.finish(self.terminal[index]);
            result = result.and(closed);
        }
        let root = self
            .proc_root
            .take()
            .map(|d| d.finish().map_err(|_| failure(1)))
            .unwrap_or(Ok(()));
        result.and(root)
    }
}
pub(super) fn snapshot(roots: &Roots, budget: &Budget) -> Result<BTreeMap<String, Vec<u8>>> {
    let _permit = budget.reserve(namespace::count(&roots.proc)? + 2)?;
    let mut root = Some(Directory::open(&roots.proc).map_err(|_| failure(1))?);
    let result = (|| {
        let mut rows = BTreeMap::new();
        for (name, file, cap) in [
            ("stat", "stat", 512 * 1024),
            ("loadavg", "loadavg", 64 * 1024),
            ("meminfo", "meminfo", 64 * 1024),
        ] {
            rows.insert(
                name.into(),
                namespace::required(root.as_ref().unwrap().file(), file, cap)?,
            );
        }
        root = Some(
            root.take()
                .unwrap()
                .descend("pressure")
                .map_err(|_| failure(1))?,
        );
        for (name, file) in [
            ("pressure-cpu", "cpu"),
            ("pressure-io", "io"),
            ("pressure-memory", "memory"),
        ] {
            rows.insert(
                name.into(),
                namespace::required(root.as_ref().unwrap().file(), file, 64 * 1024)?,
            );
        }
        root.as_ref().unwrap().verify().map_err(|_| failure(1))?;
        Ok(rows)
    })();
    let closed = root
        .map(|d| d.finish().map_err(|_| failure(1)))
        .unwrap_or(Ok(()));
    let value = result?;
    closed?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::container_benchmark::{io::Groups as _, namespace::fixture::Tree};
    use std::{fs, os::unix::fs::symlink};
    const AT: &str = "2026-10-22T00:00:00Z";
    #[test]
    fn samples_original_groups_and_admits_disappearance_only_after_typed_terminal_observation() {
        let tree = Tree::new();
        let budget = Budget::default();
        let mut groups = Groups::new(&tree.roots, &tree.processes, &budget).unwrap();
        let rows = groups.sample(AT, [false; 2]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["role"], "server");
        assert_eq!(rows[1]["memoryCurrent"], "4096\n");
        fs::remove_dir_all(tree.cgroup(1)).unwrap();
        fs::remove_dir_all(tree.roots.proc.join("322")).unwrap();
        let rows = groups.sample(AT, [false, true]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["role"], "server");
        assert!(groups.sample(AT, [false; 2]).is_err());
        fs::remove_dir_all(tree.cgroup(0)).unwrap();
        fs::remove_dir_all(tree.roots.proc.join("321")).unwrap();
        assert!(groups.sample(AT, [true; 2]).unwrap().is_empty());
        groups.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    #[test]
    fn replaced_cgroup_and_oversized_inputs_refuse_without_adopting_or_deleting_replacement() {
        let tree = Tree::new();
        let budget = Budget::default();
        let mut groups = Groups::new(&tree.roots, &tree.processes, &budget).unwrap();
        let original = tree.cgroup(0);
        fs::rename(&original, original.with_extension("retained")).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("FOREIGN"), b"unchanged").unwrap();
        assert!(groups.sample(AT, [false; 2]).is_err());
        assert!(groups.finish().is_err());
        assert_eq!(budget.count(), 0);
        assert_eq!(fs::read(original.join("FOREIGN")).unwrap(), b"unchanged");
        let other = Tree::new();
        let mut groups = Groups::new(&other.roots, &other.processes, &budget).unwrap();
        fs::write(
            other.cgroup(1).join("cpu.stat"),
            vec![b'x'; namespace::LIMIT as usize + 1],
        )
        .unwrap();
        assert!(groups.sample(AT, [false; 2]).is_err());
        groups.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    #[test]
    fn snapshots_are_bounded_and_do_not_follow_pressure_directory_replacements() {
        let tree = Tree::new();
        let budget = Budget::default();
        for name in ["stat", "loadavg", "meminfo"] {
            fs::write(tree.roots.proc.join(name), b"owned\n").unwrap();
        }
        let pressure = tree.roots.proc.join("pressure");
        fs::create_dir(&pressure).unwrap();
        for name in ["cpu", "io", "memory"] {
            fs::write(pressure.join(name), b"some avg10=0\n").unwrap();
        }
        assert_eq!(snapshot(&tree.roots, &budget).unwrap().len(), 6);
        assert_eq!(budget.count(), 0);
        let saved = pressure.with_extension("original");
        fs::rename(&pressure, &saved).unwrap();
        symlink(&saved, &pressure).unwrap();
        assert!(snapshot(&tree.roots, &budget).is_err());
        assert_eq!(budget.count(), 0);
        assert_eq!(fs::read(saved.join("cpu")).unwrap(), b"some avg10=0\n");
    }
}
