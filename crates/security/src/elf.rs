//! The audit of the production executable inside an image's files.
//!
//! Nothing of the image is run. Only little-endian ELF64 shared objects for
//! the supported Linux platforms are read, and every offset, table, name and
//! decompression is bounded before it is used. The executable must carry
//! every hardening property and the dependency metadata that the build
//! embedded, and it and every library it needs are bound to their bytes.
//! An object's dynamic table is read where the loader reads it, and no
//! library may undo what the executable is held to: its stack header, its
//! segments and its relocations are held to the same rules.
//! Libraries are looked for inside the image alone, through its own links:
//! neither a path of this host nor its loader is asked.

use crate::{
    Fault, Result, digest,
    document::{Document, Unread},
    files, require,
};
use flate2::{Decompress, FlushDecompress, Status};
use rustix::fs::{Mode, OFlags};
use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    fs::File,
    io::{ErrorKind, Read},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::ExitCode,
};

const USAGE: &str = concat!(
    "usage: simplestchat-security elf --rootfs DIR [--platform linux/amd64|linux/arm64]",
    " --output FILE",
);
/// The most bytes an executable or a library may have.
const MAX_BINARY: u64 = 256 * 1024 * 1024;
/// The most bytes the dependency metadata may have, packed or not.
const MAX_METADATA: usize = 8 * 1024 * 1024;
/// The most bytes a table of names may have.
const MAX_STRINGS: u64 = 1024 * 1024;
/// The most packages the dependency metadata may list.
const MAX_PACKAGES: usize = 16384;
/// The most entries a table may have.
const MAX_TABLE: u64 = 8192;
/// The most links one path may lead through.
const MAX_LINKS: usize = 40;
/// The most bytes of a name.
const MAX_NAME: usize = 256;
/// The most needed libraries a refusal says something of.
const MAX_DIAGNOSTIC: usize = 32;
const HEADER: u64 = 64;
const PROGRAM: u64 = 56;
const SECTION: u64 = 64;
const ENTRY: u64 = 16;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_GNU_STACK: u32 = 0x6474_e551;
const PT_GNU_RELRO: u32 = 0x6474_e552;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;
const DT_RPATH: i64 = 15;
const DT_TEXTREL: i64 = 22;
const DT_BIND_NOW: i64 = 24;
const DT_RUNPATH: i64 = 29;
const DT_FLAGS: i64 = 30;
const DT_FLAGS_1: i64 = 0x6fff_fffb;
/// What makes the loader load something that the executable does not name
/// as needed: two kinds of audit library and two kinds of filter.
const LOADER_HOOKS: [i64; 4] = [0x6fff_fefb, 0x6fff_fefc, 0x7fff_fffd, 0x7fff_ffff];
const DF_TEXTREL: u64 = 4;
const DF_BIND_NOW: u64 = 8;
const DF_1_NOW: u64 = 1;
const DF_1_PIE: u64 = 0x0800_0000;
const ET_DYN: u16 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const SHT_PROGBITS: u32 = 1;
const SHT_STRTAB: u32 = 3;
const SHT_NOBITS: u32 = 8;
/// Where the loader looks for a library, in its order.
const LIBRARY_DIRECTORIES: [&str; 4] = ["/lib64", "/usr/lib64", "/lib", "/usr/lib"];
/// The libraries the executable may need besides its platform's loader.
const RUNTIME_LIBRARIES: [&str; 3] = ["libc.so.6", "libm.so.6", "libgcc_s.so.1"];
/// The executable of the image.
const BINARY: &str = "/app/simplestChat";

/// The machine and the loader of a supported platform.
fn platform(name: &str) -> Option<(u16, &'static str)> {
    match name {
        "linux/amd64" => Some((62, "/lib64/ld-linux-x86-64.so.2")),
        "linux/arm64" => Some((183, "/lib/ld-linux-aarch64.so.1")),
        _ => None,
    }
}

/// Why an audit refuses: a fixed reason, or the protections that are
/// missing with what may be said of the needed libraries.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// The reason.
    pub reason: String,
    /// What may be said of the needed libraries.
    pub dependencies: Option<Document>,
}

/// How an audit ends without a report.
#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// It refuses what it read.
    Refused(Refusal),
    /// An entry that it asked for is not there.
    Missing,
    /// The image's files cannot be read.
    Unreadable,
}

impl From<Fault> for Stop {
    fn from(fault: Fault) -> Self {
        Self::Refused(Refusal {
            reason: fault.0.to_owned(),
            dependencies: None,
        })
    }
}

fn unreadable(error: &std::io::Error) -> Stop {
    if error.kind() == ErrorKind::NotFound {
        Stop::Missing
    } else {
        Stop::Unreadable
    }
}

