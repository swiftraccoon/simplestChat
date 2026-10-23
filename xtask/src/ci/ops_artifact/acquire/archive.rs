//! Closed flat ZIP admission. The parent pins the original ZIP digest/stamp and
//! owns every output namespace, explicit close and eventual publication. Parsing
//! borrows the same File; no descriptor or untrusted path is transferred here.

use super::{failure, io};
use crate::tooling::Result;
use rawzip::{CompressionMethod, ReaderAt, ZipArchive, ZipArchiveEntryWayfinder};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::{FileExt, MetadataExt},
};

pub(super) const MAX_ZIP: u64 = 300 * 1024 * 1024;
const MAX_TOTAL: u64 = 281 * 1024 * 1024;
const FIXED: [&str; 6] = [
    "simplestchat-ops",
    "ops-release.json",
    "ops-evidence.json",
    "ops-predicate.json",
    "ops-preparation.json",
    "ops-attestation.jsonl",
];

pub(super) struct Member {
    name: String,
    bytes: u64,
}
impl Member {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
pub(super) struct Record {
    member: Member,
    sha256: String,
}
impl Record {
    pub fn name(&self) -> &str {
        self.member.name()
    }
    pub fn bytes(&self) -> u64 {
        self.member.bytes()
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}
pub(super) struct Inventory {
    records: Vec<Record>,
    bytes: u64,
}
impl Inventory {
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// A failed begin/write/finish can retain an active private output. The caller
/// always settles that owner after extract returns, including on refusal.
pub(super) trait Sink {
    fn begin(&mut self, member: &Member) -> Result<()>;
    fn write(&mut self, bytes: &[u8]) -> Result<()>;
    fn finish(&mut self, member: &Member, sha256: &str) -> Result<()>;
}

struct Guard<'a, F> {
    file: &'a File,
    stamp: Metadata,
    checkpoint: &'a mut F,
}
impl<F: FnMut() -> Result<()>> Guard<'_, F> {
    fn check(&mut self) -> Result<()> {
        (self.checkpoint)()?;
        let now = io(self.file.metadata())?;
        if !same(&self.stamp, &now) {
            return Err(failure(1));
        }
        Ok(())
    }
    fn read<const N: usize>(&mut self, offset: u64) -> Result<[u8; N]> {
        self.check()?;
        if offset
            .checked_add(N as u64)
            .is_none_or(|end| end > self.stamp.len())
        {
            return Err(failure(1));
        }
        let mut value = [0; N];
        io(self.file.read_exact_at(&mut value, offset))?;
        self.check()?;
        Ok(value)
    }
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(failure(1)) }
}
fn maximum(name: &str) -> Option<u64> {
    match name {
        "simplestchat-ops" => Some(256 * 1024 * 1024),
        "ops-attestation.jsonl" => Some(8 * 1024 * 1024),
        "ops-release.json"
        | "ops-evidence.json"
        | "ops-predicate.json"
        | "ops-preparation.json" => Some(64 * 1024),
        _ => name
            .strip_prefix("ops-evidence-")
            .and_then(|value| value.strip_suffix(".bin"))
            .filter(|value| value.len() == 2 && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|value| *value < 16)
            .map(|_| 1024 * 1024),
    }
}

struct Located {
    member: Member,
    location: ZipArchiveEntryWayfinder,
    method: CompressionMethod,
    header: u64,
    compressed: u64,
    crc: u32,
    flags: u16,
    wide: bool,
}

/// Name-override, encryption, fork and unknown extension semantics are outside
/// this flat profile. Harmless timestamp/UID fields are bounded but not applied.
fn extras(mut fields: rawzip::extra_fields::ExtraFields<'_>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for (kind, bytes) in fields.by_ref() {
        require(seen.insert(kind.as_u16()))?;
        require(match kind.as_u16() {
            0x0001 => matches!(bytes.len(), 8 | 16 | 24),
            0x5455 => !bytes.is_empty() && bytes.len() <= 13,
            0x000a => bytes.len() <= 64,
            0x7875 => bytes.len() <= 19,
            _ => false,
        })?;
    }
    require(fields.remaining_bytes().is_empty())
}

fn wide_fields(fields: rawzip::extra_fields::ExtraFields<'_>, needed: &[u64]) -> Result<()> {
    let fields: Vec<_> = fields.filter(|(kind, _)| kind.as_u16() == 1).collect();
    if needed.is_empty() {
        return require(fields.is_empty());
    }
    require(fields.len() == 1 && fields[0].1.len() == needed.len() * 8)?;
    require(
        needed
            .iter()
            .enumerate()
            .all(|(n, value)| u64_at(fields[0].1, n * 8) == *value),
    )
}

