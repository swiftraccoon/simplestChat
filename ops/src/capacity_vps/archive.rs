//! Bounded evidence snapshot and streaming. No archive name selects a host path.

use super::{
    Failure, Result,
    control::{Owner, now},
    files::{self, Fs},
    host::{ROOT, Session},
    protocol::{self, Unit, Workload},
};
use flate2::{Compression, write::GzEncoder};
use rustix::fs::FileType;
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
};

const MAX_COMPRESSED: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 1024 * 1024 * 1024;
const MAX_MEMBER: u64 = 144 * 1024 * 1024;
const MAX_ENTRIES: usize = 10000;

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;

struct DirectoryWitness {
    path: String,
    before: Metadata,
    names: Vec<String>,
}
struct Member {
    path: String,
    name: String,
    before: Metadata,
}
struct Snapshot {
    directories: Vec<DirectoryWitness>,
    members: Vec<Member>,
    count: usize,
}
impl Snapshot {
    fn walk(fs: &mut Fs, owner: &mut Owner, root: &str) -> Result<Self> {
        let mut value = Self {
            directories: Vec::new(),
            members: Vec::new(),
            count: 0,
        };
        value.directory(fs, owner, root, "")?;
        Ok(value)
    }
    fn directory(&mut self, fs: &mut Fs, owner: &mut Owner, root: &str, name: &str) -> Result<()> {
        owner.check(false)?;
        let path = if name.is_empty() {
            root.into()
        } else {
            format!("{root}/{name}")
        };
        let directory = fs.directory(&path, false)?;
        let before = fs.dir_stamp(directory)?;
        let names = fs.names(directory, MAX_ENTRIES, owner)?;
        self.count = self.count.checked_add(names.len()).ok_or(Failure::Bounds)?;
        if self.count > MAX_ENTRIES {
            return Err(Failure::Bounds);
        }
        for child in &names {
            let directory = fs.directory(&path, false)?;
            let relative = if name.is_empty() {
                child.clone()
            } else {
                format!("{name}/{child}")
            };
            let child_path = format!("{root}/{relative}");
            match fs.kind(directory, child)? {
                FileType::Directory => self.directory(fs, owner, root, &relative)?,
                FileType::RegularFile => {
                    let id = fs.open(&child_path, MAX_MEMBER, None)?;
                    let before = fs.stamp(id).clone();
                    let closed = fs.close(id);
                    closed?;
                    self.members.push(Member {
                        path: child_path,
                        name: relative,
                        before,
                    });
                }
                _ => return Err(Failure::Archive),
            }
        }
        // Ancestors may have been explicitly closed while traversing a large
        // sibling tree. Reopening is checked identity, never original-FD custody.
        let directory = fs.directory(&path, false)?;
        if !files::same(&before, &fs.dir_stamp(directory)?)
            || fs.names(directory, MAX_ENTRIES, owner)? != names
        {
            return Err(Failure::Changed);
        }
        self.directories.push(DirectoryWitness {
            path,
            before,
            names,
        });
        fs.trim_directories()
    }
    fn verify(&self, fs: &mut Fs, owner: &mut Owner) -> Result<()> {
        for directory in &self.directories {
            owner.check(false)?;
            let id = fs.directory(&directory.path, false)?;
            if !files::same(&directory.before, &fs.dir_stamp(id)?)
                || fs.names(id, MAX_ENTRIES, owner)? != directory.names
            {
                return Err(Failure::Changed);
            }
            fs.trim_directories()?;
        }
        for member in &self.members {
            owner.check(false)?;
            let id = fs.open(&member.path, MAX_MEMBER, None)?;
            let checked = if files::same(&member.before, fs.stamp(id)) {
                Ok(())
            } else {
                Err(Failure::Changed)
            };
            let closed = fs.close(id);
            checked?;
            closed?;
            fs.trim_directories()?;
        }
        Ok(())
    }
}

