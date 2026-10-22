//! Fixed runner-file append. The captured file is a caller-owned exclusive
//! boundary; retained identity checks are optimistic, not locks or atomic CAS.

use crate::tooling::{Environment, Failure, Result};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    net::Ipv4Addr,
    os::unix::fs::MetadataExt,
    path::PathBuf,
};
const LIMIT: u64 = 1024 * 1024;
fn fail() -> Failure {
    Failure::new(1, "Media address runner-file publication failed.")
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| fail())
}
fn binding(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.nlink() == right.nlink()
}
fn stamped(left: &Metadata, right: &Metadata) -> bool {
    binding(left, right)
        && left.len() == right.len()
        && (
            left.mtime(),
            left.mtime_nsec(),
            left.ctime(),
            left.ctime_nsec(),
        ) == (
            right.mtime(),
            right.mtime_nsec(),
            right.ctime(),
            right.ctime_nsec(),
        )
}
fn bytes(file: &mut File) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0)).map_err(|_| fail())?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail())?;
    if bytes.len() as u64 > LIMIT {
        return Err(fail());
    }
    Ok(bytes)
}

pub(super) struct Target {
    path: PathBuf,
    file: File,
    stamp: Metadata,
    original: Vec<u8>,
}
impl Target {
    pub fn open(environment: &Environment) -> Result<Self> {
        let path = environment
            .get(std::ffi::OsStr::new("GITHUB_ENV"))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.as_os_str().as_encoded_bytes().len() <= 4096)
            .ok_or_else(fail)?;
        let mut file = File::from(
            rustix::fs::open(
                &path,
                OFlags::RDWR
                    | OFlags::APPEND
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| fail())?,
        );
        let admission = (|| {
            let stamp = file.metadata().map_err(|_| fail())?;
            if !stamp.is_file()
                || stamp.nlink() != 1
                || stamp.len() > LIMIT
                || stamp.uid() != rustix::process::getuid().as_raw()
                || stamp.mode() & 0o022 != 0
            {
                return Err(fail());
            }
            let original = bytes(&mut file)?;
            if !original.is_empty() && !original.ends_with(b"\n")
                || original.contains(&0)
                || original.contains(&b'\r')
                || std::str::from_utf8(&original).is_err()
                || !stamped(&stamp, &file.metadata().map_err(|_| fail())?)
            {
                return Err(fail());
            }
            Ok((stamp, original))
        })();
        let (stamp, original) = match admission {
            Ok(value) => value,
            Err(error) => {
                let _ = close(file);
                return Err(error);
            }
        };
        let value = Self {
            path,
            file,
            stamp,
            original,
        };
        if let Err(error) = value.check() {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&self) -> Result<()> {
        let visible = fs::symlink_metadata(&self.path).map_err(|_| fail())?;
        if !stamped(&self.stamp, &visible)
            || !stamped(&self.stamp, &self.file.metadata().map_err(|_| fail())?)
        {
            return Err(fail());
        }
        Ok(())
    }
    pub fn append(&mut self, address: Ipv4Addr) -> Result<()> {
        self.append_with(address, || {})
    }
    fn append_with(&mut self, address: Ipv4Addr, before_write: impl FnOnce()) -> Result<()> {
        self.check()?;
        let record = format!("TEST_ANNOUNCE_IP={address}\n");
        if self.stamp.len() + record.len() as u64 > LIMIT {
            return Err(fail());
        }
        if bytes(&mut self.file)? != self.original {
            return Err(fail());
        }
        self.check()?;
        before_write();
        self.file.write_all(record.as_bytes()).map_err(|_| fail())?;
        self.file.sync_all().map_err(|_| fail())?;
        let after = self.file.metadata().map_err(|_| fail())?;
        let mut expected = self.original.clone();
        expected.extend_from_slice(record.as_bytes());
        if !binding(&self.stamp, &after)
            || after.len() != expected.len() as u64
            || bytes(&mut self.file)? != expected
            || !stamped(&after, &self.file.metadata().map_err(|_| fail())?)
            || !stamped(
                &after,
                &fs::symlink_metadata(&self.path).map_err(|_| fail())?,
            )
        {
            return Err(fail());
        }
        self.original = expected;
        self.stamp = after;
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        self.finish_with(close)
    }
    fn finish_with(self, close: impl FnOnce(File) -> Result<()>) -> Result<()> {
        let checked = self.check();
        let closed = close(self.file);
        checked?;
        closed?;
        if !stamped(
            &self.stamp,
            &fs::symlink_metadata(&self.path).map_err(|_| fail())?,
        ) {
            return Err(fail());
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "media_ip_files_tests.rs"]
mod tests;