fn directory<R: ReaderAt, F: FnMut() -> Result<()>>(
    archive: &ZipArchive<R>,
    guard: &mut Guard<'_, F>,
) -> Result<(Vec<Located>, u64)> {
    require((6..=22).contains(&archive.entries_hint()))?;
    let mut buffer = vec![0; 256 * 1024];
    let mut entries = archive.entries(&mut buffer);
    let mut members = Vec::new();
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    let mut central_end = archive.directory_offset();
    loop {
        guard.check()?;
        let next = entries.next_entry().map_err(|_| failure(1))?;
        guard.check()?;
        let Some(entry) = next else { break };
        require(members.len() < 22 && entry.central_directory_offset() == central_end)?;
        let raw = entry.file_path().as_bytes();
        let name = std::str::from_utf8(raw).map_err(|_| failure(1))?;
        let cap = maximum(name).ok_or_else(|| failure(1))?;
        let bytes = entry.uncompressed_size_hint();
        require(bytes != 0 && bytes <= cap && names.insert(name.to_owned()))?;
        let flags = entry.flags().bits();
        require(flags & !0x080e == 0 && !entry.is_dir())?;
        require(matches!(
            entry.external_attributes() >> 16 & 0o170000,
            0 | 0o100000
        ))?;
        require(entry.external_attributes() & 0x10 == 0)?;
        let method = entry.compression_method();
        require(matches!(
            method,
            CompressionMethod::STORE | CompressionMethod::DEFLATE
        ))?;
        require(method != CompressionMethod::STORE || flags & 6 == 0)?;
        extras(entry.extra_fields())?;
        require(entry.comment().as_bytes().is_empty())?;
        let fixed = guard.read::<46>(central_end)?;
        require(u16_at(&fixed, 34) == 0 && u16_at(&fixed, 6) <= 45)?;
        let mut wide = Vec::new();
        for (at, value) in [
            (24, bytes),
            (20, entry.compressed_size_hint()),
            (42, entry.local_header_offset()),
        ] {
            if u32_at(&fixed, at) == u32::MAX {
                wide.push(value);
            } else {
                require(u64::from(u32_at(&fixed, at)) == value)?;
            }
        }
        wide_fields(entry.extra_fields(), &wide)?;
        require(method != CompressionMethod::STORE || entry.compressed_size_hint() == bytes)?;
        central_end = central_end
            .checked_add(
                46 + raw.len() as u64 + entry.extra_fields().remaining_bytes().len() as u64,
            )
            .ok_or_else(|| failure(1))?;
        total = total.checked_add(bytes).ok_or_else(|| failure(1))?;
        require(total <= MAX_TOTAL)?;
        members.push(Located {
            member: Member {
                name: name.into(),
                bytes,
            },
            location: entry.wayfinder(),
            method,
            header: entry.local_header_offset(),
            compressed: entry.compressed_size_hint(),
            crc: entry.crc32(),
            flags,
            wide: u32_at(&fixed, 20) == u32::MAX || u32_at(&fixed, 24) == u32::MAX,
        });
    }
    require(members.len() as u64 == archive.entries_hint())?;
    require(FIXED.iter().all(|name| names.contains(*name)))?;
    let evidence = names.len() - FIXED.len();
    require((0..evidence).all(|n| names.contains(&format!("ops-evidence-{n:02}.bin"))))?;
    footer(archive, central_end, members.len() as u64, guard)?;
    Ok((members, total))
}