/// A place in the bytes, which holds whatever a bound has admitted.
fn place(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The whole of a region that lies inside the bytes.
fn region(data: &[u8], offset: u64, size: u64) -> Result<&[u8]> {
    let length = data.len() as u64;
    require(
        offset <= length && size <= length - offset,
        "elf_region_bounds",
    )?;
    Ok(&data[place(offset)..place(offset + size)])
}

fn half(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn word(bytes: &[u8], at: usize) -> u32 {
    let mut value = [0; 4];
    value.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(value)
}

fn long(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}

/// The ASCII name that begins at `offset` and ends with a zero byte.
fn name(data: &[u8], offset: u64) -> Result<&str> {
    require(offset < data.len() as u64, "elf_string_offset")?;
    let rest = &data[place(offset)..];
    let end = rest.iter().position(|byte| *byte == 0);
    let end = end.filter(|end| *end <= MAX_NAME);
    let found = &rest[..end.ok_or(Fault("elf_string_bounds"))?];
    require(found.is_ascii(), "elf_string_encoding")?;
    std::str::from_utf8(found).map_err(|_| Fault("elf_string_encoding"))
}

/// What a program header says of one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Its kind.
    pub kind: u32,
    /// Its flags.
    pub flags: u32,
    /// Where it begins in the file.
    pub offset: u64,
    /// Where it begins in memory.
    pub address: u64,
    /// Its bytes in the file.
    pub file_size: u64,
    /// Its bytes in memory.
    pub memory_size: u64,
}

/// What the policy reads of an executable or a library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elf {
    /// Its segments.
    pub segments: Vec<Segment>,
    /// The values of every tag of its dynamic table, in the order the tags
    /// first appear.
    pub dynamic: Vec<(i64, Vec<u64>)>,
    /// The libraries it needs.
    pub needed: Vec<String>,
    /// The loader it asks for.
    pub interpreter: Option<String>,
    /// Its packed dependency metadata.
    pub audit: Option<Vec<u8>>,
}

impl Elf {
    fn values(&self, tag: i64) -> &[u64] {
        let held = self.dynamic.iter().find(|(held, _)| *held == tag);
        held.map_or(&[], |(_, values)| values)
    }

    fn has(&self, tag: i64) -> bool {
        self.dynamic.iter().any(|(held, _)| *held == tag)
    }

    fn kind(&self, kind: u32) -> impl Iterator<Item = &Segment> {
        self.segments
            .iter()
            .filter(move |segment| segment.kind == kind)
    }

    /// Whether there is one stack header and it says that the stack cannot
    /// be run. The loader makes the stack of the whole process runnable for
    /// an object without one.
    fn stack_protected(&self) -> bool {
        let mut stacks = self.kind(PT_GNU_STACK);
        matches!((stacks.next(), stacks.next()), (Some(stack), None) if stack.flags & PF_X == 0)
    }

    /// Whether no loaded segment can be both written and run.
    fn segments_protected(&self) -> bool {
        let both = PF_X | PF_W;
        !self
            .kind(PT_LOAD)
            .any(|segment| segment.flags & both == both)
    }

    /// Whether the text needs no relocation, which would have it written.
    fn text_protected(&self) -> bool {
        let flags = self.values(DT_FLAGS).first().copied().unwrap_or(0);
        !self.has(DT_TEXTREL) && flags & DF_TEXTREL == 0
    }
}

/// Every segment, each held to the file before any address is resolved.
fn program_headers(data: &[u8], offset: u64, count: u16, size: u16) -> Result<Vec<Segment>> {
    let count = u64::from(count);
    require(
        u64::from(size) == PROGRAM && 0 < count && count <= MAX_TABLE,
        "elf_program_table",
    )?;
    let table = region(data, offset, count * PROGRAM)?;
    let mut segments = Vec::new();
    for raw in table.chunks_exact(place(PROGRAM)) {
        let segment = Segment {
            kind: word(raw, 0),
            flags: word(raw, 4),
            offset: long(raw, 8),
            address: long(raw, 16),
            file_size: long(raw, 32),
            memory_size: long(raw, 40),
        };
        require(segment.file_size <= segment.memory_size, "elf_segment_size")?;
        region(data, segment.offset, segment.file_size)?;
        segments.push(segment);
    }
    Ok(segments)
}

/// Where the bytes at an address lie in the file: in the one loaded
/// segment whose file holds all of them.
fn mapped(segments: &[Segment], address: u64, size: u64) -> Result<u64> {
    let end = u128::from(address) + u128::from(size);
    let holds = |segment: &&Segment| {
        segment.kind == PT_LOAD
            && segment.address <= address
            && end <= u128::from(segment.address) + u128::from(segment.file_size)
    };
    let mut matches = segments.iter().filter(holds);
    let (Some(segment), None) = (matches.next(), matches.next()) else {
        return Err(Fault("elf_virtual_region"));
    };
    let offset = u128::from(segment.offset) + u128::from(address - segment.address);
    u64::try_from(offset).map_err(|_| Fault("elf_region_bounds"))
}

