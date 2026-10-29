//! Checked streaming copies of admitted payload originals into fixed release leaves.

use super::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
};

pub(super) fn copy(original: &Original, path: &Path, control: &mut Control) -> Result<Original> {
    original.check(control)?;
    let parent = path.parent().ok_or(Error::Input)?;
    let held = rustix::fs::open(
        parent,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| Error::Io)?;
    let mut source = None;
    let mut destination = None;
    let result = (|| {
        use std::os::unix::fs::MetadataExt;
        let metadata = held.metadata().map_err(|_| Error::Io)?;
        require(metadata.uid() == 0 && metadata.gid() == 0 && metadata.mode() & 0o7777 == 0o700)?;
        source = Some(original.file()?.try_clone().map_err(|_| Error::Io)?);
        // Cloning shares the offset; all original readers use positional reads.
        use std::io::{Seek, SeekFrom};
        source
            .as_mut()
            .ok_or(Error::Close)?
            .seek(SeekFrom::Start(0))
            .map_err(|_| Error::Io)?;
        destination = Some(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(
                    (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32,
                )
                .open(path)
                .map_err(|_| Error::Io)?,
        );
        let mut buffer = [0u8; 65536];
        let mut total = 0u64;
        loop {
            control.check()?;
            let count = source
                .as_mut()
                .ok_or(Error::Close)?
                .read(&mut buffer)
                .map_err(|_| Error::Io)?;
            if count == 0 {
                break;
            }
            total = total.checked_add(count as u64).ok_or(Error::Bounds)?;
            require(total <= original.bytes)?;
            destination
                .as_mut()
                .ok_or(Error::Close)?
                .write_all(&buffer[..count])
                .map_err(|_| Error::Io)?;
        }
        require(total == original.bytes)?;
        destination
            .as_ref()
            .ok_or(Error::Close)?
            .sync_all()
            .map_err(|_| Error::Io)?;
        held.sync_all().map_err(|_| Error::Io)?;
        let binding =
            |m: &std::fs::Metadata| (m.dev(), m.ino(), m.mode(), m.uid(), m.gid(), m.nlink());
        require(binding(&metadata) == binding(&held.metadata().map_err(|_| Error::Io)?))?;
        require(
            binding(&metadata)
                == binding(&std::fs::symlink_metadata(parent).map_err(|_| Error::Changed)?),
        )?;
        original.check(control)
    })();
    if let Err(error) = result {
        control.record(error);
    }
    let mut close_error = None;
    for file in [destination, source, Some(held)].into_iter().flatten() {
        if let Err(error) = super::super::files::close(file, control) {
            close_error.get_or_insert(error);
        }
    }
    result.and(close_error.map_or(Ok(()), Err))?;
    let mut copied = Original::open(path, original.bytes, 0, 0o600, control)?;
    if copied.sha256 != original.sha256 || copied.bytes != original.bytes {
        control.record(Error::Changed);
        let _ = copied.finish(control);
        return Err(Error::Changed);
    }
    Ok(copied)
}
