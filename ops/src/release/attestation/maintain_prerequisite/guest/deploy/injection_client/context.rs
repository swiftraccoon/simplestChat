//! Retained fixed originals and the parent's absolute monotonic deadline.
use super::*;
use crate::owned_process::CompletionSignals;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata},
    os::unix::fs::{FileExt, MetadataExt},
};

pub(super) fn stamp(value: &Metadata) -> Result<Value> {
    let nanos = |seconds: i64, fraction: i64| {
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(fraction))
            .ok_or(Error::Bounds)
    };
    Ok(json!([
        value.dev(),
        value.ino(),
        value.mode(),
        value.uid(),
        value.gid(),
        value.nlink(),
        value.len(),
        nanos(value.mtime(), value.mtime_nsec())?,
        nanos(value.ctime(), value.ctime_nsec())?
    ]))
}
fn valid_stamp(value: &Value) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() == 9
            && items
                .iter()
                .all(|item| item.as_i64().is_some() || item.as_u64().is_some())
    })
}
pub(super) fn monotonic() -> Result<f64> {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    require(now.tv_sec >= 0 && (0..1_000_000_000).contains(&now.tv_nsec))?;
    Ok(now.tv_sec as f64 + now.tv_nsec as f64 / 1_000_000_000.0)
}
pub(super) struct Fence {
    end: Option<f64>,
    signals: CompletionSignals,
    finished: bool,
}
impl Fence {
    fn new() -> Result<Self> {
        Ok(Self {
            end: None,
            signals: CompletionSignals::new().map_err(|_| Error::Io)?,
            finished: false,
        })
    }
    pub fn check(&self) -> Result<()> {
        require(self.signals.status().is_none())?;
        if let Some(end) = self.end {
            require(end.is_finite() && monotonic()? < end)?;
        }
        Ok(())
    }
    pub fn remaining(&self) -> Result<std::time::Duration> {
        self.check()?;
        std::time::Duration::try_from_secs_f64(self.end.ok_or(Error::Refused)? - monotonic()?)
            .map_err(|_| Error::Deadline)
    }
    pub(super) fn finish(&mut self) -> Result<()> {
        if !self.finished {
            self.finished = true;
            require(self.signals.finish().map_err(|_| Error::Close)?.is_none())?;
        }
        self.check()
    }
}
pub(super) struct Config {
    pub nonce: String,
    pub image: String,
    pub namespace: u64,
    pub docker_sha: String,
    pub docker_stamp: Value,
    pub native_sha: String,
    pub native_stamp: Value,
    pub end: f64,
}
impl Config {
    pub fn decode(raw: &[u8]) -> Result<Self> {
        require(raw.len() <= FRAME)?;
        let value = decode(raw)?;
        exact(
            &value,
            &[
                "schemaVersion",
                "nonce",
                "bodyEnd",
                "image",
                "realCliSha256",
                "realCliStamp",
                "nativeSha256",
                "nativeStamp",
                "namespaceInode",
            ],
        )?;
        require(value["schemaVersion"].as_u64() == Some(1))?;
        let nonce = value["nonce"]
            .as_str()
            .filter(|value| hex(value, 32))
            .ok_or(Error::Refused)?;
        let image = value["image"].as_str().ok_or(Error::Refused)?;
        require(
            image
                .strip_prefix("sha256:")
                .is_some_and(|value| hex(value, 64)),
        )?;
        let namespace = value["namespaceInode"]
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or(Error::Refused)?;
        let end = value["bodyEnd"]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or(Error::Refused)?;
        require(valid_stamp(&value["realCliStamp"]) && valid_stamp(&value["nativeStamp"]))?;
        Ok(Self {
            nonce: nonce.into(),
            image: image.into(),
            namespace,
            end,
            docker_sha: digest(&value["realCliSha256"])?.into(),
            native_sha: digest(&value["nativeSha256"])?.into(),
            docker_stamp: value["realCliStamp"].clone(),
            native_stamp: value["nativeStamp"].clone(),
        })
    }
}
pub(super) struct Original {
    pub path: std::path::PathBuf,
    pub file: Option<File>,
    pub before: Metadata,
}
impl Original {
    fn open(path: &str, maximum: u64, mode: u32, fence: &Fence) -> Result<Self> {
        Self::open_for(path, maximum, mode, (0, 0), fence)
    }
    fn open_for(
        path: &str,
        maximum: u64,
        mode: u32,
        owner: (u32, u32),
        fence: &Fence,
    ) -> Result<Self> {
        fence.check()?;
        let file = File::from(
            rustix::fs::open(
                path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| Error::Io)?,
        );
        let admitted = (|| {
            let before = file.metadata().map_err(|_| Error::Io)?;
            require(
                before.is_file()
                    && before.uid() == owner.0
                    && before.gid() == owner.1
                    && before.mode() & 0o7777 == mode
                    && before.nlink() == 1
                    && before.len() <= maximum
                    && (maximum == 0 || before.len() > 0)
                    && fs::canonicalize(path).map_err(|_| Error::Io)? == Path::new(path),
            )?;
            require(
                stamp(&before)? == stamp(&fs::symlink_metadata(path).map_err(|_| Error::Io)?)?,
            )?;
            fence.check()?;
            Ok(before)
        })();
        match admitted {
            Ok(before) => Ok(Self {
                path: path.into(),
                file: Some(file),
                before,
            }),
            Err(first) => {
                let _ = nix::unistd::close(file);
                Err(first)
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        let file = self.file.as_ref().ok_or(Error::Close)?;
        require(
            stamp(&file.metadata().map_err(|_| Error::Io)?)? == stamp(&self.before)?
                && stamp(&fs::symlink_metadata(&self.path).map_err(|_| Error::Changed)?)?
                    == stamp(&self.before)?,
        )
    }
    pub fn data(&self, fence: &Fence) -> Result<Vec<u8>> {
        fence.check()?;
        require(self.before.len() <= FRAME as u64)?;
        let mut value = vec![0; self.before.len() as usize];
        let mut at = 0;
        while at < value.len() {
            fence.check()?;
            let count = self
                .file
                .as_ref()
                .ok_or(Error::Close)?
                .read_at(&mut value[at..], at as u64)
                .map_err(|_| Error::Io)?;
            fence.check()?;
            require(count > 0)?;
            at += count;
        }
        self.check()?;
        fence.check()?;
        Ok(value)
    }
    pub fn hash(&self, fence: &Fence) -> Result<String> {
        let mut hash = Sha256::new();
        let mut at = 0u64;
        let mut data = [0; CHUNK];
        loop {
            fence.check()?;
            let count = self
                .file
                .as_ref()
                .ok_or(Error::Close)?
                .read_at(&mut data, at)
                .map_err(|_| Error::Io)?;
            fence.check()?;
            if count == 0 {
                break;
            }
            at = at.checked_add(count as u64).ok_or(Error::Bounds)?;
            require(at <= self.before.len())?;
            hash.update(&data[..count]);
        }
        require(at == self.before.len())?;
        self.check()?;
        Ok(super::super::super::files::hex(&hash.finalize()))
    }
    pub fn close(&mut self) -> Result<()> {
        self.file
            .take()
            .map(nix::unistd::close)
            .transpose()
            .map(|_| ())
            .map_err(|_| Error::Close)
    }
}
pub(super) struct Context {
    fence: Option<Fence>,
    pub config: Option<Config>,
    raw: Vec<u8>,
    originals: Vec<Original>,
    directories: Vec<(String, File, Metadata)>,
    config_index: Option<usize>,
}
impl Context {
    pub fn new() -> Self {
        Self {
            fence: Fence::new().ok(),
            config: None,
            raw: Vec::new(),
            originals: Vec::new(),
            directories: Vec::new(),
            config_index: None,
        }
    }
    pub fn fence(&self) -> Result<&Fence> {
        self.fence.as_ref().ok_or(Error::Io)
    }
    pub fn config(&self) -> Result<&Config> {
        self.config.as_ref().ok_or(Error::Refused)
    }
    pub fn original(&self, index: usize) -> Result<&Original> {
        self.originals.get(index).ok_or(Error::Refused)
    }
    pub fn open(&mut self, path: &str, maximum: u64, mode: u32) -> Result<usize> {
        let original = Original::open(path, maximum, mode, self.fence()?)?;
        let index = self.originals.len();
        self.originals.push(original);
        Ok(index)
    }
    pub fn admit(&mut self) -> Result<()> {
        self.fence()?.check()?;
        require(rustix::process::geteuid().as_raw() == 0)?;
        for (path, mode) in [
            (ROOT, 0o700),
            ("/usr/local/libexec/simplestchat-public", 0o755),
        ] {
            let file = File::from(
                rustix::fs::open(
                    path,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|_| Error::Io)?,
            );
            let before = file.metadata().map_err(|_| Error::Io);
            match before {
                Ok(before) => {
                    self.directories.push((path.into(), file, before));
                    let (_, _, before) = self.directories.last().ok_or(Error::Io)?;
                    require(
                        before.uid() == 0
                            && before.gid() == 0
                            && before.mode() & 0o7777 == mode
                            && fs::canonicalize(path).map_err(|_| Error::Io)? == Path::new(path),
                    )?;
                }
                Err(first) => {
                    let _ = nix::unistd::close(file);
                    return Err(first);
                }
            }
        }
        let config = self.open(CONFIG, FRAME as u64, 0o600)?;
        self.config_index = Some(config);
        self.raw = self.original(config)?.data(self.fence()?)?;
        let decoded = Config::decode(&self.raw)?;
        self.fence.as_mut().ok_or(Error::Io)?.end = Some(decoded.end);
        self.config = Some(decoded);
        self.fence()?.check()?;
        let marker = self.open(MARKER, 33, 0o600)?;
        require(
            self.original(marker)?.data(self.fence()?)?
                == format!("{}\n", self.config()?.nonce).as_bytes(),
        )?;
        super::namespace::private(self)?;
        let native = self.open(INSTALLED, 256 * 1024 * 1024, 0o755)?;
        require(
            stamp(&self.original(native)?.before)? == self.config()?.native_stamp
                && self.original(native)?.hash(self.fence()?)? == self.config()?.native_sha
                && stamp(&fs::metadata("/proc/self/exe").map_err(|_| Error::Io)?)?
                    == self.config()?.native_stamp,
        )?;
        self.check()
    }
    pub fn check(&self) -> Result<()> {
        self.fence()?.check()?;
        for (path, file, before) in &self.directories {
            let current = fs::symlink_metadata(path).map_err(|_| Error::Changed)?;
            let held = file.metadata().map_err(|_| Error::Io)?;
            require(
                current.is_dir()
                    && held.is_dir()
                    && current.dev() == before.dev()
                    && current.ino() == before.ino()
                    && held.dev() == before.dev()
                    && held.ino() == before.ino()
                    && current.uid() == 0
                    && current.gid() == 0
                    && current.mode() & 0o7777 == before.mode() & 0o7777,
            )?;
        }
        for original in &self.originals {
            if original.file.is_some() {
                original.check()?;
            }
        }
        let config = self.original(self.config_index.ok_or(Error::Refused)?)?;
        require(config.data(self.fence()?)? == self.raw)?;
        require(
            fs::metadata("/proc/self/ns/mnt")
                .map_err(|_| Error::Io)?
                .ino()
                != self.config()?.namespace,
        )?;
        self.fence()?.check()
    }
    pub fn real(&mut self) -> Result<usize> {
        let index = self.open(ALIAS, 320 * 1024 * 1024, 0o755)?;
        require(
            stamp(&self.original(index)?.before)? == self.config()?.docker_stamp
                && self.original(index)?.hash(self.fence()?)? == self.config()?.docker_sha,
        )?;
        Ok(index)
    }
    pub fn close_original(&mut self, index: usize) -> Result<()> {
        self.originals.get_mut(index).ok_or(Error::Refused)?.close()
    }
    pub fn take(&mut self, index: usize) -> Result<File> {
        self.original(index)?.check()?;
        self.originals
            .get_mut(index)
            .ok_or(Error::Refused)?
            .file
            .take()
            .ok_or(Error::Close)
    }
    pub fn close(&mut self) -> Result<()> {
        let mut first = None;
        for original in self.originals.iter_mut().rev() {
            if let Err(error) = original.close() {
                first.get_or_insert(error);
            }
        }
        for (_, file, _) in self.directories.drain(..).rev() {
            if nix::unistd::close(file).is_err() {
                first.get_or_insert(Error::Close);
            }
        }
        first.map_or(Ok(()), Err)
    }
    pub fn finish(&mut self) -> Result<()> {
        self.fence.as_mut().ok_or(Error::Io)?.finish()
    }
}

#[cfg(test)]
impl Fence {
    pub(super) fn fixture(seconds: f64) -> Result<Self> {
        let mut owner = Self::new()?;
        owner.end = Some(monotonic()? + seconds);
        Ok(owner)
    }
}
#[cfg(test)]
impl Original {
    pub(super) fn fixture(path: &str, maximum: u64, mode: u32, fence: &Fence) -> Result<Self> {
        Self::open_for(
            path,
            maximum,
            mode,
            (
                rustix::process::getuid().as_raw(),
                rustix::process::getgid().as_raw(),
            ),
            fence,
        )
    }
}