/// The bytes at an address, as the loader reads them.
fn virtual_region<'a>(
    data: &'a [u8],
    segments: &[Segment],
    address: u64,
    size: u64,
) -> Result<&'a [u8]> {
    region(data, mapped(segments, address, size)?, size)
}

/// The one dynamic table: bounded, ended, and without a second value for a
/// tag that may have one. The loader reads the table at its address, so
/// the address must lead, through one loaded segment, to the very bytes
/// that the header names in the file: otherwise the audit would read one
/// table and the loader another.
fn dynamic_table(data: &[u8], segments: &[Segment]) -> Result<Vec<(i64, Vec<u64>)>> {
    let mut tables = segments.iter().filter(|segment| segment.kind == PT_DYNAMIC);
    let (Some(table), None) = (tables.next(), tables.next()) else {
        return Err(Fault("elf_dynamic_table_count"));
    };
    require(
        0 < table.file_size
            && table.file_size <= MAX_TABLE * ENTRY
            && table.file_size.is_multiple_of(ENTRY),
        "elf_dynamic_table_size",
    )?;
    require(
        mapped(segments, table.address, table.file_size) == Ok(table.offset),
        "elf_dynamic_table_mapping",
    )?;
    let mut dynamic: Vec<(i64, Vec<u64>)> = Vec::new();
    let mut ended = false;
    for raw in region(data, table.offset, table.file_size)?.chunks_exact(place(ENTRY)) {
        let (tag, value) = (long(raw, 0).cast_signed(), long(raw, 8));
        if ended {
            require(tag == 0 && value == 0, "elf_dynamic_trailing_data")?;
        } else if tag == 0 {
            ended = true;
        } else if let Some((_, values)) = dynamic.iter_mut().find(|(held, _)| *held == tag) {
            values.push(value);
        } else {
            dynamic.push((tag, vec![value]));
        }
    }
    require(ended, "elf_dynamic_unterminated")?;
    let once = [DT_STRTAB, DT_STRSZ, DT_FLAGS, DT_FLAGS_1, DT_BIND_NOW];
    let twice = |(tag, values): &(i64, Vec<u64>)| once.contains(tag) && values.len() > 1;
    require(!dynamic.iter().any(twice), "elf_duplicate_dynamic_tag")?;
    Ok(dynamic)
}

/// The one section of packed dependency metadata, when there is one.
fn audit_section(
    data: &[u8],
    offset: u64,
    count: u16,
    size: u16,
    names: u16,
) -> Result<Option<Vec<u8>>> {
    let count = u64::from(count);
    require(
        u64::from(size) == SECTION && count <= MAX_TABLE && u64::from(names) < count,
        "elf_section_table",
    )?;
    let table = region(data, offset, count * SECTION)?;
    let sections: Vec<&[u8]> = table.chunks_exact(place(SECTION)).collect();
    let strings = sections[usize::from(names)];
    let length = long(strings, 32);
    require(
        word(strings, 4) == SHT_STRTAB && 0 < length && length <= MAX_STRINGS,
        "elf_section_names_size",
    )?;
    let labels = region(data, long(strings, 24), length)?;
    let mut found = Vec::new();
    for section in sections {
        let (kind, start, length) = (word(section, 4), long(section, 24), long(section, 32));
        if kind != SHT_NOBITS {
            region(data, start, length)?;
        }
        if name(labels, u64::from(word(section, 0)))? == ".dep-v0" {
            require(
                kind == SHT_PROGBITS && 0 < length && length <= MAX_METADATA as u64,
                "elf_audit_section_size",
            )?;
            found.push(region(data, start, length)?.to_vec());
        }
    }
    require(found.len() <= 1, "elf_audit_section_duplicate")?;
    Ok(found.pop())
}

/// A needed library's name: a letter or a digit, then up to 127 letters,
/// digits, dots, lines, pluses and dashes.
fn library_name(name: &str) -> bool {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte);
    let bytes = name.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.len() <= 128
        && bytes.iter().all(|byte| plain(*byte))
}

