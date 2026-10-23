//! Retained same-host proc/netns witnesses. These are observations under the
//! trusted rootful Linux daemon assumption, not authenticated kernel ownership.
use super::{
    failure,
    io::{Budget, Namespace, Permit, Process},
};
use crate::{ci::release_controller::files::Directory, tooling::Result};
use rustix::fs::{Mode, OFlags};
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::Read,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

const FILE: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
pub(super) const LIMIT: u64 = 64 * 1024;
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure(1))
}
pub(super) fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn stamped(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
pub(super) fn count(path: &Path) -> Result<usize> {
    let count = path.components().count();
    if !path.is_absolute()
        || count > 24
        || path
            .components()
            .any(|v| !matches!(v, Component::RootDir | Component::Normal(_)))
    {
        return Err(failure(1));
    }
    Ok(count)
}
pub(super) fn read(parent: &File, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut file = match rustix::fs::openat(parent, name, FILE, Mode::empty()) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(failure(1)),
    };
    let value = (|| {
        let before = file.metadata().map_err(|_| failure(1))?;
        if !before.is_file() {
            return Err(failure(1));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure(1))?;
        if bytes.len() as u64 > limit {
            return Err(failure(1));
        }
        if !binding(&before, &file.metadata().map_err(|_| failure(1))?) {
            return Err(failure(1));
        }
        Ok(bytes)
    })();
    let closed = close(file);
    let bytes = value?;
    closed?;
    Ok(Some(bytes))
}
pub(super) fn required(parent: &File, name: &str, limit: u64) -> Result<Vec<u8>> {
    read(parent, name, limit)?.ok_or_else(|| failure(1))
}
pub(super) fn ticks(bytes: &[u8], pid: u32) -> Result<u64> {
    let first = bytes
        .iter()
        .position(|b| *b == b' ')
        .ok_or_else(|| failure(1))?;
    if &bytes[..first] != pid.to_string().as_bytes() || bytes.get(first + 1) != Some(&b'(') {
        return Err(failure(1));
    }
    let end = bytes
        .iter()
        .rposition(|b| *b == b')')
        .ok_or_else(|| failure(1))?;
    let fields = std::str::from_utf8(bytes.get(end + 1..).ok_or_else(|| failure(1))?)
        .map_err(|_| failure(1))?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let value = fields.get(19).ok_or_else(|| failure(1))?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(failure(1));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| failure(1))
}
pub(super) fn membership(bytes: &[u8], id: &str) -> Result<PathBuf> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err(failure(1));
    }
    let value = lines[0].strip_prefix("0::").ok_or_else(|| failure(1))?;
    if value.bytes().any(|b| b < 32 || b == 127) || value.len() > 4096 {
        return Err(failure(1));
    }
    let path = Path::new(value);
    count(path)?;
    if path == Path::new("/")
        || !path.components().any(|part| match part {
            Component::Normal(name) => {
                name == id || name == OsString::from(format!("docker-{id}.scope"))
            }
            _ => false,
        })
    {
        return Err(failure(1));
    }
    Ok(path.to_owned())
}
#[derive(Clone)]
pub(super) struct Roots {
    pub proc: PathBuf,
    pub cgroup: PathBuf,
    real: bool,
}
impl Roots {
    pub fn native() -> Self {
        Self {
            proc: "/proc".into(),
            cgroup: "/sys/fs/cgroup".into(),
            real: true,
        }
    }
    #[cfg(test)]
    pub fn fixture(proc: PathBuf, cgroup: PathBuf) -> Self {
        Self {
            proc,
            cgroup,
            real: false,
        }
    }
}
pub(super) struct ProcessFiles {
    root: Option<Directory>,
    pid: Option<File>,
    namespace: Option<File>,
    pid_stamp: Metadata,
    namespace_stamp: Metadata,
    pub process: Process,
    pub cgroup: PathBuf,
    start: u64,
    roots: Roots,
    _permit: Permit,
}
impl ProcessFiles {
    pub fn open(roots: &Roots, process: &Process, budget: &Budget) -> Result<Self> {
        if process.pid <= 1
            || process.pid > i32::MAX as u32
            || process.id.len() != 64
            || !process
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || process.started_at.is_empty()
            || process.started_at.len() > 128
            || process.started_at.chars().any(char::is_control)
        {
            return Err(failure(1));
        }
        let permit = budget.reserve(count(&roots.proc)? + 3)?;
        let root = Directory::open(&roots.proc).map_err(|_| failure(1))?;
        let pid = match rustix::fs::openat(
            root.file(),
            process.pid.to_string(),
            FILE | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map(File::from)
        {
            Ok(file) => file,
            Err(_) => {
                let _ = root.finish();
                return Err(failure(1));
            }
        };
        let initial = (|| {
            let pid_stamp = pid.metadata().map_err(|_| failure(1))?;
            let start = ticks(&required(&pid, "stat", LIMIT)?, process.pid)?;
            let cgroup = membership(&required(&pid, "cgroup", LIMIT)?, &process.id)?;
            Ok((pid_stamp, start, cgroup))
        })();
        let (pid_stamp, start, cgroup) = match initial {
            Ok(value) => value,
            Err(error) => {
                let _ = close(pid);
                let _ = root.finish();
                return Err(error);
            }
        };
        let namespace = match Self::open_namespace(&pid, roots.real) {
            Ok(file) => file,
            Err(error) => {
                let _ = close(pid);
                let _ = root.finish();
                return Err(error);
            }
        };
        let namespace_stamp = match namespace.metadata() {
            Ok(value) => value,
            Err(_) => {
                let _ = close(namespace);
                let _ = close(pid);
                let _ = root.finish();
                return Err(failure(1));
            }
        };
        let value = Self {
            root: Some(root),
            pid: Some(pid),
            namespace: Some(namespace),
            pid_stamp,
            namespace_stamp,
            process: process.clone(),
            cgroup,
            start,
            roots: roots.clone(),
            _permit: permit,
        };
        if let Err(error) = value.check() {
            let _ = value.finish(true);
            return Err(error);
        }
        Ok(value)
    }
    fn open_namespace(pid: &File, real: bool) -> Result<File> {
        // Only this fixed kernel magic link may be followed. No received
        // namespace pathname or general descriptor map exists.
        let file = rustix::fs::openat(
            pid,
            "ns/net",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure(1))?;
        if real {
            #[cfg(target_os = "linux")]
            let valid = rustix::fs::fstatfs(&file).is_ok_and(|s| s.f_type as u64 == 0x6e73_6673);
            #[cfg(not(target_os = "linux"))]
            let valid = false;
            if !valid {
                let _ = close(file);
                return Err(failure(1));
            }
        }
        Ok(file)
    }
    pub fn check(&self) -> Result<()> {
        let root = self.root.as_ref().ok_or_else(|| failure(1))?;
        let pid = self.pid.as_ref().ok_or_else(|| failure(1))?;
        root.verify().map_err(|_| failure(1))?;
        let visible = rustix::fs::openat(
            root.file(),
            self.process.pid.to_string(),
            FILE | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure(1))?;
        let metadata = visible.metadata().map_err(|_| failure(1));
        let closed = close(visible);
        if !binding(&metadata?, &self.pid_stamp)
            || !binding(&pid.metadata().map_err(|_| failure(1))?, &self.pid_stamp)
        {
            return Err(failure(1));
        }
        closed?;
        if ticks(&required(pid, "stat", LIMIT)?, self.process.pid)? != self.start
            || membership(&required(pid, "cgroup", LIMIT)?, &self.process.id)? != self.cgroup
        {
            return Err(failure(1));
        }
        let visible = Self::open_namespace(pid, self.roots.real)?;
        let metadata = visible.metadata().map_err(|_| failure(1));
        let closed = close(visible);
        if !stamped(&metadata?, &self.namespace_stamp)
            || !stamped(
                &self
                    .namespace
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .metadata()
                    .map_err(|_| failure(1))?,
                &self.namespace_stamp,
            )
        {
            return Err(failure(1));
        }
        closed
    }
    pub fn descriptor_path(&self) -> Result<OsString> {
        self.check()?;
        Ok(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.namespace
                .as_ref()
                .ok_or_else(|| failure(1))?
                .as_raw_fd()
        )
        .into())
    }
    pub fn retained(&self) -> Result<()> {
        if !stamped(
            &self
                .namespace
                .as_ref()
                .ok_or_else(|| failure(1))?
                .metadata()
                .map_err(|_| failure(1))?,
            &self.namespace_stamp,
        ) {
            return Err(failure(1));
        }
        Ok(())
    }
    pub fn finish(mut self, terminal: bool) -> Result<()> {
        let checked = if terminal {
            self.retained()
        } else {
            self.check()
        };
        let namespace = self.namespace.take().map(close).unwrap_or(Ok(()));
        let pid = self.pid.take().map(close).unwrap_or(Ok(()));
        let root = self
            .root
            .take()
            .map(|d| d.finish().map_err(|_| failure(1)))
            .unwrap_or(Ok(()));
        checked.and(namespace).and(pid).and(root)
    }
}
pub(super) struct Lease {
    owners: Mutex<Option<ProcessFiles>>,
}
impl Lease {
    pub fn open(roots: &Roots, process: &Process, budget: &Budget) -> Result<Self> {
        Ok(Self {
            owners: Mutex::new(Some(ProcessFiles::open(roots, process, budget)?)),
        })
    }
    pub fn check_original(&self) -> Result<()> {
        self.owners
            .lock()
            .map_err(|_| failure(1))?
            .as_ref()
            .ok_or_else(|| failure(1))?
            .check()
    }
    pub fn descriptor_path(&self) -> Result<OsString> {
        self.owners
            .lock()
            .map_err(|_| failure(1))?
            .as_ref()
            .ok_or_else(|| failure(1))?
            .descriptor_path()
    }
}
impl Namespace for Lease {
    fn check(&self, process: &Process) -> Result<()> {
        let owners = self.owners.lock().map_err(|_| failure(1))?;
        let owners = owners.as_ref().ok_or_else(|| failure(1))?;
        if &owners.process != process {
            return Err(failure(1));
        }
        owners.check()
    }
    fn finish(&self) -> Result<()> {
        let owners = self.owners.lock().map_err(|_| failure(1))?.take();
        owners
            .map(|value| value.finish(false))
            .ok_or_else(|| failure(1))?
    }
}

