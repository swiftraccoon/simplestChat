//! Validate archives before CodeQL sees them; never extract cache members here.

use super::{io, require};
use crate::ci::inputs;
use anyhow::Result;
use rawzip::{CompressionMethod, FileReader, ZipArchive, ZipArchiveEntryWayfinder};
use regex::Regex;
use serde_json::Map;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

pub(super) const MAX_BUNDLE: u64 = 4 * 1024 * 1024 * 1024;
const MAX_MEMBERS: usize = 100_000;
const MAX_SOURCE: u64 = 16 * 1024 * 1024;
const REQUIRED: &[&str] = &[
    "vendor/mediasoup-sys-0.19.0/src/RTC/DtlsTransport.cpp",
    "vendor/mediasoup-sys-0.19.0/src/RTC/ICE/StunPacket.cpp",
    "vendor/mediasoup-sys-0.19.0/src/RTC/SCTP/association/Association.cpp",
    "vendor/mediasoup-sys-0.19.0/src/RTC/RTP/Packet.cpp",
];

// Standard IBM CP437 high bytes, not a locale or UTF-8 heuristic. Python's ZIP
// reader uses this table unless the UTF-8 flag is set. Matching UnicodePath
// metadata is applied separately, retaining this name for local-header checks.
static CP437: LazyLock<Vec<char>> = LazyLock::new(|| {
    concat!(
        "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
        "áíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
        "└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
        "αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ",
    )
    .chars()
    .collect()
});

fn filename(raw: &[u8], utf8: bool) -> Result<String> {
    require(!raw.contains(&0), "codeql_cache_archive_path")?;
    if utf8 {
        return Ok(std::str::from_utf8(raw)?.to_owned());
    }
    Ok(raw
        .iter()
        .map(|byte| {
            if *byte < 128 {
                char::from(*byte)
            } else {
                CP437[usize::from(*byte - 128)]
            }
        })
        .collect())
}