/// What the policy reads of `data`, which must be a shared object of
/// `platform`. No instruction of it is read.
pub fn parse(data: &[u8], platform_name: &str) -> Result<Elf> {
    let length = data.len() as u64;
    let Some((machine, _)) =
        platform(platform_name).filter(|_| (HEADER..=MAX_BINARY).contains(&length))
    else {
        return Err(Fault("elf_input"));
    };
    require(
        data[..7] == *b"\x7fELF\x02\x01\x01"
            && [0, 3].contains(&data[7])
            && word(data, 20) == 1
            && half(data, 16) == ET_DYN
            && half(data, 18) == machine
            && u64::from(half(data, 52)) == HEADER,
        "elf_header_identity",
    )?;
    let entry = long(data, 24);
    let segments = program_headers(data, long(data, 32), half(data, 56), half(data, 54))?;
    let dynamic = dynamic_table(data, &segments)?;
    let first = |tag: i64| {
        let held = dynamic.iter().find(|(held, _)| *held == tag);
        held.map(|(_, values)| values[0])
    };
    let (Some(table), Some(size)) = (first(DT_STRTAB), first(DT_STRSZ)) else {
        return Err(Fault("elf_dynamic_strings_missing"));
    };
    require(0 < size && size <= MAX_STRINGS, "elf_dynamic_strings_size")?;
    let strings = virtual_region(data, &segments, table, size)?;
    let mut needed: Vec<String> = Vec::new();
    let mut plain = true;
    let wanted = dynamic.iter().find(|(tag, _)| *tag == DT_NEEDED);
    for offset in wanted.map_or(&[][..], |(_, offsets)| offsets) {
        let library = name(strings, *offset)?;
        plain &= library_name(library) && !needed.iter().any(|held| held == library);
        needed.push(library.to_owned());
    }
    require(plain, "elf_needed_name")?;
    let mut interpreters = segments.iter().filter(|segment| segment.kind == PT_INTERP);
    let found = interpreters.next();
    require(interpreters.next().is_none(), "elf_interpreter_count")?;
    let mut interpreter = None;
    if let Some(segment) = found {
        let runs = |segment: &Segment| {
            segment.kind == PT_LOAD
                && segment.flags & PF_X != 0
                && segment.address <= entry
                && u128::from(entry) < u128::from(segment.address) + u128::from(segment.file_size)
        };
        require(segments.iter().any(runs), "elf_entrypoint_not_executable")?;
        let encoded = region(data, segment.offset, segment.file_size)?;
        let asked = name(encoded, 0)?;
        require(
            asked.len() + 1 == encoded.len(),
            "elf_interpreter_trailing_data",
        )?;
        interpreter = Some(asked.to_owned());
    }
    let audit = audit_section(
        data,
        long(data, 40),
        half(data, 60),
        half(data, 58),
        half(data, 62),
    )?;
    Ok(Elf {
        segments,
        dynamic,
        needed,
        interpreter,
        audit,
    })
}

/// The whole of a packed section, unpacked within the bound. A stream that
/// does not end with the section, or that unpacks to more, is out of
/// bounds; one that is no such stream is invalid.
fn unpacked(section: &[u8]) -> Result<Vec<u8>> {
    let bounds = Fault("auditable_compression_bounds");
    let mut stream = Decompress::new(true);
    let mut decoded = Vec::with_capacity(
        section
            .len()
            .saturating_mul(4)
            .clamp(1024, MAX_METADATA + 1),
    );
    loop {
        if decoded.len() == decoded.capacity() {
            let more = decoded.len().min(MAX_METADATA + 1 - decoded.len());
            decoded.reserve_exact(more);
        }
        let (read, written) = (stream.total_in(), decoded.len());
        let rest = &section[place(read)..];
        let status = stream.decompress_vec(rest, &mut decoded, FlushDecompress::None);
        let status = status.map_err(|_| Fault("auditable_metadata_invalid"))?;
        if decoded.len() > MAX_METADATA {
            return Err(bounds);
        }
        if status == Status::StreamEnd {
            break;
        }
        // A stream that neither reads nor writes has no more to give.
        require(
            stream.total_in() > read || decoded.len() > written,
            bounds.0,
        )?;
    }
    require(stream.total_in() == section.len() as u64, bounds.0)?;
    Ok(decoded)
}

/// A package's name: letters, digits, lines and dashes.
fn package_name(name: &str) -> bool {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-';
    !name.is_empty() && name.bytes().all(plain)
}

/// A package's version: three numbers with dots between them, and then any
/// number of tags, each a dash or a plus before letters, digits, dots and
/// dashes.
fn package_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    let mut at = 0;
    for part in 0..3 {
        let digits = bytes[at..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        at += digits;
        let dot = part == 2 || bytes.get(at) == Some(&b'.');
        if digits == 0 || !dot {
            return false;
        }
        at += usize::from(part < 2);
    }
    let tags = &bytes[at..];
    let within = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-';
    let begun = |place: usize| place == 0 || tags[place] == b'+';
    let held =
        |place: usize| !begun(place) || tags.get(place + 1).is_some_and(|next| within(*next));
    tags.first().is_none_or(|first| b"-+".contains(first))
        && tags.iter().all(|byte| within(*byte) || *byte == b'+')
        && (0..tags.len()).all(held)
}