struct Compressed<'a> {
    file: &'a mut File,
    end: u64,
}
impl Write for Compressed<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if now().map_err(|_| std::io::Error::other("archive_clock"))? >= self.end {
            return Err(std::io::Error::other("archive_deadline"));
        }
        let size = self.file.metadata()?.len();
        if bytes.len() as u64 > MAX_COMPRESSED.saturating_sub(size) {
            return Err(std::io::Error::other("archive_compressed_bound"));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
struct Expanded<'a, W> {
    inner: &'a mut W,
    count: u64,
    owner: &'a mut Owner,
}
impl<W: Write> Write for Expanded<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.owner
            .check(false)
            .map_err(|_| std::io::Error::other("archive_owner"))?;
        if bytes.len() as u64 > MAX_EXPANDED.saturating_sub(self.count) {
            return Err(std::io::Error::other("archive_expanded_bound"));
        }
        let count = self.inner.write(bytes)?;
        self.count += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn produce(snapshot: &Snapshot, fs: &mut Fs, owner: &mut Owner, file: &mut File) -> Result<()> {
    let compressed = Compressed {
        file,
        end: owner.end(),
    };
    let mut gzip = GzEncoder::new(compressed, Compression::default());
    let body = {
        let mut expanded = Expanded {
            inner: &mut gzip,
            count: 0,
            owner,
        };
        let mut tar = tar::Builder::new(&mut expanded);
        let body = (|| {
            for member in &snapshot.members {
                let id = fs.open(&member.path, MAX_MEMBER, None)?;
                let appended = (|| {
                    if !files::same(&member.before, fs.stamp(id)) {
                        return Err(Failure::Changed);
                    }
                    let mut header = tar::Header::new_ustar();
                    header.set_size(member.before.len());
                    header.set_mode(0o600);
                    header.set_uid(0);
                    header.set_gid(0);
                    header.set_mtime(0);
                    header.set_entry_type(tar::EntryType::Regular);
                    // Use mature local PAX only when USTAR cannot represent the
                    // bounded UTF-8 path; never emit a GNU long-name record.
                    let path = if header.set_path(&member.name).is_ok() {
                        member.name.as_str()
                    } else {
                        tar.append_pax_extensions([("path", member.name.as_bytes())])
                            .map_err(|_| Failure::Archive)?;
                        "bounded-evidence"
                    };
                    header.set_cksum();
                    files::io(fs.file_mut(id).seek(SeekFrom::Start(0)))?;
                    let appended = tar
                        .append_data(&mut header, path, fs.file_mut(id).take(member.before.len()))
                        .map_err(|_| Failure::Archive);
                    let checked = fs.check(id);
                    appended?;
                    checked?;
                    Ok(())
                })();
                let closed = fs.close(id);
                appended?;
                closed?;
                fs.trim_directories()?;
            }
            Ok(())
        })();
        if let Err(error) = &body {
            tar.get_mut().owner.latch(*error);
        }
        let finished = tar.finish().map_err(|_| Failure::Archive);
        body.and(finished)
    };
    if let Err(error) = &body {
        owner.latch(*error);
        owner.begin_cleanup();
    }
    let finished = gzip.try_finish().map_err(|_| Failure::Archive);
    body.and(finished)
}