/// rawzip locates/parses ZIP and ZIP64 records. These fixed field comparisons
/// additionally refuse its recovery/prelude/trailer/multi-disk allowances.
fn footer<R: ReaderAt, F: FnMut() -> Result<()>>(
    archive: &ZipArchive<R>,
    central_end: u64,
    count: u64,
    guard: &mut Guard<'_, F>,
) -> Result<()> {
    let end = archive.eocd_offset();
    let eocd = guard.read::<22>(end)?;
    require(archive.end_offset() == guard.stamp.len())?;
    require(u32_at(&eocd, 0) == 0x0605_4b50 && u16_at(&eocd, 20) == 0)?;
    require(u16_at(&eocd, 4) == 0 && u16_at(&eocd, 6) == 0)?;
    let size = central_end
        .checked_sub(archive.directory_offset())
        .ok_or_else(|| failure(1))?;
    for at in [8, 10] {
        require(u64::from(u16_at(&eocd, at)) == count || u16_at(&eocd, at) == u16::MAX)?;
    }
    for (at, expected) in [(12, size), (16, archive.directory_offset())] {
        require(u64::from(u32_at(&eocd, at)) == expected || u32_at(&eocd, at) == u32::MAX)?;
    }
    if central_end == end {
        return require(
            u16_at(&eocd, 8) != u16::MAX
                && u16_at(&eocd, 10) != u16::MAX
                && u32_at(&eocd, 12) != u32::MAX
                && u32_at(&eocd, 16) != u32::MAX,
        );
    }
    require(central_end.checked_add(76) == Some(end))?;
    let zip64 = guard.read::<56>(central_end)?;
    let locator = guard.read::<20>(central_end + 56)?;
    require(u32_at(&zip64, 0) == 0x0606_4b50 && u64_at(&zip64, 4) == 44)?;
    require(u16_at(&zip64, 14) <= 45 && u32_at(&zip64, 16) == 0 && u32_at(&zip64, 20) == 0)?;
    require(u64_at(&zip64, 24) == count && u64_at(&zip64, 32) == count)?;
    require(u64_at(&zip64, 40) == size && u64_at(&zip64, 48) == archive.directory_offset())?;
    require(u32_at(&locator, 0) == 0x0706_4b50 && u32_at(&locator, 4) == 0)?;
    require(u64_at(&locator, 8) == central_end && u32_at(&locator, 16) == 1)
}

fn layout<R: ReaderAt, F: FnMut() -> Result<()>>(
    archive: &ZipArchive<R>,
    members: &mut [Located],
    guard: &mut Guard<'_, F>,
) -> Result<()> {
    members.sort_by_key(|entry| entry.header);
    let mut next = 0_u64;
    let mut buffer = vec![0; 128 * 1024];
    for (index, member) in members.iter().enumerate() {
        require(member.header == next)?;
        guard.check()?;
        let entry = archive.get_entry(member.location).map_err(|_| failure(1))?;
        let local = entry.local_header(&mut buffer).map_err(|_| failure(1))?;
        guard.check()?;
        require(local.file_path().as_bytes() == member.member.name.as_bytes())?;
        require(
            local.flags().bits() == member.flags && local.compression_method() == member.method,
        )?;
        extras(local.extra_fields())?;
        let fixed = guard.read::<30>(member.header)?;
        require(u16_at(&fixed, 4) <= 45)?;
        let mut wide = Vec::new();
        for (at, value) in [
            (22, local.uncompressed_size_hint()),
            (18, local.compressed_size_hint()),
        ] {
            if u32_at(&fixed, at) == u32::MAX {
                wide.push(value);
            } else {
                require(u64::from(u32_at(&fixed, at)) == value)?;
            }
        }
        wide_fields(local.extra_fields(), &wide)?;
        let actual = (
            local.crc32(),
            local.compressed_size_hint(),
            local.uncompressed_size_hint(),
        );
        let expected = (member.crc, member.compressed, member.member.bytes);
        require(actual == expected || (member.flags & 8 != 0 && actual == (0, 0, 0)))?;
        let (start, end) = entry.compressed_data_range();
        require(end.checked_sub(start) == Some(member.compressed))?;
        require(
            start
                == member.header
                    + 30
                    + u64::from(u16_at(&fixed, 26))
                    + u64::from(u16_at(&fixed, 28)),
        )?;
        let boundary = members
            .get(index + 1)
            .map_or(archive.directory_offset(), |v| v.header);
        if member.flags & 8 == 0 {
            require(end == boundary)?;
        } else {
            descriptor(member, end, boundary, guard)?;
        }
        next = boundary;
    }
    require(next == archive.directory_offset())
}

fn descriptor<F: FnMut() -> Result<()>>(
    member: &Located,
    end: u64,
    boundary: u64,
    guard: &mut Guard<'_, F>,
) -> Result<()> {
    let plain = if member.wide { 20 } else { 12 };
    let length = boundary.checked_sub(end).ok_or_else(|| failure(1))?;
    require(length == plain || length == plain + 4)?;
    let offset = if length == plain + 4 {
        require(u32_at(&guard.read::<4>(end)?, 0) == 0x0807_4b50)?;
        end + 4
    } else {
        end
    };
    if member.wide {
        let bytes = guard.read::<20>(offset)?;
        require(
            u32_at(&bytes, 0) == member.crc
                && u64_at(&bytes, 4) == member.compressed
                && u64_at(&bytes, 12) == member.member.bytes,
        )
    } else {
        let bytes = guard.read::<12>(offset)?;
        require(
            u32_at(&bytes, 0) == member.crc
                && u64::from(u32_at(&bytes, 4)) == member.compressed
                && u64::from(u32_at(&bytes, 8)) == member.member.bytes,
        )
    }
}