/// The packages that `package` depends on, by their places in the list of
/// `count` packages.
fn dependencies(package: &Document, count: usize) -> Result<Vec<usize>> {
    let Some(listed) = package.get("dependencies") else {
        return Ok(Vec::new());
    };
    let listed = listed.items().ok_or(Fault("auditable_dependencies"))?;
    let place = |item: &Document| {
        let index = item.whole().and_then(|index| usize::try_from(index).ok());
        index.filter(|index| *index < count)
    };
    let places: Option<Vec<usize>> = listed.iter().map(place).collect();
    let places = places.ok_or(Fault("auditable_dependency_index"))?;
    let twice = |at: usize| places[..at].contains(&places[at]);
    require(
        !(0..places.len()).any(twice),
        "auditable_dependency_duplicate",
    )?;
    Ok(places)
}

/// The dependency metadata that the build embedded in `section`: bounded,
/// of the current format, with one local root and no cycle among its
/// packages. The answer binds it to its bytes, packed and unpacked.
pub fn dependency_metadata(section: Option<&[u8]>) -> Result<Document> {
    let section = section.ok_or(Fault("auditable_metadata_missing"))?;
    let decoded = unpacked(section)?;
    let document = Document::read(&decoded).map_err(|unread| match unread {
        Unread::Duplicate => Fault("auditable_duplicate_key"),
        Unread::Invalid => Fault("auditable_metadata_invalid"),
    })?;
    let fields = document.fields().ok_or(Fault("auditable_object"))?;
    let known = |(name, _): &(String, Document)| name == "packages" || name == "format";
    require(fields.iter().all(known), "auditable_fields")?;
    let format = document.get("format").and_then(Document::whole);
    require(format == Some(1), "auditable_format")?;
    let packages = document.get("packages").and_then(Document::items);
    let packages = packages.ok_or(Fault("auditable_packages"))?;
    require(
        !packages.is_empty() && packages.len() <= MAX_PACKAGES,
        "auditable_package_count",
    )?;
    let identity = ["name", "version", "source"];
    let further = ["root", "kind", "dependencies"];
    let mut edges = Vec::new();
    let mut roots = 0;
    for package in packages {
        let fields = package.fields().ok_or(Fault("auditable_package_object"))?;
        let named = |name: &&str| fields.iter().any(|(held, _)| held == name);
        let known = |(name, _): &(String, Document)| {
            identity.contains(&name.as_str()) || further.contains(&name.as_str())
        };
        require(
            identity.iter().all(named) && fields.iter().all(known),
            "auditable_package_fields",
        )?;
        let text = |name: &str| {
            let text = package.get(name).and_then(Document::text);
            let text = text.filter(|text| text.chars().count() <= MAX_NAME);
            text.ok_or(Fault("auditable_package_identity"))
        };
        let (name, version, source) = (text("name")?, text("version")?, text("source")?);
        require(
            package_name(name)
                && package_version(version)
                && ["local", "crates.io", "git", "registry"].contains(&source),
            "auditable_package_identity",
        )?;
        let kind = package.get("kind").map(Document::text);
        let kinds = [Some("build"), Some("runtime")];
        require(
            kind.is_none_or(|kind| kinds.contains(&kind)),
            "auditable_kind",
        )?;
        let root = package.get("root");
        let said = |root: &Document| matches!(root, Document::Bool(_));
        require(root.is_none_or(said), "auditable_root_type")?;
        if root == Some(&Document::Bool(true)) {
            require(
                name == "simplestchat" && source == "local",
                "auditable_root",
            )?;
            roots += 1;
        }
        edges.push(dependencies(package, packages.len())?);
    }
    require(roots == 1, "auditable_root_count")?;
    let mut waiting = vec![0_usize; packages.len()];
    for dependency in edges.iter().flatten() {
        waiting[*dependency] += 1;
    }
    let mut ready: VecDeque<usize> = (0..packages.len()).filter(|at| waiting[*at] == 0).collect();
    let mut visited = 0;
    while let Some(package) = ready.pop_front() {
        visited += 1;
        for dependency in &edges[package] {
            waiting[*dependency] -= 1;
            if waiting[*dependency] == 0 {
                ready.push_back(*dependency);
            }
        }
    }
    require(visited == packages.len(), "auditable_dependency_cycle")?;
    Ok(Document::record([
        ("format", 1_usize.into()),
        ("sha256", digest(&decoded).into()),
        ("compressedSha256", digest(section).into()),
        ("packageCount", packages.len().into()),
        ("packages", packages.to_vec().into()),
    ]))
}

