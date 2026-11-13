//! Member selection: an authenticated archive is read in memory, every entry
//! is judged, and only the members the lock names are kept, under the
//! destinations it gives them. No archive path is ever written.

use super::{
    Fault, Limits, Result, digest,
    lock::{Asset, Format},
    require,
};
use crate::record::safe_name;
use rawzip::{CompressionMethod, ZipArchive, ZipArchiveEntryWayfinder, extra_fields::ExtraFieldId};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read, Write},
    sync::LazyLock,
};

/// Bytes that are not the archive their format says, or one that is cut or
/// damaged.
const FORMAT: Fault = Fault("tool_archive_format");
const COMPRESSION: Fault = Fault("tool_archive_compression");
const PATH: Fault = Fault("tool_archive_path");

/// A destination and the bytes to write there, in the archive's order.
pub(super) type Selected = Vec<(String, Vec<u8>)>;

/// The files `asset` selects from `archive`, once its digest holds.
pub(super) fn selected(asset: &Asset, archive: &[u8], limits: &Limits) -> Result<Selected> {
    require(digest(archive) == asset.sha256, "tool_archive_digest")?;
    let found = match asset.format {
        Format::Binary => {
            require(
                !archive.is_empty() && archive.len() as u64 <= limits.executable,
                "tool_executable_size",
            )?;
            asset
                .files
                .values()
                .map(|destination| (destination.clone(), archive.to_vec()))
                .collect()
        }
        Format::Zip => wheel(asset, archive, limits)?,
        Format::TarGz => tarball(asset, &gunzip(archive, limits.unpacked)?, limits)?,
        Format::TarXz => tarball(asset, &unxz(archive, limits.unpacked)?, limits)?,
    };
    require(found.len() == asset.files.len(), "tool_executable_missing")?;
    Ok(found)
}

/// Every member of the stream, and nothing after the last one.
fn gunzip(packed: &[u8], limit: u64) -> Result<Vec<u8>> {
    let mut unpacked = Vec::new();
    flate2::read::MultiGzDecoder::new(packed)
        .take(limit.saturating_add(1))
        .read_to_end(&mut unpacked)
        .map_err(|_| COMPRESSION)?;
    require(unpacked.len() as u64 <= limit, "tool_archive_expansion")?;
    Ok(unpacked)
}

/// Output that stops accepting bytes at its bound instead of growing past it.
struct Capped {
    bytes: Vec<u8>,
    room: u64,
}

