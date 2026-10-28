//! One byte-pinned old YAML contract, executed by its unchanged Python class.
//! The existing shard Control owns every deadline and final signal observation.
//! Copies are data custody, not retained-FD Python execution or same-UID isolation.
#[path = "python_legacy_maintenance/routing.rs"]
pub(super) mod routing;
use super::{Cause, Control, Outcome};
use crate::tooling::Environment;
use rustix::fs::{Mode, OFlags};
use std::{
    ffi::OsString,
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy)]
struct Pin {
    source: &'static str,
    relative: &'static str,
    bytes: usize,
    sha: &'static str,
}
const PINS: [Pin; 3] = [
    Pin {
        source: "ops/ansible/tests/test_release_playbook.py",
        relative: "ops/ansible/tests/test_release_playbook.py",
        bytes: routing::FROZEN_MODULE_BYTES,
        sha: routing::FROZEN_MODULE_SHA256,
    },
    Pin {
        source: "xtask/src/ci/automation/reference/maintenance-python.yml",
        relative: "ops/ansible/maintenance.yml",
        bytes: routing::OLD_PLAYBOOK_BYTES,
        sha: routing::OLD_PLAYBOOK_SHA256,
    },
    Pin {
        source: "xtask/src/ci/automation/reference/source-python.yml",
        relative: "ops/ansible/tasks/source.yml",
        bytes: routing::OLD_SOURCE_BYTES,
        sha: routing::OLD_SOURCE_SHA256,
    },
];
#[derive(Clone, Eq, PartialEq)]
struct Stamp {
    identity: (u64, u64),
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn new(m: &Metadata) -> Self {
        Self {
            identity: (m.dev(), m.ino()),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            links: m.nlink(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}
struct HeldFile {
    path: PathBuf,
    file: Option<File>,
    pin: Pin,
    stamp: Option<Stamp>,
}
struct HeldDirectory {
    path: PathBuf,
    file: Option<File>,
    identity: Option<(u64, u64)>,
}
pub(super) struct LegacyMaintenance {
    path: Option<PathBuf>,
    original_root: Option<PathBuf>,
    originals: Vec<HeldFile>,
    copies: Vec<HeldFile>,
    directories: Vec<HeldDirectory>,
    ready: bool,
    unadmitted_namespace: bool,
    #[cfg(test)]
    refuse_close: Option<usize>,
    #[cfg(test)]
    close_attempts: usize,
}
impl LegacyMaintenance {
    pub(super) fn ready_for_final_check(&self) -> bool {
        self.ready
    }
    pub(super) fn empty() -> Self {
        Self {
            path: None,
            original_root: None,
            originals: Vec::new(),
            copies: Vec::new(),
            directories: Vec::new(),
            ready: false,
            unadmitted_namespace: false,
            #[cfg(test)]
            refuse_close: None,
            #[cfg(test)]
            close_attempts: 0,
        }
    }
    fn directory(&mut self, path: PathBuf, control: &mut Control) -> Outcome<()> {
        control.body_fence()?;
        self.unadmitted_namespace = true;
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| Cause::Io)?;
        self.admit_directory(path, control)
    }
    fn admit_directory(&mut self, path: PathBuf, control: &mut Control) -> Outcome<()> {
        self.unadmitted_namespace = true;
        self.directories.push(HeldDirectory {
            path: path.clone(),
            file: None,
            identity: None,
        });
        let slot = self.directories.last_mut().ok_or(Cause::Io)?;
        let file = File::from(
            rustix::fs::open(
                &path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Cause::Io)?,
        );
        slot.file = Some(file);
        let m = slot
            .file
            .as_ref()
            .ok_or(Cause::Io)?
            .metadata()
            .map_err(|_| Cause::Io)?;
        if !m.is_dir()
            || m.uid() != rustix::process::getuid().as_raw()
            || m.mode() & 0o7777 != 0o700
        {
            return Err(Cause::Io);
        }
        slot.identity = Some((m.dev(), m.ino()));
        self.unadmitted_namespace = false;
        control.body_fence()
    }
    fn directories_current(&self) -> Outcome<()> {
        for dir in &self.directories {
            let held = dir
                .file
                .as_ref()
                .ok_or(Cause::Cleanup)?
                .metadata()
                .map_err(|_| Cause::Io)?;
            let visible = fs::symlink_metadata(&dir.path).map_err(|_| Cause::Io)?;
            if !held.is_dir()
                || !visible.is_dir()
                || Some((held.dev(), held.ino())) != dir.identity
                || (held.dev(), held.ino()) != (visible.dev(), visible.ino())
                || held.uid() != rustix::process::getuid().as_raw()
                || visible.mode() & 0o7777 != 0o700
            {
                return Err(Cause::Inventory);
            }
        }
        Ok(())
    }
    pub(super) fn prepare(
        &mut self,
        root: &Path,
        environment: &Environment,
        control: &mut Control,
    ) -> Outcome<()> {
        if self.path.is_some() || !self.originals.is_empty() {
            return Err(Cause::Inventory);
        }
        self.original_root = Some(root.to_owned());
        let mut payloads = Vec::new();
        for pin in PINS {
            control.body_fence()?;
            let path = root.join(pin.source);
            let file = File::from(
                rustix::fs::open(
                    &path,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| Cause::Inventory)?,
            );
            self.originals.push(HeldFile {
                path,
                file: Some(file),
                pin,
                stamp: None,
            });
            let held = self.originals.last_mut().ok_or(Cause::Io)?;
            let metadata = held
                .file
                .as_ref()
                .ok_or(Cause::Io)?
                .metadata()
                .map_err(|_| Cause::Inventory)?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != pin.bytes as u64 {
                return Err(Cause::Inventory);
            }
            held.stamp = Some(Stamp::new(&metadata));
            payloads.push(read(held, control, true)?);
        }
        let parent = ["RUNNER_TEMP", "TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .find_map(|name| {
                environment
                    .get(std::ffi::OsStr::new(name))
                    .filter(|v| !v.is_empty())
            })
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
            .canonicalize()
            .map_err(|_| Cause::Io)?;
        for _ in 0..32 {
            control.body_fence()?;
            let path = parent.join(format!(
                "simplestchat-maintenance-reference-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            self.unadmitted_namespace = true;
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    self.admit_directory(path.clone(), control)?;
                    self.path = Some(path);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.unadmitted_namespace = false;
                    continue;
                }
                Err(_) => return Err(Cause::Io),
            }
        }
        let path = self.path.as_ref().ok_or(Cause::Io)?.clone();
        for name in [
            "ops",
            "ops/ansible",
            "ops/ansible/tests",
            "ops/ansible/tasks",
        ] {
            self.directory(path.join(name), control)?;
        }
        for (pin, bytes) in PINS.into_iter().zip(payloads) {
            self.directories_current()?;
            control.body_fence()?;
            let path = path.join(pin.relative);
            self.unadmitted_namespace = true;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
                .open(&path)
                .map_err(|_| Cause::Io)?;
            self.copies.push(HeldFile {
                path,
                file: Some(file),
                pin,
                stamp: None,
            });
            let held = self.copies.last_mut().ok_or(Cause::Io)?;
            let file = held.file.as_mut().ok_or(Cause::Io)?;
            // Register the actual created inode before writing or a late fence.
            held.stamp = Some(Stamp::new(&file.metadata().map_err(|_| Cause::Io)?));
            self.unadmitted_namespace = false;
            for chunk in bytes.chunks(16384) {
                control.body_fence()?;
                file.write_all(chunk).map_err(|_| Cause::Io)?;
            }
            file.sync_all().map_err(|_| Cause::Io)?;
            held.stamp = Some(Stamp::new(&file.metadata().map_err(|_| Cause::Io)?));
            read(held, control, true)?;
        }
        self.ready = true;
        self.current(control)
    }
    pub(super) fn current(&self, control: &mut Control) -> Outcome<()> {
        control.final_fence()?;
        if !self.ready || self.unadmitted_namespace {
            return Err(Cause::Inventory);
        }
        self.directories_current()?;
        for record in self.originals.iter().chain(&self.copies) {
            read(record, control, false)?;
        }
        control.final_fence()
    }
    pub(super) fn environment(&self, base: &Environment) -> Outcome<Environment> {
        if !self.ready {
            return Err(Cause::Inventory);
        }
        let mut environment = base.clone();
        let pythonpath = routing::legacy_pythonpath(
            self.path.as_ref().ok_or(Cause::Inventory)?,
            self.original_root.as_ref().ok_or(Cause::Inventory)?,
            base.get(std::ffi::OsStr::new("PYTHONPATH"))
                .map(OsString::as_os_str),
        )
        .map_err(|_| Cause::Inventory)?;
        environment.insert("PYTHONPATH".into(), pythonpath);
        Ok(environment)
    }
    pub(super) fn close(&mut self, control: &mut Control) -> Outcome<()> {
        let mut first = None;
        let mut note = |result: Outcome<()>| {
            if first.is_none() {
                first = result.err();
            }
        };
        #[cfg(test)]
        let refused = self.refuse_close;
        #[cfg(not(test))]
        let refused = None;
        let mut closes = 0;
        if self.ready {
            note(self.current(control));
        }
        if self.unadmitted_namespace {
            note(Err(Cause::Cleanup));
        }
        for held in &mut self.copies {
            let admitted = (|| {
                let current = fs::symlink_metadata(&held.path).map_err(|_| Cause::Cleanup)?;
                let stamp = held.stamp.as_ref().ok_or(Cause::Cleanup)?;
                let file = held
                    .file
                    .as_ref()
                    .ok_or(Cause::Cleanup)?
                    .metadata()
                    .map_err(|_| Cause::Cleanup)?;
                if !current.is_file()
                    || (current.dev(), current.ino()) != stamp.identity
                    || (file.dev(), file.ino()) != stamp.identity
                    || current.uid() != rustix::process::getuid().as_raw()
                {
                    return Err(Cause::Cleanup);
                }
                fs::remove_file(&held.path).map_err(|_| Cause::Cleanup)
            })();
            note(admitted);
            note(close_known(&mut held.file, &mut closes, refused));
        }
        for held in &mut self.originals {
            note(close_known(&mut held.file, &mut closes, refused));
        }
        for dir in self.directories.iter_mut().rev() {
            note((|| {
                let m = fs::symlink_metadata(&dir.path).map_err(|_| Cause::Cleanup)?;
                if !m.is_dir()
                    || Some((m.dev(), m.ino())) != dir.identity
                    || m.uid() != rustix::process::getuid().as_raw()
                    || m.mode() & 0o7777 != 0o700
                {
                    return Err(Cause::Cleanup);
                }
                fs::remove_dir(&dir.path).map_err(|_| Cause::Cleanup)
            })());
            note(close_known(&mut dir.file, &mut closes, refused));
        }
        self.ready = false;
        #[cfg(test)]
        {
            self.close_attempts = closes;
        }
        note(control.final_fence());
        first.map_or(Ok(()), Err)
    }
}
fn close_known(file: &mut Option<File>, closes: &mut usize, refused: Option<usize>) -> Outcome<()> {
    let Some(file) = file.take() else {
        return Ok(());
    };
    let ordinal = *closes;
    *closes += 1;
    let actual = nix::unistd::close(file).map_err(|_| Cause::Close);
    actual?;
    if refused == Some(ordinal) {
        Err(Cause::Close)
    } else {
        Ok(())
    }
}
#[cfg(test)]
impl LegacyMaintenance {
    pub(super) fn private_root(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub(super) fn inject_close_refusal(&mut self, ordinal: usize) {
        self.refuse_close = Some(ordinal);
    }
    pub(super) fn all_known_handles_closed(&self) -> bool {
        self.originals
            .iter()
            .chain(&self.copies)
            .all(|f| f.file.is_none())
            && self.directories.iter().all(|d| d.file.is_none())
    }
    pub(super) fn close_attempts(&self) -> usize {
        self.close_attempts
    }
}
fn read(held: &HeldFile, control: &mut Control, body: bool) -> Outcome<Vec<u8>> {
    let fence = |owner: &mut Control| {
        if body {
            owner.body_fence()
        } else {
            owner.final_fence()
        }
    };
    fence(control)?;
    let file = held.file.as_ref().ok_or(Cause::Close)?;
    let expected = held.stamp.as_ref().ok_or(Cause::Inventory)?;
    let check = || {
        let a = file.metadata().map_err(|_| Cause::Inventory)?;
        let b = fs::symlink_metadata(&held.path).map_err(|_| Cause::Inventory)?;
        if !a.is_file()
            || a.nlink() != 1
            || Stamp::new(&a) != *expected
            || Stamp::new(&b) != *expected
        {
            return Err(Cause::Inventory);
        }
        Ok(())
    };
    check()?;
    let mut bytes = vec![0; held.pin.bytes + 1];
    let mut at = 0;
    loop {
        fence(control)?;
        let count = file
            .read_at(&mut bytes[at..], at as u64)
            .map_err(|_| Cause::Io)?;
        fence(control)?;
        if count == 0 {
            break;
        }
        at += count;
        if at > held.pin.bytes {
            return Err(Cause::Inventory);
        }
    }
    bytes.truncate(at);
    if !routing::source_matches(&bytes, held.pin.bytes, held.pin.sha) {
        return Err(Cause::Inventory);
    }
    check()?;
    fence(control)?;
    Ok(bytes)
}