/// The name of the platform's loader, which the C library may name as
/// needed.
fn loader_name(loader: &str) -> &str {
    loader.rsplit('/').next().unwrap_or(loader)
}

fn approved(library: &str, loader: &str) -> bool {
    RUNTIME_LIBRARIES.contains(&library) || library == loader_name(loader)
}

/// Whether a library has the name libraries usually have, so that the name
/// may be said: `lib`, a small letter, up to 47 plain characters, `.so` and
/// up to four numbers of at most eight digits; or a loader's name.
fn conventional(library: &str) -> bool {
    let number = |part: &str| {
        (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    let versions = |rest: &str, most: usize| {
        rest.is_empty()
            || rest.strip_prefix('.').is_some_and(|numbers| {
                numbers.split('.').count() <= most && numbers.split('.').all(number)
            })
    };
    let plain =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_+.-".contains(&byte);
    let ordinary = library.strip_prefix("lib").is_some_and(|rest| {
        let bytes = rest.as_bytes();
        let stem = bytes
            .iter()
            .skip(1)
            .take(47)
            .take_while(|byte| plain(**byte))
            .count();
        bytes.first().is_some_and(u8::is_ascii_lowercase)
            && (0..=stem).any(|taken| {
                rest[1 + taken..]
                    .strip_prefix(".so")
                    .is_some_and(|rest| versions(rest, 4))
            })
    });
    let loaders = ["ld-linux-x86-64.so.", "ld-linux-aarch64.so."];
    let loader = |prefix: &&str| library.strip_prefix(*prefix).is_some_and(number);
    ordinary || loaders.iter().any(loader)
}

/// What may be said of the needed libraries of a refused executable: how
/// many there are, a digest of all their names, and of at most 32 of them,
/// the unapproved first, a digest, whether each is approved, and the name
/// when it is a usual one.
fn dependency_diagnostics(needed: &[String], loader: &str) -> Document {
    let mut ordered: Vec<&String> = needed.iter().collect();
    ordered.sort_by_key(|library| (approved(library, loader), library.as_str()));
    let identity = |library: &&String| {
        Document::record([
            (
                "name",
                conventional(library).then_some(library.as_str()).into(),
            ),
            ("sha256", digest(library.as_bytes()).into()),
            ("approved", approved(library, loader).into()),
        ])
    };
    let names: Vec<&[u8]> = needed.iter().map(String::as_bytes).collect();
    Document::record([
        ("count", needed.len().into()),
        (
            "omitted",
            needed.len().saturating_sub(MAX_DIAGNOSTIC).into(),
        ),
        ("sha256", digest(&names.join(&0)).into()),
        (
            "identities",
            ordered
                .iter()
                .take(MAX_DIAGNOSTIC)
                .map(identity)
                .collect::<Vec<_>>()
                .into(),
        ),
    ])
}

/// Hold an executable to every hardening property, each by itself: it is
/// position independent with the platform's loader, its relocations are
/// made read-only, it binds at once, its stack and every writable segment
/// cannot be run, it has no relocation of its text, no search path, nothing
/// that makes the loader load more, and needs only approved libraries.
pub fn hardening(elf: &Elf, platform_name: &str) -> std::result::Result<Document, Refusal> {
    let loader = platform(platform_name).map_or("", |(_, loader)| loader);
    let first = |tag: i64| elf.values(tag).first().copied().unwrap_or(0);
    let (flags, further) = (first(DT_FLAGS), first(DT_FLAGS_1));
    let checks = [
        (
            "pie",
            further & DF_1_PIE != 0 && elf.interpreter.as_deref() == Some(loader),
        ),
        (
            "relro",
            elf.kind(PT_GNU_RELRO)
                .any(|segment| segment.memory_size != 0),
        ),
        (
            "bindNow",
            elf.has(DT_BIND_NOW) || flags & DF_BIND_NOW != 0 || further & DF_1_NOW != 0,
        ),
        ("nonExecutableStack", elf.stack_protected()),
        ("noWritableExecutableLoad", elf.segments_protected()),
        ("noTextRelocations", elf.text_protected()),
        ("noRpath", !elf.has(DT_RPATH) && !elf.has(DT_RUNPATH)),
        (
            "noLoaderHooks",
            !LOADER_HOOKS.iter().any(|tag| elf.has(*tag)),
        ),
        (
            "approvedLibraries",
            !elf.needed.is_empty() && elf.needed.iter().all(|library| approved(library, loader)),
        ),
    ];
    let failed: Vec<&str> = checks
        .iter()
        .filter(|(_, held)| !held)
        .map(|(name, _)| *name)
        .collect();
    if failed.is_empty() {
        return Ok(Document::record(
            checks.map(|(name, held)| (name, held.into())),
        ));
    }
    Err(Refusal {
        reason: format!("elf_hardening:{}", failed.join(",")),
        dependencies: Some(dependency_diagnostics(&elf.needed, loader)),
    })
}

/// The steps of a path as the image names them: no empty step and none
/// that stays where it is.
fn steps(path: &[u8]) -> impl DoubleEndedIterator<Item = &[u8]> {
    path.split(|byte| *byte == b'/')
        .filter(|step| !step.is_empty() && *step != b".")
}

/// The file that `name` leads to inside the image at `root`, through the
/// image's own links: a link to an absolute path begins again at the
/// image's root, and no step leads out of it. Every step but the last is a
/// directory and the last is a regular file.
pub fn image_path(root: &Path, name: &str) -> std::result::Result<PathBuf, Stop> {
    require(
        name.starts_with('/') && !name.contains('\0'),
        "image_path_invalid",
    )?;
    let mut pending: VecDeque<OsString> = steps(name.as_bytes())
        .map(|step| OsStr::from_bytes(step).to_owned())
        .collect();
    let mut resolved: Vec<OsString> = Vec::new();
    let mut links = 0;
    while let Some(step) = pending.pop_front() {
        if step == ".." {
            require(resolved.pop().is_some(), "image_path_escape")?;
            continue;
        }
        let mut path = root.to_path_buf();
        path.extend(&resolved);
        path.push(&step);
        let entry = path
            .symlink_metadata()
            .map_err(|error| unreadable(&error))?;
        if entry.file_type().is_symlink() {
            links += 1;
            require(links <= MAX_LINKS, "image_symlink_limit")?;
            let target = std::fs::read_link(&path).map_err(|error| unreadable(&error))?;
            let target = target.into_os_string().into_vec();
            if target.first() == Some(&b'/') {
                resolved.clear();
            }
            for step in steps(&target).rev() {
                pending.push_front(OsStr::from_bytes(step).to_owned());
            }
        } else {
            let kind = entry.file_type();
            let wanted = if pending.is_empty() {
                kind.is_file()
            } else {
                kind.is_dir()
            };
            require(wanted, "image_file_kind")?;
            resolved.push(step);
        }
    }
    require(!resolved.is_empty(), "image_path_empty")?;
    let mut path = root.to_path_buf();
    path.extend(&resolved);
    Ok(path)
}

/// The whole of a regular file of the image that is no link, has bytes and
/// has no more of them than a binary may.
pub fn read_binary(path: &Path) -> std::result::Result<Vec<u8>, Stop> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = rustix::fs::open(path, flags, Mode::empty());
    let file = opened
        .map(File::from)
        .map_err(|error| unreadable(&error.into()))?;
    let entry = file.metadata().map_err(|error| unreadable(&error))?;
    require(
        entry.is_file() && 0 < entry.len() && entry.len() <= MAX_BINARY,
        "elf_file_size",
    )?;
    let mut content = Vec::new();
    let read = file.take(MAX_BINARY + 1).read_to_end(&mut content);
    read.map_err(|error| unreadable(&error))?;
    require(content.len() as u64 == entry.len(), "elf_file_changed")?;
    Ok(content)
}

/// The library `name` of the image: the first of the loader's directories
/// that holds it. Every directory that holds it must hold the same bytes,
/// so that no order of looking hides another library.
fn library_path(root: &Path, name: &str) -> std::result::Result<PathBuf, Stop> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for directory in LIBRARY_DIRECTORIES {
        match image_path(root, &format!("{directory}/{name}")) {
            Ok(candidate) if candidates.contains(&candidate) => {}
            Ok(candidate) => candidates.push(candidate),
            Err(Stop::Missing) => {}
            Err(stop) => return Err(stop),
        }
    }
    let Some(first) = candidates.first() else {
        return Err(Fault("elf_dependency_missing").into());
    };
    let held = digest(&read_binary(first)?);
    for other in &candidates[1..] {
        require(
            digest(&read_binary(other)?) == held,
            "elf_dependency_ambiguous",
        )?;
    }
    Ok(first.clone())
}