#[cfg(test)]
pub(super) mod fixture {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    pub struct Tree {
        pub path: PathBuf,
        pub roots: Roots,
        pub processes: [Process; 2],
    }
    pub fn stat(pid: u32, start: u64) -> Vec<u8> {
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = "S".into();
        fields[19] = start.to_string();
        format!("{pid} (inert ) name) {}\n", fields.join(" ")).into_bytes()
    }
    impl Tree {
        pub fn new() -> Self {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "container-benchmark-adapter-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            let roots = Roots::fixture(path.join("proc"), path.join("cgroup"));
            fs::create_dir(&roots.proc).unwrap();
            fs::create_dir(&roots.cgroup).unwrap();
            fs::create_dir(path.join("namespaces")).unwrap();
            fs::write(roots.proc.join("uptime"), b"10.00 1.00\n").unwrap();
            let processes = std::array::from_fn(|n| Process {
                id: if n == 0 { "a" } else { "b" }.repeat(64),
                pid: 321 + n as u32,
                started_at: "2026-10-22T00:00:00Z".into(),
            });
            for process in &processes {
                let pid = roots.proc.join(process.pid.to_string());
                fs::create_dir(&pid).unwrap();
                fs::create_dir(pid.join("ns")).unwrap();
                let payload = path.join(format!("namespaces/{}", process.pid));
                fs::write(&payload, b"inert namespace").unwrap();
                std::os::unix::fs::symlink(payload, pid.join("ns/net")).unwrap();
                fs::write(pid.join("stat"), stat(process.pid, 99)).unwrap();
                fs::write(
                    pid.join("cgroup"),
                    format!("0::/system.slice/docker-{}.scope\n", process.id),
                )
                .unwrap();
                let group = roots
                    .cgroup
                    .join(format!("system.slice/docker-{}.scope", process.id));
                fs::create_dir_all(&group).unwrap();
                for (name, bytes) in [
                    ("cpu.stat", "usage_usec 1\n"),
                    ("memory.current", "4096\n"),
                    ("memory.events", "oom 0\n"),
                ] {
                    fs::write(group.join(name), bytes).unwrap();
                }
            }
            Self {
                path,
                roots,
                processes,
            }
        }
        pub fn private_file(&self, name: &str, bytes: &[u8]) -> File {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(self.path.join(name))
                .unwrap();
            file.write_all(bytes).unwrap();
            file
        }
        pub fn cgroup(&self, index: usize) -> PathBuf {
            self.roots.cgroup.join(format!(
                "system.slice/docker-{}.scope",
                self.processes[index].id
            ))
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Arc};
    #[test]
    fn process_start_and_membership_are_typed_not_name_or_pid_guesses() {
        assert_eq!(ticks(&fixture::stat(321, 99), 321).unwrap(), 99);
        assert!(ticks(&fixture::stat(321, 99), 322).is_err());
        assert!(ticks(b"321 (name) S 1", 321).is_err());
        let id = "a".repeat(64);
        assert_eq!(
            membership(format!("0::/docker/{id}\n").as_bytes(), &id).unwrap(),
            Path::new(&format!("/docker/{id}"))
        );
        for bytes in [
            b"0::/\n".as_slice(),
            b"0::/foreign\n",
            b"0::/../foreign\n",
            b"1::/x\n",
        ] {
            assert!(membership(bytes, &id).is_err());
        }
    }
    #[test]
    fn retained_lease_refuses_reused_process_and_replaced_namespace_without_deleting_foreign_data()
    {
        let tree = fixture::Tree::new();
        let budget = Budget::default();
        let lease = Arc::new(Lease::open(&tree.roots, &tree.processes[0], &budget).unwrap());
        let alias = lease.clone();
        lease.check(&tree.processes[0]).unwrap();
        assert!(
            lease
                .descriptor_path()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&format!("/proc/{}/fd/", std::process::id()))
        );
        let stat = tree.roots.proc.join("321/stat");
        fs::write(&stat, fixture::stat(321, 100)).unwrap();
        assert!(lease.check_original().is_err());
        assert!(lease.finish().is_err());
        assert!(alias.check_original().is_err());
        assert_eq!(budget.count(), 0);
        fs::write(stat, fixture::stat(321, 99)).unwrap();
        let lease = Lease::open(&tree.roots, &tree.processes[0], &budget).unwrap();
        let ns = tree.roots.proc.join("321/ns/net");
        fs::rename(&ns, ns.with_extension("original")).unwrap();
        fs::write(&ns, b"FOREIGN").unwrap();
        assert!(lease.check_original().is_err());
        assert!(lease.finish().is_err());
        assert_eq!(budget.count(), 0);
        assert_eq!(fs::read(ns).unwrap(), b"FOREIGN");
    }
    #[test]
    fn descriptor_admission_is_bounded_before_proc_acquisition() {
        let tree = fixture::Tree::new();
        let budget = Budget::default();
        let held = budget.reserve(126).unwrap();
        assert!(Lease::open(&tree.roots, &tree.processes[0], &budget).is_err());
        assert_eq!(budget.count(), 126);
        drop(held);
        assert_eq!(budget.count(), 0);
    }
}
