//! Original CA descriptor and namespace retained through all requests.

use super::{Result, fail, files};
use crate::ci::release_controller::files::Directory;
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
};

pub(super) struct Ca {
    directory: Directory,
    name: String,
    file: File,
    stamp: Metadata,
    sha256: String,
}
impl Ca {
    pub fn open(config: &super::config::Config) -> Result<(Self, Vec<u8>)> {
        let parent = config
            .ca
            .parent()
            .ok_or_else(|| fail(1, "fixture_ca_file"))?;
        let name = config
            .ca
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| fail(1, "fixture_ca_file"))?
            .to_owned();
        let directory = Directory::open(parent).map_err(|_| fail(1, "fixture_ca_file"))?;
        let admitted = (|| {
            let visible =
                rustix::fs::statat(directory.file(), name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| fail(1, "fixture_ca_file"))?;
            if rustix::fs::FileType::from_raw_mode(visible.st_mode)
                != rustix::fs::FileType::RegularFile
            {
                return Err(fail(1, "fixture_ca_file"));
            }
            rustix::fs::openat(
                directory.file(),
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| fail(1, "fixture_ca_file"))
        })();
        let file = match admitted {
            Ok(file) => file,
            Err(e) => {
                let _ = directory.finish();
                return Err(e);
            }
        };
        let admitted = file.metadata().map_err(|_| fail(1, "fixture_ca_file"));
        let stamp = match admitted {
            Ok(m) if m.is_file() && m.len() > 0 && m.len() <= 65536 => m,
            _ => {
                let _ = files::close(file);
                let _ = directory.finish();
                return Err(fail(1, "fixture_ca_file"));
            }
        };
        let mut ca = Self {
            directory,
            name,
            file,
            stamp,
            sha256: config.sha256.clone(),
        };
        match ca.bytes() {
            Ok(bytes) if bytes.is_ascii() => Ok((ca, bytes)),
            value => {
                let error = value.err().unwrap_or_else(|| fail(1, "http_policy_failed"));
                let _ = ca.finish();
                Err(error)
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        self.directory
            .verify()
            .map_err(|_| fail(1, "fixture_ca_file"))?;
        let visible = rustix::fs::statat(
            self.directory.file(),
            self.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| fail(1, "fixture_ca_file"))?;
        if files::number(visible.st_dev)? != self.stamp.dev()
            || visible.st_ino != self.stamp.ino()
            || !files::same(
                &self
                    .file
                    .metadata()
                    .map_err(|_| fail(1, "fixture_ca_file"))?,
                &self.stamp,
            )
        {
            return Err(fail(1, "fixture_ca_file"));
        }
        Ok(())
    }
    pub fn bytes(&mut self) -> Result<Vec<u8>> {
        self.check()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| fail(1, "fixture_ca_file"))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.file)
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| fail(1, "fixture_ca_file"))?;
        self.check()?;
        if bytes.len() as u64 != self.stamp.len()
            || crate::ci::io::hex(&Sha256::digest(&bytes)) != self.sha256
        {
            return Err(fail(1, "fixture_ca_digest"));
        }
        Ok(bytes)
    }
    pub fn finish(mut self) -> Result<()> {
        let checked = self.bytes().map(|_| ());
        let file = files::close(self.file);
        let directory = self
            .directory
            .finish()
            .map_err(|_| fail(1, "fixture_ca_file"));
        checked.and(file).and(directory)
    }
}