/// Audit the executable of the image whose files are below `root`: its
/// hardening, its dependency metadata, and the loader and every library
/// that it or one of them needs, each bound to its bytes. A library may
/// need only approved libraries, may have neither a search path nor
/// anything that makes the loader load more, and is held to the
/// executable's rules for its stack header, its segments and its text.
pub fn audit(root: &Path, platform_name: &str) -> std::result::Result<Document, Stop> {
    let entry = root.symlink_metadata();
    require(
        entry.is_ok_and(|entry| entry.is_dir()),
        "image_root_invalid",
    )?;
    let data = read_binary(&image_path(root, BINARY)?)?;
    let executable = parse(&data, platform_name)?;
    let checks = hardening(&executable, platform_name).map_err(Stop::Refused)?;
    let metadata = dependency_metadata(executable.audit.as_deref())?;
    let loader = platform(platform_name).map_or("", |(_, loader)| loader);
    let mut queue = VecDeque::from([image_path(root, loader)?]);
    for library in &executable.needed {
        queue.push_back(library_path(root, library)?);
    }
    let mut libraries = Vec::new();
    let mut visited: Vec<PathBuf> = Vec::new();
    while let Some(path) = queue.pop_front() {
        if visited.contains(&path) {
            continue;
        }
        let content = read_binary(&path)?;
        let library = parse(&content, platform_name)?;
        require(
            library.needed.iter().all(|needed| approved(needed, loader)),
            "elf_transitive_dependency_forbidden",
        )?;
        require(
            !library.has(DT_RPATH) && !library.has(DT_RUNPATH),
            "elf_library_rpath",
        )?;
        require(
            !LOADER_HOOKS.iter().any(|tag| library.has(*tag)),
            "elf_library_loader_hook",
        )?;
        require(library.stack_protected(), "elf_library_stack")?;
        require(
            library.segments_protected(),
            "elf_library_writable_executable",
        )?;
        require(library.text_protected(), "elf_library_text_relocations")?;
        let below = path.strip_prefix(root).ok().and_then(Path::to_str);
        let below = below.ok_or(Stop::Unreadable)?;
        libraries.push(Document::record([
            ("path", format!("/{below}").into()),
            ("sha256", digest(&content).into()),
            ("bytes", content.len().into()),
            (
                "needed",
                library
                    .needed
                    .iter()
                    .map(|name| name.as_str().into())
                    .collect::<Vec<Document>>()
                    .into(),
            ),
        ]));
        for needed in &library.needed {
            queue.push_back(library_path(root, needed)?);
        }
        visited.push(path);
    }
    Ok(Document::record([
        ("passed", true.into()),
        ("platform", platform_name.into()),
        ("binary", BINARY.into()),
        ("binarySha256", digest(&data).into()),
        ("binaryBytes", data.len().into()),
        ("checks", checks),
        ("auditable", metadata),
        ("libraries", libraries.into()),
        ("standardDirectoryCandidatesEqual", true.into()),
        ("noLibraryLoaderHooks", true.into()),
    ]))
}