impl Write for Capped {
    fn write(&mut self, part: &[u8]) -> std::io::Result<usize> {
        let size = part.len() as u64;
        if size > self.room {
            return Err(std::io::Error::other("unpacked bound"));
        }
        self.room -= size;
        self.bytes.extend_from_slice(part);
        Ok(part.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One whole stream; the decoder refuses anything after it.
fn unxz(packed: &[u8], limit: u64) -> Result<Vec<u8>> {
    let mut input = packed;
    let mut output = Capped {
        bytes: Vec::new(),
        room: limit.saturating_add(1),
    };
    lzma_rs::xz_decompress(&mut input, &mut output).map_err(|_| COMPRESSION)?;
    require(output.bytes.len() as u64 <= limit, "tool_archive_expansion")?;
    Ok(output.bytes)
}

fn tarball(asset: &Asset, unpacked: &[u8], limits: &Limits) -> Result<Selected> {
    require(!unpacked.is_empty(), FORMAT.0)?;
    let mut archive = tar::Archive::new(unpacked);
    let mut names = BTreeSet::new();
    let mut found = Vec::new();
    for entry in archive.entries().map_err(|_| FORMAT)? {
        let mut entry = entry.map_err(|_| FORMAT)?;
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            continue;
        }
        let raw = entry.path_bytes().into_owned();
        let text = std::str::from_utf8(&raw).map_err(|_| PATH)?;
        // The oldest format marks a directory only by its trailing slash.
        let old = entry.header().as_bytes()[156] == 0;
        let directory = kind == tar::EntryType::Directory || (old && text.ends_with('/'));
        let file =
            !directory && matches!(kind, tar::EntryType::Regular | tar::EntryType::Continuous);
        let name = if directory {
            text.trim_end_matches('/')
        } else {
            text
        };
        require(names.len() < limits.members && safe_name(name), PATH.0)?;
        require(names.insert(name.to_owned()), "tool_archive_duplicate")?;
        require(file || directory, "tool_archive_entry_type")?;
        let size = entry.size();
        require(size <= limits.executable, "tool_archive_entry_size")?;
        if let Some(destination) = asset.files.get(name) {
            require(file && size > 0, "tool_executable_type")?;
            let mut content = Vec::new();
            (&mut entry)
                .take(limits.executable.saturating_add(1))
                .read_to_end(&mut content)
                .map_err(|_| FORMAT)?;
            found.push((destination.clone(), content));
        }
    }
    Ok(found)
}

// The code page a ZIP name is in unless its UTF-8 flag is set.
static CP437: LazyLock<Vec<char>> = LazyLock::new(|| {
    concat!(
        "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
        "áíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
        "└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
        "αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    )
    .chars()
    .collect()
});

fn decoded(raw: &[u8], utf8: bool) -> Result<String> {
    if utf8 {
        return Ok(std::str::from_utf8(raw).map_err(|_| PATH)?.to_owned());
    }
    Ok(raw
        .iter()
        .map(|byte| match byte.checked_sub(128) {
            Some(high) => CP437[usize::from(high)],
            None => char::from(*byte),
        })
        .collect())
}

/// The name a Unicode path field gives the entry, when the field is bound to
/// the header's own name by its checksum.
fn renamed(
    raw: &[u8],
    mut fields: rawzip::extra_fields::ExtraFields<'_>,
) -> Result<Option<String>> {
    let mut name = None;
    for (kind, data) in fields.by_ref() {
        if kind != ExtraFieldId::INFO_ZIP_UNICODE_PATH {
            continue;
        }
        let (bound, text) = data.split_at_checked(5).ok_or(FORMAT)?;
        let checksum = u32::from_le_bytes([bound[1], bound[2], bound[3], bound[4]]);
        if bound[0] == 1 && checksum == rawzip::crc32(raw) {
            let text = std::str::from_utf8(text).map_err(|_| FORMAT)?;
            if !text.is_empty() {
                name = Some(text.to_owned());
            }
        }
    }
    require(fields.remaining_bytes().len() < 4, FORMAT.0)?;
    Ok(name)
}

struct Member {
    /// The header's name as it reads; the local header must read the same.
    name: String,
    size: u64,
    method: CompressionMethod,
    flags: u16,
    kind: u32,
    location: ZipArchiveEntryWayfinder,
}

fn wheel(asset: &Asset, archive: &[u8], limits: &Limits) -> Result<Selected> {
    // Any header's three variable fields fit this buffer.
    let mut buffer = vec![0_u8; 256 * 1024];
    let zip = ZipArchive::from_seekable(Cursor::new(archive), &mut buffer).map_err(|_| FORMAT)?;
    let mut members = Vec::new();
    let mut entries = zip.entries(&mut buffer);
    while let Some(entry) = entries.next_entry().map_err(|_| FORMAT)? {
        require(members.len() < limits.members, "tool_archive_entry_count")?;
        let raw = entry.file_path();
        let name = decoded(raw.as_bytes(), entry.flags().bits() & 0x800 != 0)?;
        // The entry is known by its Unicode path field when it has one, and
        // only up to a NUL. A name that reads differently that way is another
        // spelling of the entry and is not admitted.
        let spelled = renamed(raw.as_bytes(), entry.extra_fields())?;
        let known = spelled.as_deref().unwrap_or(&name).split('\0').next();
        let plain = known == Some(name.as_str());
        members.push((
            plain,
            Member {
                name,
                size: entry.uncompressed_size_hint(),
                method: entry.compression_method(),
                flags: entry.flags().bits(),
                kind: (entry.external_attributes() >> 16) & 0o170_000,
                location: entry.wayfinder(),
            },
        ));
    }
    let mut names = BTreeSet::new();
    let mut found = Vec::new();
    let mut total = 0_u64;
    for (plain, member) in &members {
        let directory = member.name.ends_with('/');
        let name = if directory {
            member.name.trim_end_matches('/')
        } else {
            &member.name
        };
        require(*plain && safe_name(name), PATH.0)?;
        require(names.insert(name), "tool_archive_duplicate")?;
        require(
            matches!(member.kind, 0 | 0o100_000) || (member.kind == 0o040_000 && directory),
            "tool_archive_entry_type",
        )?;
        require(
            member.flags & 1 == 0
                && matches!(
                    member.method,
                    CompressionMethod::STORE | CompressionMethod::DEFLATE
                ),
            COMPRESSION.0,
        )?;
        require(member.size <= limits.executable, "tool_archive_entry_size")?;
        total = total.saturating_add(member.size);
        require(total <= limits.unpacked, "tool_archive_expansion")?;
        if let Some(destination) = asset.files.get(&member.name) {
            require(!directory && member.size > 0, "tool_executable_type")?;
            let content = content(&zip, member, &mut buffer, limits)?;
            found.push((destination.clone(), content));
        }
    }
    Ok(found)
}

/// One selected member's bytes. Its local header must name it, and the reader
/// fails unless they have the recorded size and CRC-32.
fn content<R: rawzip::ReaderAt>(
    zip: &ZipArchive<R>,
    member: &Member,
    buffer: &mut [u8],
    limits: &Limits,
) -> Result<Vec<u8>> {
    let entry = zip.get_entry(member.location).map_err(|_| FORMAT)?;
    let local = entry.local_header(buffer).map_err(|_| FORMAT)?;
    let name = decoded(
        local.file_path().as_bytes(),
        local.flags().bits() & 0x800 != 0,
    )?;
    require(name == member.name, FORMAT.0)?;
    require(member.flags & 0x60 == 0, COMPRESSION.0)?;
    let unpacked: Box<dyn Read + '_> = match member.method {
        CompressionMethod::DEFLATE => Box::new(flate2::read::DeflateDecoder::new(entry.reader())),
        _ => Box::new(entry.reader()),
    };
    let mut content = Vec::new();
    entry
        .verifying_reader(unpacked)
        .take(limits.executable.saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|_| FORMAT)?;
    Ok(content)
}