fn body<F: FnMut() -> Result<()>>(
    reader: &mut impl Read,
    member: &Member,
    sink: &mut impl Sink,
    guard: &mut Guard<'_, F>,
) -> Result<String> {
    let mut total = 0_u64;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        guard.check()?;
        let remaining = member.bytes - total;
        let maximum = (remaining + 1).min(bytes.len() as u64) as usize;
        let count = io(reader.read(&mut bytes[..maximum]))?;
        guard.check()?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or_else(|| failure(1))?;
        require(total <= member.bytes)?;
        hash.update(&bytes[..count]);
        sink.write(&bytes[..count])?;
        guard.check()?;
    }
    require(total == member.bytes)?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Require the mature codec's explicit StreamEnd result together with the full
/// size, CRC and exact compressed-byte consumption checks.
fn inflated<F: FnMut() -> Result<()>>(
    mut reader: impl Read,
    member: &Located,
    sink: &mut impl Sink,
    guard: &mut Guard<'_, F>,
) -> Result<String> {
    let mut codec = flate2::Decompress::new(false);
    let mut input = [0; 65536];
    let mut output = [0; 65536];
    let (mut position, mut valid) = (0, 0);
    let mut left = member.compressed;
    let mut hash = Sha256::new();
    let mut crc = rawzip::Crc32::new();
    loop {
        guard.check()?;
        if position == valid && left != 0 {
            let maximum = left.min(input.len() as u64) as usize;
            valid = io(reader.read(&mut input[..maximum]))?;
            guard.check()?;
            require(valid != 0)?;
            left -= valid as u64;
            position = 0;
        }
        let before = (codec.total_in(), codec.total_out());
        let maximum = (member.member.bytes - before.1 + 1).min(output.len() as u64) as usize;
        let flush = if left == 0 && position == valid {
            flate2::FlushDecompress::Finish
        } else {
            flate2::FlushDecompress::None
        };
        let status = codec
            .decompress(&input[position..valid], &mut output[..maximum], flush)
            .map_err(|_| failure(1))?;
        guard.check()?;
        let consumed = codec.total_in() - before.0;
        let produced = codec.total_out() - before.1;
        require(codec.total_out() <= member.member.bytes)?;
        position += consumed as usize;
        if produced != 0 {
            let bytes = &output[..produced as usize];
            hash.update(bytes);
            crc.update(bytes);
            sink.write(bytes)?;
            guard.check()?;
        }
        if status == flate2::Status::StreamEnd {
            require(
                codec.total_in() == member.compressed
                    && codec.total_out() == member.member.bytes
                    && crc.checksum() == member.crc,
            )?;
            return Ok(hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect());
        }
        require(consumed != 0 || produced != 0)?;
    }
}

/// Structural admission precedes every sink acquisition. Private materialized
/// members still require unchanged RetainedMetadata graph/payload validation and
/// fresh cryptographic verification; this inventory supplies neither authority.
/// The borrowed ZIP cursor is unspecified on return; the parent seeks explicitly
/// before any later read. All retained descriptor ownership stays with it.
pub(super) fn extract(
    zip: &mut File,
    sink: &mut impl Sink,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<Inventory> {
    checkpoint()?;
    let stamp = io(zip.metadata())?;
    require(
        stamp.is_file()
            && stamp.nlink() == 1
            && stamp.mode() & 0o7777 == 0o600
            && stamp.uid() == rustix::process::getuid().as_raw()
            && (22..=MAX_ZIP).contains(&stamp.len()),
    )?;
    let file: &File = zip;
    let mut guard = Guard {
        file,
        stamp,
        checkpoint,
    };
    let mut buffer = vec![0; 256 * 1024];
    let archive = ZipArchive::from_seekable(file, &mut buffer).map_err(|_| failure(1))?;
    guard.check()?;
    let (mut members, total) = directory(&archive, &mut guard)?;
    layout(&archive, &mut members, &mut guard)?;
    let mut records = Vec::new();
    for member in members {
        guard.check()?;
        let entry = archive.get_entry(member.location).map_err(|_| failure(1))?;
        guard.check()?;
        sink.begin(&member.member)?;
        guard.check()?;
        let sha256 = if member.method == CompressionMethod::STORE {
            require(member.compressed == member.member.bytes)?;
            let mut reader = entry.verifying_reader(entry.reader());
            body(&mut reader, &member.member, sink, &mut guard)?
        } else {
            inflated(entry.reader(), &member, sink, &mut guard)?
        };
        sink.finish(&member.member, &sha256)?;
        guard.check()?;
        records.push(Record {
            member: member.member,
            sha256,
        });
    }
    guard.check()?;
    Ok(Inventory {
        records,
        bytes: total,
    })
}