fn canonical(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.chars().any(|character| character < ' ')
        && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

struct Member {
    name: String,
    original_name: String,
    directory: bool,
    size: u64,
    method: CompressionMethod,
    location: ZipArchiveEntryWayfinder,
    header: u64,
    flags: u16,
}

fn unicode_path(
    original: &str,
    raw: &[u8],
    mut fields: rawzip::extra_fields::ExtraFields<'_>,
) -> Result<String> {
    let mut name = original.to_owned();
    for (kind, data) in fields.by_ref() {
        if kind != rawzip::extra_fields::ExtraFieldId::INFO_ZIP_UNICODE_PATH {
            continue;
        }
        require(data.len() >= 5, "codeql_cache_archive_path")?;
        let crc = u32::from_le_bytes(data[1..5].try_into().expect("checked five-byte prefix"));
        if data[0] == 1 && crc == rawzip::crc32(raw) {
            let value = std::str::from_utf8(&data[5..])?;
            require(!value.contains('\0'), "codeql_cache_archive_path")?;
            if !value.is_empty() {
                name = value.to_owned();
            }
        }
    }
    require(
        fields.remaining_bytes().len() < 4,
        "codeql_cache_archive_path",
    )?;
    Ok(name)
}

/// Walk central records before allocating each bounded member. Keep decoded
/// paths rather than a raw-name index: identical bytes with different encoding
/// flags can name distinct files in the Python reference.
fn open(path: &Path) -> Result<(ZipArchive<FileReader>, Vec<Member>)> {
    let file = io::regular(path, MAX_BUNDLE, true)?;
    // Any complete header's three u16-sized variable fields fit this buffer.
    let mut buffer = vec![0_u8; 256 * 1024];
    let archive = ZipArchive::from_file(file, &mut buffer)?;
    require(
        archive.entries_hint() <= MAX_MEMBERS as u64,
        "codeql_cache_archive_members",
    )?;
    let mut entries = archive.entries(&mut buffer);
    let mut names = BTreeSet::new();
    let mut members = Vec::new();
    let mut total = 0_u64;
    while let Some(entry) = entries.next_entry()? {
        require(members.len() < MAX_MEMBERS, "codeql_cache_archive_members")?;
        let original_name = filename(
            entry.file_path().as_bytes(),
            entry.flags().bits() & 0x800 != 0,
        )?;
        let name = unicode_path(
            &original_name,
            entry.file_path().as_bytes(),
            entry.extra_fields(),
        )?;
        let normalized = name.trim_end_matches('/');
        let mode = (entry.external_attributes() >> 16) & 0o170000;
        require(
            canonical(normalized)
                && names.insert(normalized.to_owned())
                && !entry.flags().is_encrypted()
                && matches!(mode, 0 | 0o100000 | 0o040000),
            "codeql_cache_archive_path",
        )?;
        total = total
            .checked_add(entry.uncompressed_size_hint())
            .ok_or_else(|| anyhow::anyhow!("codeql_cache_archive_size"))?;
        require(total <= MAX_BUNDLE, "codeql_cache_archive_size")?;
        members.push(Member {
            directory: name.ends_with('/'),
            name,
            original_name,
            size: entry.uncompressed_size_hint(),
            method: entry.compression_method(),
            location: entry.wayfinder(),
            header: entry.local_header_offset(),
            flags: entry.flags().bits(),
        });
    }
    require(!members.is_empty(), "codeql_cache_archive_members")?;
    Ok((archive, members))
}

pub(super) fn bundle_integrity(path: &Path, language: &str) -> Result<()> {
    let (_, members) = open(path)?;
    let mut results = 0;
    for member in members {
        require(
            member.name.split('/').next() == Some(language) && !member.name.ends_with(".sarif"),
            "codeql_cache_bundle_content",
        )?;
        if member.name.split('/').any(|part| part == "results") && member.name.ends_with(".bqrs") {
            results += 1;
        }
    }
    require(results > 0, "codeql_cache_query_results_missing")
}

pub(super) fn source_integrity(
    database: &Path,
    source: &Path,
    manifest: &Map<String, Value>,
    language: &str,
) -> Result<()> {
    let metadata = io::bounded(&database.join("codeql-database.yml"), 65536)?;
    let expression = Regex::new(r"(?m)^\s*sourceLocationPrefix:\s*([^\n]+)$")?;
    let locations = expression
        .captures_iter(std::str::from_utf8(&metadata)?)
        .map(|capture| capture[1].to_owned())
        .collect::<Vec<_>>();
    require(
        locations
            == [source
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("codeql_cache_source_path"))?],
        "codeql_metadata_sourceLocationPrefix",
    )?;
    let (archive, members) = open(&database.join("src.zip"))?;
    let prefix = format!(
        "{}/",
        source
            .to_str()
            .expect("validated UTF-8")
            .trim_start_matches('/')
    );
    let mut header_buffer = vec![0_u8; 256 * 1024];
    let mut boundaries = members
        .iter()
        .map(|member| member.header)
        .collect::<BTreeSet<_>>();
    boundaries.insert(archive.directory_offset());
    let mut observed = BTreeSet::new();
    for member in &members {
        if member.directory {
            continue;
        }
        let Some(name) = member.name.strip_prefix(&prefix) else {
            continue;
        };
        require(
            !inputs::CODEQL_POLICY_DATA.contains(&name)
                && (!name.starts_with("web/")
                    || (language == "rust" && inputs::rust_codeql_input(name))),
            "codeql_cache_unkeyed_source",
        )?;
        if let Some(expected) = manifest.get(name) {
            require(
                if language == "c-cpp" {
                    inputs::native_codeql_input(name)
                } else {
                    inputs::rust_codeql_input(name)
                },
                "codeql_cache_unkeyed_source",
            )?;
            require(member.size <= MAX_SOURCE, "codeql_cache_source_size")?;
            let entry = archive.get_entry(member.location)?;
            let local = entry.local_header(&mut header_buffer)?;
            require(
                filename(
                    local.file_path().as_bytes(),
                    local.flags().bits() & 0x800 != 0,
                )? == member.original_name
                    && member.flags & 0x60 == 0,
                "codeql_cache_source_header",
            )?;
            let end = boundaries
                .range((
                    std::ops::Bound::Excluded(member.header),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .ok_or_else(|| anyhow::anyhow!("codeql_cache_source_header"))?;
            require(
                entry.compressed_data_range().1 <= *end,
                "codeql_cache_source_overlap",
            )?;
            let decompressor: Box<dyn Read + '_> = match member.method {
                CompressionMethod::STORE => Box::new(entry.reader()),
                CompressionMethod::DEFLATE => {
                    Box::new(flate2::read::DeflateDecoder::new(entry.reader()))
                }
                _ => anyhow::bail!("codeql_cache_source_compression"),
            };
            let mut reader = entry.verifying_reader(decompressor).take(MAX_SOURCE + 1);
            let mut hash = Sha256::new();
            let mut buffer = [0_u8; 65536];
            let mut size = 0_u64;
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                size += count as u64;
                hash.update(&buffer[..count]);
            }
            require(size == member.size, "codeql_cache_source_size")?;
            require(
                expected.as_str() == Some(io::hex(&hash.finalize()).as_str()),
                "codeql_cache_source_changed",
            )?;
            observed.insert(name);
        }
    }
    let required = if language == "c-cpp" {
        REQUIRED
    } else {
        &["src/main.rs"]
    };
    require(
        required.iter().all(|name| observed.contains(name)),
        "codeql_cache_source_coverage",
    )
}
