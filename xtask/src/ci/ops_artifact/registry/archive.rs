//! Lock-checksum-verified bytes precede mature gzip/tar interpretation.
//! Two tar passes bound extension allocation before effective-name processing.

use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};

pub(super) const COMPRESSED: u64 = 16 * 1024 * 1024;
const EXPANDED: usize = 128 * 1024 * 1024;
pub(super) const FILE: u64 = 16 * 1024 * 1024;
const MEMBERS: usize = 8192;
pub(super) struct Member {
    pub offset: usize,
    pub size: usize,
    pub directory: bool,
    pub executable: bool,
}
pub(super) struct Archive {
    pub bytes: Vec<u8>,
    pub members: BTreeMap<String, Member>,
}
pub(super) async fn inflate<E: Executor, C: Clock>(
    bytes: &[u8],
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<Vec<u8>> {
    inflate_limited(bytes, executor, deadline, EXPANDED).await
}
pub(super) async fn inflate_limited<E: Executor, C: Clock>(
    bytes: &[u8],
    executor: &mut E,
    deadline: &Deadline<'_, C>,
    maximum: usize,
) -> Result<Vec<u8>> {
    let mut gzip = flate2::bufread::GzDecoder::new(Cursor::new(bytes));
    let mut result = Vec::new();
    let mut buffer = vec![0u8; 65536];
    loop {
        executor.ready().await?;
        deadline.check(false)?;
        let n = io(gzip.read(&mut buffer))?;
        if n == 0 {
            break;
        }
        if result.len().checked_add(n).is_none_or(|v| v > maximum) {
            return Err(failure(1));
        }
        result.try_reserve(n).map_err(|_| failure(1))?;
        result.extend_from_slice(&buffer[..n]);
    }
    // Single gzip member, valid CRC/ISIZE and no concatenated/trailing bytes.
    if gzip.into_inner().position() != bytes.len() as u64 {
        return Err(failure(1));
    }
    Ok(result)
}
fn end(offset: u64, size: u64, total: usize) -> Result<u64> {
    let padded = size.checked_add(511).ok_or_else(|| failure(1))? / 512 * 512;
    offset
        .checked_add(padded)
        .filter(|v| *v <= total as u64)
        .ok_or_else(|| failure(1))
}
fn preflight(bytes: &[u8]) -> Result<()> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries_with_seek()
        .map_err(|_| failure(1))?
        .raw(true);
    let mut position = 0u64;
    let mut pending = false;
    let mut extensions = 0u64;
    for (count, entry) in entries.enumerate() {
        if count >= MEMBERS * 2 {
            return Err(failure(1));
        }
        let mut entry = entry.map_err(|_| failure(1))?;
        if entry.raw_header_position() != position {
            return Err(failure(1));
        }
        position = end(entry.raw_file_position(), entry.size(), bytes.len())?;
        let kind = entry.header().entry_type();
        if kind.is_gnu_longname() {
            if entry.header().as_gnu().is_none() && entry.header().as_ustar().is_none() {
                return Err(failure(1));
            }
            extensions = extensions
                .checked_add(entry.size())
                .ok_or_else(|| failure(1))?;
            if pending || entry.size() > 8192 || extensions > 1024 * 1024 {
                return Err(failure(1));
            }
            let mut body = Vec::new();
            io(entry.read_to_end(&mut body))?;
            let name = body.strip_suffix(&[0]).unwrap_or(&body);
            if name.contains(&0) || std::str::from_utf8(name).is_err() {
                return Err(failure(1));
            }
            pending = true;
        } else {
            if !(kind.is_file() || kind.is_dir()) || (kind.is_dir() && entry.size() != 0) {
                return Err(failure(1));
            }
            pending = false;
        }
    }
    let position = usize::try_from(position).map_err(|_| failure(1))?;
    // Two zero blocks terminate the tar stream; arbitrary zero block padding
    // remains allowed. Nonzero appended archives or hidden trailing data refuse.
    if pending
        || bytes.len().saturating_sub(position) < 1024
        || bytes[position..].iter().any(|b| *b != 0)
    {
        return Err(failure(1));
    }
    Ok(())
}
fn path(value: &str) -> Result<&str> {
    let normalized = value.strip_suffix('/').unwrap_or(value);
    if normalized.is_empty()
        || normalized.len() > 1024
        || normalized.split('/').count() > 16
        || normalized.contains('\\')
        || normalized.chars().any(|c| c.is_control())
        || normalized
            .split('/')
            .any(|c| c.is_empty() || matches!(c, "." | "..") || c.len() > 255)
    {
        return Err(failure(1));
    }
    Ok(normalized)
}
impl Archive {
    pub fn parse(bytes: Vec<u8>, selected: &lock::Crate) -> Result<Self> {
        if bytes.len() > EXPANDED {
            return Err(failure(1));
        }
        preflight(&bytes)?;
        let mut archive = tar::Archive::new(Cursor::new(&bytes));
        let mut members: BTreeMap<String, Member> = BTreeMap::new();
        let mut names = 0usize;
        for entry in archive.entries_with_seek().map_err(|_| failure(1))? {
            let entry = entry.map_err(|_| failure(1))?;
            if members.len() >= MEMBERS {
                return Err(failure(1));
            }
            let original = entry.path_bytes();
            let kind = entry.header().entry_type();
            if !(kind.is_file() || kind.is_dir()) {
                return Err(failure(1));
            }
            let directory = kind.is_dir();
            if !directory && original.ends_with(b"/") {
                return Err(failure(1));
            }
            let name = path(std::str::from_utf8(&original).map_err(|_| failure(1))?)?;
            let relative = name.strip_prefix(&format!("{}/", selected.directory));
            if relative.is_none() && !(directory && name == selected.directory) {
                return Err(failure(1));
            }
            if relative.is_some_and(|p| p.split('/').next() == Some(".cargo-checksum.json"))
                || members.contains_key(name)
                || entry.size() > FILE
                || entry.header().mode().map_err(|_| failure(1))? & 0o7000 != 0
            {
                return Err(failure(1));
            }
            names = names.checked_add(name.len()).ok_or_else(|| failure(1))?;
            if names > 8 * 1024 * 1024 {
                return Err(failure(1));
            }
            end(entry.raw_file_position(), entry.size(), bytes.len())?;
            members.insert(
                name.into(),
                Member {
                    offset: entry.raw_file_position() as usize,
                    size: entry.size() as usize,
                    directory,
                    executable: !directory
                        && entry.header().mode().map_err(|_| failure(1))? & 0o111 != 0,
                },
            );
        }
        // Prefix conflicts and all implied parents are resolved before output.
        let mut parents = BTreeSet::from([selected.directory.clone()]);
        for name in members.keys() {
            let mut parent = name.as_str();
            while let Some((next, _)) = parent.rsplit_once('/') {
                if members.get(next).is_some_and(|m| !m.directory) {
                    return Err(failure(1));
                }
                parents.insert(next.into());
                if parents.len() > MEMBERS {
                    return Err(failure(1));
                }
                parent = next;
            }
        }
        for name in parents {
            members.entry(name).or_insert(Member {
                offset: 0,
                size: 0,
                directory: true,
                executable: false,
            });
            if members.len() > MEMBERS {
                return Err(failure(1));
            }
        }
        let manifest = members
            .get(&format!("{}/Cargo.toml", selected.directory))
            .filter(|m| !m.directory)
            .ok_or_else(|| failure(1))?;
        lock::manifest(
            &bytes[manifest.offset..manifest.offset + manifest.size],
            selected,
        )?;
        Ok(Self { bytes, members })
    }
}
