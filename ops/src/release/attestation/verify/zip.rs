//! One cached ZIP/member comparison over original borrowed files; no extraction.

use super::{
    super::{
        control::{self, Cause, Owner, Result, Window},
        files::{Dir, Retained},
    },
    options::Metadata,
};
use rawzip::{CompressionMethod, Crc32, ZipArchive, extra_fields::ExtraFieldId};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::BTreeSet, io::Read, os::unix::fs::FileExt};

pub(in crate::release::attestation) const MAX_ZIP: u64 = 2 * 1024 * 1024 * 1024;
const MAX_METADATA: u64 = 64 * 1024 * 1024;
const MAX_TOTAL: u64 = MAX_ZIP + 128 * 1024 * 1024;
pub(in crate::release::attestation) const SELECTED: [&str; 10] = [
    "image.tar",
    "release.json",
    "outcome.json",
    "source.json",
    "sbom.spdx.json",
    "image-security.json",
    "runtime-proof.json",
    "vex.openvex.json",
    "release-predicate.json",
    "release-attestation.jsonl",
];
fn reject<T>(owner: &mut Owner) -> Result<T> {
    control::failed(owner, Cause::Zip)
}
fn require(owner: &mut Owner, value: bool) -> Result<()> {
    owner.check(Window::Body)?;
    if value { Ok(()) } else { reject(owner) }
}
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn read_at(file: &Retained, offset: u64, bytes: &mut [u8], owner: &mut Owner) -> Result<()> {
    file.check(owner, Window::Body)?;
    let result = file.file().read_exact_at(bytes, offset);
    if let Err(error) = result {
        let cause = if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Cause::Zip
        } else {
            Cause::Io
        };
        return control::failed(owner, cause);
    }
    file.check(owner, Window::Body)
}
struct End {
    offset: u64,
    directory: u64,
}
fn preflight(file: &Retained, owner: &mut Owner) -> Result<End> {
    require(owner, (22..=MAX_ZIP).contains(&file.len()))?;
    let count = file.len().min(65557);
    let mut tail = vec![0; count.try_into().map_err(|_| Cause::Bounds)?];
    read_at(file, file.len() - count, &mut tail, owner)?;
    let at = tail
        .windows(4)
        .rposition(|part| part == b"PK\x05\x06")
        .ok_or(Cause::Zip)?;
    require(owner, tail.len() - at >= 22)?;
    let header = &tail[at..];
    let entries = u16_at(header, 10);
    let size = u32_at(header, 12);
    let directory = u32_at(header, 16);
    let offset = file.len() - count + u64::try_from(at).map_err(|_| Cause::Bounds)?;
    require(
        owner,
        22 + usize::from(u16_at(header, 20)) == header.len()
            && u16_at(header, 4) == 0
            && u16_at(header, 6) == 0
            && u16_at(header, 8) == entries
            && (1..=256).contains(&entries)
            && (1..=1024 * 1024).contains(&size)
            && directory != u32::MAX
            && u64::from(directory) + u64::from(size) == offset,
    )?;
    Ok(End {
        offset,
        directory: u64::from(directory),
    })
}
struct Member {
    name: String,
    bytes: u64,
    compressed: u64,
    offset: u64,
    end: u64,
    method: CompressionMethod,
    crc: u32,
    flags: u16,
}
fn name(raw: &[u8], owner: &mut Owner) -> Result<String> {
    require(
        owner,
        !raw.is_empty() && raw.iter().all(|byte| (32..127).contains(byte)) && !raw.contains(&b'\\'),
    )?;
    let value = std::str::from_utf8(raw).map_err(|_| Cause::Zip)?;
    let canonical = value.trim_end_matches('/');
    require(
        owner,
        !canonical.is_empty()
            && !canonical
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            && (value == canonical || value.strip_suffix('/') == Some(canonical)),
    )?;
    Ok(canonical.into())
}
// Interpret the existing reference's extra-field policy while rawzip owns the
// central-record framing. In particular, it processes ZIP64 fields in order.
fn extra(
    header: &rawzip::ZipFileHeaderRecord<'_>,
    raw: &[u8],
    fixed: &[u8; 46],
    owner: &mut Owner,
) -> Result<(u64, u64, u64)> {
    let mut bytes = u64::from(u32_at(fixed, 24));
    let mut compressed = u64::from(u32_at(fixed, 20));
    let mut offset = u64::from(u32_at(fixed, 42));
    let mut fields = header.extra_fields();
    for (kind, mut value) in fields.by_ref() {
        owner.check(Window::Body)?;
        if kind == ExtraFieldId::ZIP64 {
            for (at, target) in [&mut bytes, &mut compressed, &mut offset]
                .into_iter()
                .enumerate()
            {
                if *target == u64::from(u32::MAX) || (at == 0 && *target == u64::MAX) {
                    let part = value.get(..8).ok_or(Cause::Zip)?;
                    *target = u64::from_le_bytes(part.try_into().unwrap());
                    value = &value[8..];
                }
            }
        } else if kind == ExtraFieldId::INFO_ZIP_UNICODE_PATH {
            require(owner, value.len() >= 5)?;
            if value[0] == 1 && u32_at(value, 1) == rawzip::crc32(raw) {
                let decoded = std::str::from_utf8(&value[5..]).map_err(|_| Cause::Zip)?;
                if !decoded.is_empty() {
                    let decoded = decoded.split('\0').next().unwrap();
                    require(owner, decoded.as_bytes() == raw)?;
                }
            }
        }
    }
    // CPython ignores fewer than four remaining bytes, but refuses a truncated
    // field with its complete type/length prefix.
    require(owner, fields.remaining_bytes().len() < 4)?;
    Ok((bytes, compressed, offset))
}
struct Checked<'a, 'b, R> {
    inner: R,
    file: &'a Retained,
    owner: &'a RefCell<&'b mut Owner>,
}
impl<R: Read> Read for Checked<'_, '_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.file
            .check(&mut self.owner.borrow_mut(), Window::Body)
            .map_err(|_| std::io::Error::other("ZIP original refused"))?;
        let observed = self.inner.read(bytes);
        if observed.is_err() {
            self.owner.borrow_mut().record(Cause::Io);
        }
        self.file
            .check(&mut self.owner.borrow_mut(), Window::Body)
            .map_err(|_| std::io::Error::other("ZIP original refused"))?;
        observed
    }
}
fn payload(
    reader: impl Read,
    member: &Member,
    owner: &RefCell<&mut Owner>,
    mut output: Option<&mut Retained>,
) -> Result<String> {
    // ZipExtFile stops at its declared uncompressed length. It does not require
    // consumption of trailing compressed bytes or a further inflater EOF.
    let mut reader = reader.take(member.bytes);
    // Fresh extraction preserves the reference's 1MiB logical return/write
    // boundary; cached comparison retains its 64KiB logical boundary. Actual
    // decoder reads stay at most64KiB, and both allocations live on the heap.
    let logical = if output.is_some() { 1024 * 1024 } else { 65536 };
    let mut buffer = vec![0; logical];
    let mut count = 0_u64;
    let mut crc = Crc32::new();
    let mut digest = Sha256::new();
    loop {
        owner.borrow_mut().check(Window::Body)?;
        let wanted = member.bytes.saturating_sub(count).min(buffer.len() as u64) as usize;
        if wanted == 0 {
            break;
        }
        let mut size = 0;
        let mut eof = false;
        while size < wanted {
            let end = wanted.min(size + 65536);
            let read = reader.read(&mut buffer[size..end]);
            if read.is_err() && owner.borrow().error().is_none() {
                owner.borrow_mut().record(Cause::Zip);
            }
            let amount = read.map_err(|_| owner.borrow().error().unwrap_or(Cause::Zip))?;
            owner.borrow_mut().check(Window::Body)?;
            if amount == 0 {
                eof = true;
                break;
            }
            size += amount;
        }
        count = count.checked_add(size as u64).ok_or(Cause::Bounds)?;
        crc.update(&buffer[..size]);
        digest.update(&buffer[..size]);
        // ZipExtFile checks CRC before returning its final logical read. A bad
        // final chunk must therefore never enter a fresh output's prefix.
        if eof || count == member.bytes {
            require(&mut owner.borrow_mut(), crc.checksum() == member.crc)?;
        }
        if let Some(output) = &mut output {
            output.write_chunk(
                &buffer[..size],
                count - size as u64,
                &mut owner.borrow_mut(),
            )?;
        }
        if eof {
            break;
        }
    }
    require(
        &mut owner.borrow_mut(),
        count == member.bytes && crc.checksum() == member.crc,
    )?;
    let digest = super::super::files::hex(&digest.finalize());
    if let Some(output) = output {
        output.seal(&digest, &mut owner.borrow_mut())?;
    }
    Ok(digest)
}
fn members(
    file: &Retained,
    directory: &Dir,
    retained: &mut [Option<Retained>; 10],
    metadata: &Metadata,
    owner: &mut Owner,
    extract: bool,
) -> Result<()> {
    require(
        owner,
        file.len() == metadata.zip_bytes() && file.len() <= MAX_ZIP,
    )?;
    let checksum = file.hash(owner)?;
    require(owner, checksum == metadata.zip_sha256())?;
    let end = preflight(file, owner)?;
    let mut buffer = vec![0; 256 * 1024];
    let archive = ZipArchive::from_seekable(file.file(), &mut buffer).map_err(|_| Cause::Zip)?;
    file.check(owner, Window::Body)?;
    require(
        owner,
        archive.eocd_offset() == end.offset
            && archive.directory_offset() == end.directory
            && archive.end_offset() == file.len(),
    )?;
    let mut names = BTreeSet::new();
    let mut members = Vec::new();
    let mut cursor = end.directory;
    let mut total = 0_u64;
    {
        let mut entries = archive.entries(&mut buffer);
        while let Some(header) = entries.next_entry().map_err(|_| Cause::Zip)? {
            file.check(owner, Window::Body)?;
            require(
                owner,
                header.central_directory_offset() == cursor && members.len() < 256,
            )?;
            let raw_path = header.file_path();
            let raw = raw_path.as_ref();
            let canonical = name(raw, owner)?;
            let directory = header.is_dir();
            require(owner, names.insert(canonical.clone()))?;
            let attributes = header.external_attributes();
            let kind = (attributes >> 16) & 0o170000;
            require(
                owner,
                (kind == 0 || kind == if directory { 0o040000 } else { 0o100000 })
                    && (attributes & 0x10 == 0 || directory)
                    && !header.flags().is_encrypted()
                    && [CompressionMethod::STORE, CompressionMethod::DEFLATE]
                        .contains(&header.compression_method()),
            )?;
            let mut fixed = [0; 46];
            read_at(file, cursor, &mut fixed, owner)?;
            require(owner, fixed[6] <= 63)?;
            let (bytes, compressed, offset) = extra(&header, raw, &fixed, owner)?;
            require(
                owner,
                bytes
                    <= if canonical == "image.tar" && !directory {
                        MAX_ZIP
                    } else {
                        MAX_METADATA
                    },
            )?;
            total = total.checked_add(bytes).ok_or(Cause::Bounds)?;
            require(owner, total <= MAX_TOTAL)?;
            let length = 46
                + raw.len()
                + header.extra_fields().remaining_bytes().len()
                + header.comment().as_bytes().len();
            cursor = cursor
                .checked_add(length.try_into().map_err(|_| Cause::Bounds)?)
                .ok_or(Cause::Bounds)?;
            require(owner, cursor <= end.offset)?;
            members.push(Member {
                name: if directory {
                    format!("{canonical}/")
                } else {
                    canonical
                },
                bytes,
                compressed,
                offset,
                end: 0,
                method: header.compression_method(),
                crc: header.crc32(),
                flags: u16_at(&fixed, 8),
            });
        }
    }
    require(owner, !members.is_empty() && cursor == end.offset)?;
    let mut order: Vec<_> = members
        .iter()
        .enumerate()
        .map(|(at, member)| (member.offset, at))
        .collect();
    // CPython reverses a stable ascending header-offset sort, including
    // central-order ties, before assigning each entry's next local boundary.
    order.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut next = end.directory;
    for (offset, at) in order {
        members[at].end = next;
        next = offset;
    }
    if extract {
        require(
            owner,
            SELECTED.iter().all(|name| {
                members
                    .iter()
                    .any(|member| member.name == *name && member.bytes > 0)
            }),
        )?;
    }
    for (at, name) in SELECTED.iter().enumerate() {
        let member = members
            .iter()
            .find(|member| member.name == *name)
            .ok_or(Cause::Zip)?;
        require(owner, member.bytes > 0)?;
        if !extract {
            if retained[at].is_none() {
                retained[at] = Some(Retained::open(directory.clone(), name, owner)?);
            }
            let original = retained[at].as_ref().unwrap();
            original.check(owner, Window::Body)?;
            require(owner, original.len() == member.bytes)?;
        }
        // ZipFile.open applies these unsupported-mode guards to the selected
        // central ZipInfo, while the corresponding local flags are ignored.
        require(owner, member.flags & 0x60 == 0)?;
        let mut fixed = [0; 30];
        read_at(file, member.offset, &mut fixed, owner)?;
        require(owner, &fixed[..4] == b"PK\x03\x04")?;
        let name_bytes = usize::from(u16_at(&fixed, 26));
        let mut local = vec![0; name_bytes];
        read_at(
            file,
            member.offset.checked_add(30).ok_or(Cause::Bounds)?,
            &mut local,
            owner,
        )?;
        require(owner, local == name.as_bytes())?;
        let start = member
            .offset
            .checked_add(30 + u64::from(u16_at(&fixed, 26)) + u64::from(u16_at(&fixed, 28)))
            .ok_or(Cause::Bounds)?;
        let finish = u128::from(start)
            .checked_add(u128::from(member.compressed))
            .ok_or(Cause::Bounds)?;
        // The reference warns instead of refusing when equal local offsets make
        // this boundary equal the entry's own header. Original name, decoded
        // count/CRC and retained-file digest checks still constrain its bytes.
        require(
            owner,
            finish <= u128::from(member.end) || member.end == member.offset,
        )?;
        // A declared ZIP64 compressed extent can exceed u64 while Python still
        // returns the bounded declared output prefix. Only actual original ZIP
        // bytes can be read: no allocation or read uses that logical extent.
        let physical_end =
            u64::try_from(finish.min(u128::from(file.len()))).map_err(|_| Cause::Bounds)?;
        require(owner, start <= physical_end)?;
        if extract {
            retained[at] = Some(Retained::create(directory.clone(), name, owner)?);
        }
        let checks = RefCell::new(&mut *owner);
        let raw = rawzip::RangeReader::new(archive.get_ref(), start..physical_end);
        let checked = Checked {
            inner: raw,
            file,
            owner: &checks,
        };
        let digest = if member.method == CompressionMethod::STORE {
            payload(
                checked,
                member,
                &checks,
                extract.then(|| retained[at].as_mut().unwrap()),
            )?
        } else {
            payload(
                flate2::read::DeflateDecoder::new(checked),
                member,
                &checks,
                extract.then(|| retained[at].as_mut().unwrap()),
            )?
        };
        let actual = retained[at]
            .as_ref()
            .unwrap()
            .hash(&mut checks.borrow_mut())?;
        require(&mut checks.borrow_mut(), actual == digest)?;
    }
    file.check(owner, Window::Body)
}

pub(in crate::release::attestation) fn verify(
    file: &Retained,
    directory: &Dir,
    retained: &mut [Option<Retained>; 10],
    metadata: &Metadata,
    owner: &mut Owner,
) -> Result<()> {
    members(file, directory, retained, metadata, owner, false)
}
pub(in crate::release::attestation) fn extract(
    file: &Retained,
    directory: &Dir,
    retained: &mut [Option<Retained>; 10],
    metadata: &Metadata,
    owner: &mut Owner,
) -> Result<()> {
    members(file, directory, retained, metadata, owner, true)
}
