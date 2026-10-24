//! Bounded gzip and mature raw/normal tar passes before any extracted result is
//! admitted. Only fresh private regular files are written; archive metadata
//! never chooses permissions, owners, links or an absolute filesystem root.

use super::super::namespace::{self, Budget, Chain, ChainWitness, Held};
use super::{
    Result, failure,
    files::{Input, Original, Output},
    io, projection,
    worker::Checkpoint,
};
use flate2::bufread::GzDecoder;
use rustix::fs::Mode;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{File, Metadata},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

const EXPANDED: u64 = 1024 * 1024 * 1024;
const MEMBER: u64 = 144 * 1024 * 1024;
const FILES: usize = 10000;

struct Directory {
    witness: ChainWitness,
    stamp: Metadata,
    names: BTreeSet<OsString>,
}
pub(super) struct Extraction {
    files: Vec<Original>,
    directories: Vec<Directory>,
}
impl Extraction {
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        for directory in &self.directories {
            let held = directory.witness.reopen(budget)?;
            let result = (|| {
                if !namespace::same(&directory.stamp, &io(held.file().metadata())?) {
                    return Err(failure(1));
                }
                inventory(held.file(), &directory.names, budget, checkpoint)?;
                if !namespace::same(&directory.stamp, &io(held.file().metadata())?) {
                    return Err(failure(1));
                }
                Ok(())
            })();
            result.and(held.finish())?;
        }
        for file in &self.files {
            file.check(budget, checkpoint)?;
        }
        checkpoint.check()
    }
}
fn inventory(
    file: &File,
    expected: &BTreeSet<OsString>,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut actual = BTreeSet::new();
    projection::entries(file, budget, checkpoint, |name| {
        if matches!(name.to_bytes(), b"." | b"..") {
            return Ok(());
        }
        let name = std::ffi::OsStr::from_bytes(name.to_bytes());
        if actual.len() >= expected.len()
            || !expected.contains(name)
            || !actual.insert(name.to_owned())
        {
            return Err(failure(1));
        }
        Ok(())
    })?;
    if &actual == expected {
        Ok(())
    } else {
        Err(failure(1))
    }
}
fn extent(position: u64, size: u64, total: u64) -> Result<u64> {
    size.checked_add(511)
        .and_then(|n| (n / 512).checked_mul(512))
        .and_then(|n| position.checked_add(n))
        .filter(|n| *n <= total)
        .ok_or_else(|| failure(1))
}
fn preflight(file: &File, total: u64, checkpoint: &Checkpoint) -> Result<()> {
    let mut cursor = file;
    io(cursor.seek(SeekFrom::Start(0)))?;
    let mut bundle = tar::Archive::new(&mut cursor);
    let entries = io(bundle.entries_with_seek())?.raw(true);
    let mut last = 0u64;
    for (index, entry) in entries.enumerate() {
        checkpoint.check()?;
        if index >= FILES * 2 {
            return Err(failure(1));
        }
        let mut entry = io(entry)?;
        // The reference's physical TarInfo.frombuf pass decodes these header
        // strings with strict UTF-8. Binary Unix paths use PAX metadata instead.
        for range in [0..100, 157..257, 265..297, 297..329, 345..500] {
            let field = &entry.header().as_bytes()[range];
            let end = field
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(field.len());
            std::str::from_utf8(&field[..end]).map_err(|_| failure(1))?;
        }
        last = extent(entry.raw_file_position(), entry.size(), total)?;
        let kind = entry.header().as_bytes()[156];
        if !matches!(kind, 0 | b'0' | b'5' | b'x') {
            return Err(failure(1));
        }
        if kind == b'x' {
            if entry.size() > 8192 {
                return Err(failure(1));
            }
            let mut metadata = Vec::new();
            io(entry.read_to_end(&mut metadata))?;
            for field in tar::PaxExtensions::new(&metadata) {
                let field = io(field)?;
                if field
                    .key()
                    .map_err(|_| failure(1))?
                    .starts_with("GNU.sparse")
                {
                    return Err(failure(1));
                }
            }
        } else if entry.size() > MEMBER {
            return Err(failure(1));
        }
    }
    if total.saturating_sub(last) < 1024 {
        return Err(failure(1));
    }
    io(cursor.seek(SeekFrom::Start(last)))?;
    let mut block = [0u8; 65536];
    loop {
        checkpoint.check()?;
        let count = io(cursor.read(&mut block))?;
        if count == 0 {
            break;
        }
        if block[..count].iter().any(|b| *b != 0) {
            return Err(failure(1));
        }
    }
    io(cursor.seek(SeekFrom::Start(0)))?;
    Ok(())
}
fn gzip(source: &Input, writer: &File, checkpoint: &Checkpoint) -> Result<u64> {
    source.check(checkpoint)?;
    let mut reader = source.file()?;
    io(reader.seek(SeekFrom::Start(0)))?;
    let mut header = [0u8; 10];
    io(reader.read_exact(&mut header))?;
    if header[..3] != [0x1f, 0x8b, 0x08] || !matches!(header[3], 0 | 8) {
        return Err(failure(1));
    }
    if header[3] == 8 {
        let mut ended = false;
        for _ in 0..256 {
            let mut byte = [0];
            io(reader.read_exact(&mut byte))?;
            if byte[0] == 0 {
                ended = true;
                break;
            }
        }
        if !ended {
            return Err(failure(1));
        }
    }
    io(reader.seek(SeekFrom::Start(0)))?;
    let mut decoder = GzDecoder::new(BufReader::new(reader));
    let mut writer = writer;
    let mut total = 0u64;
    let mut block = [0u8; 65536];
    loop {
        checkpoint.check()?;
        let count = io(decoder.read(&mut block))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= EXPANDED)
            .ok_or_else(|| failure(1))?;
        io(writer.write_all(&block[..count]))?;
    }
    let mut remaining = decoder.into_inner();
    let mut extra = [0];
    if io(remaining.read(&mut extra))? != 0 {
        return Err(failure(1));
    }
    io(writer.sync_all())?;
    source.check(checkpoint)?;
    Ok(total)
}
fn directory_path(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.into()
    } else {
        root.join(relative)
    }
}
fn ensure_directory(
    root: &Path,
    name: &Path,
    directories: &mut BTreeMap<PathBuf, BTreeSet<OsString>>,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<()> {
    let mut missing = Vec::new();
    let mut selected = name;
    while !directories.contains_key(selected) {
        checkpoint.check()?;
        missing.push(selected.to_owned());
        selected = selected.parent().ok_or_else(|| failure(1))?;
    }
    while let Some(name) = missing.pop() {
        checkpoint.check()?;
        let parent = name.parent().ok_or_else(|| failure(1))?;
        let held = Chain::open(&directory_path(root, parent), budget)?;
        let leaf = name.file_name().ok_or_else(|| failure(1))?;
        let created = rustix::fs::mkdirat(held.file(), leaf, Mode::from_bits_truncate(0o700))
            .map_err(|_| failure(1));
        let closed = held.finish();
        created.and(closed)?;
        directories
            .get_mut(parent)
            .ok_or_else(|| failure(1))?
            .insert(leaf.into());
        directories.insert(name, BTreeSet::new());
    }
    Ok(())
}
fn parse(
    file: &File,
    total: u64,
    root: &Path,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<Extraction> {
    let mut cursor = file;
    io(cursor.seek(SeekFrom::Start(0)))?;
    let mut bundle = tar::Archive::new(&mut cursor);
    let mut seen = BTreeSet::new();
    let mut bytes = 0u64;
    let mut directories = BTreeMap::from([(PathBuf::new(), BTreeSet::new())]);
    let mut originals = Vec::new();
    for entry in io(bundle.entries_with_seek())? {
        checkpoint.check()?;
        let mut entry = io(entry)?;
        if !entry.header().entry_type().is_file() || entry.size() > MEMBER || seen.len() >= FILES {
            return Err(failure(1));
        }
        extent(entry.raw_file_position(), entry.size(), total)?;
        let raw = entry.path_bytes().into_owned();
        let path = Path::new(std::ffi::OsStr::from_bytes(&raw));
        if raw.contains(&b'\\')
            || path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
            || path.as_os_str().is_empty()
            || !seen.insert(raw.clone())
        {
            return Err(failure(1));
        }
        // PurePosixPath in the reference removes redundant separators and dot
        // components; preserve that behavior without interpreting parent parts.
        let path = path
            .components()
            .filter(|part| !matches!(part, Component::CurDir))
            .collect::<PathBuf>();
        if path.as_os_str().is_empty() {
            return Err(failure(1));
        }
        bytes = bytes
            .checked_add(entry.size())
            .filter(|n| *n <= EXPANDED)
            .ok_or_else(|| failure(1))?;
        let parent = path.parent().ok_or_else(|| failure(1))?;
        ensure_directory(root, parent, &mut directories, budget, checkpoint)?;
        let held = Chain::open(&directory_path(root, parent), budget)?;
        let result = (|| {
            let name = path.file_name().ok_or_else(|| failure(1))?;
            let writer = Held::create_name(held.file(), name, false, budget)?;
            let result = (|| {
                let before = namespace::private_file(&writer, false)?;
                let mut hash = Sha256::new();
                let mut remaining = entry.size();
                let mut block = [0u8; 65536];
                while remaining != 0 {
                    checkpoint.check()?;
                    let count = io(entry.read(&mut block[..remaining.min(65536) as usize]))?;
                    if count == 0 {
                        return Err(failure(1));
                    }
                    io(writer.file().write_all(&block[..count]))?;
                    hash.update(&block[..count]);
                    remaining -= count as u64;
                }
                let mut extra = [0];
                if io(entry.read(&mut extra))? != 0 {
                    return Err(failure(1));
                }
                io(writer.file().sync_all())?;
                let after = namespace::private_file(&writer, false)?;
                if !namespace::binding(&before, &after) || after.len() != entry.size() {
                    return Err(failure(1));
                }
                let original = Original::written(
                    held.witness()?,
                    name.into(),
                    after,
                    super::super::hex(&hash.finalize()),
                );
                original.check(budget, checkpoint)?;
                Ok(original)
            })();
            let closed = writer.finish();
            result.and_then(|value| closed.map(|()| value))
        })();
        let closed = held.finish();
        let original = result.and_then(|value| closed.map(|()| value))?;
        directories
            .get_mut(parent)
            .unwrap()
            .insert(path.file_name().unwrap().into());
        originals.push(original);
    }
    let mut sealed = Vec::new();
    for (path, names) in directories {
        let held = Chain::open(&directory_path(root, &path), budget)?;
        let result = (|| {
            let stamp = io(held.file().metadata())?;
            inventory(held.file(), &names, budget, checkpoint)?;
            if !namespace::same(&stamp, &io(held.file().metadata())?) {
                return Err(failure(1));
            }
            Ok(Directory {
                witness: held.witness()?,
                stamp,
                names,
            })
        })();
        let closed = held.finish();
        sealed.push(result.and_then(|v| closed.map(|()| v))?);
    }
    let extracted = Extraction {
        files: originals,
        directories: sealed,
    };
    extracted.check(budget, checkpoint)?;
    Ok(extracted)
}
pub(super) fn extract(
    source: &Input,
    output: &Output,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<Extraction> {
    source.check(checkpoint)?;
    let root = output.witness().reopen(budget)?;
    let result = (|| {
        let expanded = Held::create(root.file(), "expanded.tar", false, budget)?;
        let result = (|| {
            let before = namespace::private_file(&expanded, false)?;
            let total = gzip(source, expanded.file(), checkpoint)?;
            let stamp = namespace::private_file(&expanded, false)?;
            if !namespace::binding(&before, &stamp) || stamp.len() != total {
                return Err(failure(1));
            }
            preflight(expanded.file(), total, checkpoint)?;
            rustix::fs::mkdirat(root.file(), "remote", Mode::from_bits_truncate(0o700))
                .map_err(|_| failure(1))?;
            let extracted = parse(
                expanded.file(),
                total,
                &output.path("remote"),
                budget,
                checkpoint,
            )?;
            if !namespace::same(&stamp, &io(expanded.file().metadata())?) {
                return Err(failure(1));
            }
            source.check(checkpoint)?;
            Ok(extracted)
        })();
        let closed = expanded.finish();
        result.and_then(|v| closed.map(|()| v))
    })();
    let closed = root.finish();
    result.and_then(|v| closed.map(|()| v))
}