struct Stdout {
    file: std::io::Stdout,
    previous: rustix::fs::OFlags,
    restored: bool,
}
impl Stdout {
    fn new() -> Result<Self> {
        let file = std::io::stdout();
        let previous = rustix::fs::fcntl_getfl(&file).map_err(|_| Failure::Io)?;
        let mut value = Self {
            file,
            previous,
            restored: false,
        };
        let set = rustix::fs::fcntl_setfl(&value.file, previous | rustix::fs::OFlags::NONBLOCK)
            .map_err(|_| Failure::Io);
        if let Err(error) = set {
            let _ = value.restore();
            return Err(error);
        }
        Ok(value)
    }
    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        rustix::fs::fcntl_setfl(&self.file, self.previous).map_err(|_| Failure::Io)
    }
    fn write(&mut self, bytes: &[u8], owner: &mut Owner) -> Result<()> {
        let mut position = 0;
        while position < bytes.len() {
            owner.check(false)?;
            match rustix::io::write(&self.file, &bytes[position..]) {
                Ok(0) => return Err(Failure::Io),
                Ok(n) => position += n,
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(_) => return Err(Failure::Io),
            }
        }
        Ok(())
    }
    fn finish(mut self) -> Result<()> {
        self.restore()
    }
}
impl Drop for Stdout {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

impl Session<'_> {
    pub fn archive(&mut self, workload: &Workload) -> Result<()> {
        self.require_saved(workload)?;
        let state = self.unit(Unit::Capacity, false)?;
        if !state.terminal() || state.main_pid != "0" {
            return Err(Failure::Unit);
        }
        let lock = self.lock()?;
        let body = (|| {
            let directory = self.header.evidence_path();
            let collected = self.fs.open(
                &format!("{directory}/collection.json"),
                protocol::MAX_RESPONSE as u64,
                Some(0o600),
            )?;
            self.fs.check(collected)?;
            let snapshot = Snapshot::walk(self.fs, self.owner, &directory)?;
            let mut random = [0u8; 16];
            let mut count = 0;
            while count < random.len() {
                self.owner.check(false)?;
                let n = rustix::rand::getrandom(
                    &mut random[count..],
                    rustix::rand::GetRandomFlags::NONBLOCK,
                )
                .map_err(|_| Failure::Io)?;
                if n == 0 {
                    return Err(Failure::Io);
                }
                count += n;
            }
            let suffix = random
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect::<String>();
            let path = format!(
                "{ROOT}/artifacts/.capacity-archive.{}.{suffix}.tmp",
                self.header.run
            );
            let writer = self.fs.writer(&path)?;
            let mut file = self.fs.take(writer)?;
            let produced = produce(&snapshot, self.fs, self.owner, &mut file);
            let synced = file.sync_all().map_err(|_| Failure::Io);
            let expected = file.metadata().map_err(|_| Failure::Io);
            let put = self.fs.put(writer, file);
            let written = if put.is_ok() {
                self.fs.written(writer)
            } else {
                Err(Failure::Unproven)
            };
            let closed = if put.is_ok() {
                self.fs.close(writer)
            } else {
                Ok(())
            };
            let prior = produced
                .err()
                .or_else(|| synced.err())
                .or_else(|| put.err())
                .or_else(|| written.err())
                .or_else(|| closed.err());
            if let Some(error) = prior {
                self.owner.latch(error);
                self.owner.begin_cleanup();
            }
            let reader = self.fs.open(&path, MAX_COMPRESSED, Some(0o600));
            let mut original = None;
            let admitted = (|| {
                let id = reader?;
                if !files::same(expected.as_ref().map_err(|e| *e)?, self.fs.stamp(id)) {
                    return Err(Failure::Changed);
                }
                original = Some(id);
                if let Some(error) = prior {
                    return Err(error);
                }
                self.fs.hash(id, self.owner)?;
                snapshot.verify(self.fs, self.owner)?;
                Ok(id)
            })();
            // This is an owned temporary transition, not deletion of an
            // archive-selected name. Its original readonly FD spans streaming.
            let removed = original.map(|id| self.fs.unlink(id)).unwrap_or(Ok(()));
            let streamed = match admitted {
                Ok(id) => {
                    removed?;
                    let mut stdout = Stdout::new()?;
                    let streamed = (|| {
                        let mut buffer = [0u8; 65536];
                        let mut total = 0u64;
                        loop {
                            self.owner.check(false)?;
                            self.fs.check(id)?;
                            let count = files::io(self.fs.file_mut(id).read(&mut buffer))?;
                            if count == 0 {
                                break;
                            }
                            total = total.checked_add(count as u64).ok_or(Failure::Bounds)?;
                            if total > MAX_COMPRESSED {
                                return Err(Failure::Bounds);
                            }
                            stdout.write(&buffer[..count], self.owner)?;
                        }
                        if total != self.fs.stamp(id).len() {
                            return Err(Failure::Changed);
                        }
                        self.fs.check(id)
                    })();
                    let restored = stdout.finish();
                    streamed.and(restored)
                }
                Err(error) => Err(error),
            };
            let closed = original.map(|id| self.fs.close(id)).unwrap_or(Ok(()));
            if let Some(error) = prior {
                return Err(error);
            }
            streamed.and(removed).and(closed)
        })();
        let unlocked = self.unlock(lock);
        body.and(unlocked)
    }
}