/// The report of an audit, whichever way it ended.
fn report(audited: std::result::Result<Document, Stop>) -> (Document, bool) {
    let refusal = match audited {
        Ok(report) => return (report, true),
        Err(Stop::Refused(refusal)) => refusal,
        Err(Stop::Missing | Stop::Unreadable) => Refusal {
            reason: "elf_filesystem_error".to_owned(),
            dependencies: None,
        },
    };
    let mut fields = vec![
        ("passed".to_owned(), false.into()),
        ("reason".to_owned(), refusal.reason.into()),
    ];
    if let Some(dependencies) = refusal.dependencies {
        fields.push(("dynamicDependencies".to_owned(), dependencies));
    }
    (Document::Record(fields), false)
}

/// What a command line asks for: the image's files, the platform and the
/// new file of the report.
fn asked(arguments: &[String]) -> Option<(PathBuf, &'static str, PathBuf)> {
    let (mut rootfs, mut output, mut chosen) = (None, None, None);
    let mut rest = arguments.iter();
    while let Some(option) = rest.next() {
        let value = rest
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with('-'))?;
        match option.as_str() {
            "--rootfs" if rootfs.is_none() => rootfs = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--platform" if chosen.is_none() => {
                let offered = ["linux/amd64", "linux/arm64"];
                chosen = Some(*offered.iter().find(|offered| **offered == value)?);
            }
            _ => return None,
        }
    }
    Some((rootfs?, chosen.unwrap_or("linux/amd64"), output?))
}

/// Audit the image that a command line names and write the report as a new
/// private file. Whether the audit passed is the answer.
fn checked(arguments: &[String]) -> Option<Result<bool>> {
    let (rootfs, platform_name, output) = asked(arguments)?;
    let (report, passed) = report(audit(&rootfs, platform_name));
    let written = files::write_private(&output, report.pretty().as_bytes(), 0o600);
    Some(written.map(|()| passed))
}

/// Run `elf --rootfs DIR [--platform NAME] --output FILE`: status 0 when the
/// audit passed, 1 when it did not, with the report written either way, and
/// one line on standard error when the report cannot be written.
pub fn run(arguments: &[String]) -> ExitCode {
    match checked(arguments) {
        None => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Some(Ok(true)) => ExitCode::SUCCESS,
        Some(Ok(false)) => ExitCode::from(1),
        Some(Err(fault)) => {
            eprintln!("ELF audit failed: {fault}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests;
