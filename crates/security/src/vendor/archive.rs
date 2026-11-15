//! The members of an upstream archive, read in memory after its digest was
//! checked and never written anywhere. Only files and directories are
//! members: a link, a device or anything else refuses the archive, and so
//! do a name that is no canonical path, a name given twice, a file where
//! another member has a directory, and more bytes than the budget.

use super::{
    Bounds, Verdict,
    compare::Files,
    manifest::{Source, relative_path},
    problem,
};
use crate::{
    digest,
    tools::{decoded, renamed},
};
use rawzip::{CompressionMethod, ZipArchive};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
};

/// What an archive has been found to hold so far.
struct Budget {
    bounds: Bounds,
    seen: BTreeSet<String>,
    files: Files,
    size: u64,
}

impl Budget {
    fn new(bounds: &Bounds) -> Self {
        Self {
            bounds: *bounds,
            seen: BTreeSet::new(),
            files: Files::new(),
            size: 0,
        }
    }

    /// Admit the member `name` of `size` bytes and answer its path.
    fn reserve(&mut self, name: &str, size: u64, directory: bool) -> Verdict<String> {
        let name = if directory {
            name.strip_suffix('/').unwrap_or(name)
        } else {
            name
        };
        let key = relative_path(name)?.to_owned();
        if self.seen.contains(&key) || self.seen.len() >= self.bounds.entries {
            return Err(problem("Duplicate or excessive archive members"));
        }
        let above = key
            .match_indices('/')
            .any(|(at, _)| self.files.contains_key(&key[..at]));
        let below = format!("{key}/");
        let inside = !directory
            && self
                .seen
                .iter()
                .any(|existing| existing.starts_with(&below));
        if above || inside {
            return Err(problem("Archive file/directory collision"));
        }
        self.seen.insert(key.clone());
        self.size = self.size.saturating_add(size);
        if size > self.bounds.member || self.size > self.bounds.content {
            return Err(problem("Archive expansion budget exceeded"));
        }
        Ok(key)
    }
}

/// Every member of a gzip stream, unpacked whole: one member after another,
/// with nothing after the last but zero bytes, and no more than `most`
/// bytes together, or `budget` is what is wrong.
pub(crate) fn unpacked(data: &[u8], most: u64, budget: &str) -> Verdict<Vec<u8>> {
    let unreadable = || problem("Unreadable gzip stream");
    let limit = most + 1;
    let mut expanded = Vec::new();
    let mut rest = data;
    if rest.is_empty() {
        return Err(unreadable());
    }
    while !rest.is_empty() {
        let mut member = flate2::bufread::GzDecoder::new(rest);
        let room = limit - expanded.len() as u64;
        let read = (&mut member).take(room).read_to_end(&mut expanded);
        read.map_err(|_| unreadable())?;
        if expanded.len() as u64 >= limit {
            return Err(problem(budget));
        }
        rest = member.into_inner();
        let padding = rest.iter().take_while(|byte| **byte == 0).count();
        rest = &rest[padding..];
    }
    Ok(expanded)
}

/// What a member of a tar archive is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A directory.
    Directory,
    /// A file with bytes of its own.
    File,
    /// A link, a device, a pipe or anything else.
    Other,
}

/// What a walk over a tar archive does with one member: its name, what it
/// is, the bytes it says it has, and those bytes.
pub(crate) type Visit<'a> = dyn FnMut(&str, Kind, u64, &mut dyn Read) -> Verdict<()> + 'a;

/// Walk the members of the unpacked tar archive `expanded` in their order.
/// A directory is named without the slashes at its end, and a header that
/// speaks for the whole archive is passed over.
pub(crate) fn tar_each(expanded: &[u8], visit: &mut Visit<'_>) -> Verdict<()> {
    let unreadable = || problem("Unreadable tar archive");
    if expanded.is_empty() {
        return Err(unreadable());
    }
    let mut archive = tar::Archive::new(Cursor::new(expanded));
    for entry in archive.entries().map_err(|_| unreadable())? {
        let mut entry = entry.map_err(|_| unreadable())?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        let path = entry.path_bytes().into_owned();
        let name = std::str::from_utf8(&path).unwrap_or("\u{fffd}");
        let directory =
            kind.is_dir() || (entry.header().as_bytes()[156] == 0 && name.ends_with('/'));
        let (kind, name) = if directory {
            (Kind::Directory, name.trim_end_matches('/'))
        } else if kind.is_file() || kind.is_contiguous() {
            (Kind::File, name)
        } else {
            (Kind::Other, name)
        };
        let size = entry.size();
        visit(name, kind, size, &mut entry)?;
    }
    Ok(())
}

/// The files of a gzip tar archive.
fn tar_members(data: &[u8], bounds: &Bounds) -> Verdict<Files> {
    let budget = "Archive expansion budget exceeded";
    let expanded = unpacked(data, bounds.content, budget)?;
    let mut budget = Budget::new(bounds);
    tar_each(&expanded, &mut |name, kind, size, content| {
        if kind == Kind::Other {
            return Err(problem("Unsupported tar member type"));
        }
        let directory = kind == Kind::Directory;
        let key = budget.reserve(name, size, directory)?;
        if directory {
            if size != 0 {
                return Err(problem("Nonempty archive directory"));
            }
            return Ok(());
        }
        let mut body = Vec::new();
        let read = content.read_to_end(&mut body);
        read.map_err(|_| problem("Unreadable tar archive"))?;
        if body.len() as u64 != size {
            return Err(problem("Tar member size differs"));
        }
        budget.files.insert(key, body);
        Ok(())
    })?;
    Ok(budget.files)
}

/// What the central directory says of one member.
struct Member {
    raw: Vec<u8>,
    name: String,
    plain: bool,
    size: u64,
    method: CompressionMethod,
    flags: u16,
    kind: u32,
    location: rawzip::ZipArchiveEntryWayfinder,
}

/// The files of a zip archive: stored or deflated, not encrypted, and
/// files or directories by their mode when they have one. A member is
/// known by one name: one that reads differently by its Unicode path
/// field, or up to a zero byte, is refused.
fn zip_members(data: &[u8], bounds: &Bounds) -> Verdict<Files> {
    let unreadable = || problem("Unreadable zip archive");
    let mut buffer = vec![0_u8; 256 * 1024];
    let zip = ZipArchive::from_seekable(Cursor::new(data), &mut buffer);
    let zip = zip.map_err(|_| unreadable())?;
    let mut members = Vec::new();
    let mut entries = zip.entries(&mut buffer);
    while let Some(entry) = entries.next_entry().map_err(|_| unreadable())? {
        let raw = entry.file_path().as_bytes().to_vec();
        let flags = entry.flags().bits();
        let name = decoded(&raw, flags & 0x800 != 0).map_err(|_| unreadable())?;
        let spelled = renamed(&raw, entry.extra_fields()).map_err(|_| unreadable())?;
        let known = spelled.as_deref().unwrap_or(&name).split('\0').next();
        members.push(Member {
            plain: known == Some(name.as_str()),
            raw,
            name,
            size: entry.uncompressed_size_hint(),
            method: entry.compression_method(),
            flags,
            kind: (entry.external_attributes() >> 16) & 0o170_000,
            location: entry.wayfinder(),
        });
    }
    let mut budget = Budget::new(bounds);
    for member in &members {
        if !member.plain {
            return Err(problem("Ambiguous zip member name"));
        }
        let packed = matches!(
            member.method,
            CompressionMethod::STORE | CompressionMethod::DEFLATE
        );
        if member.flags & 1 != 0 || !matches!(member.kind, 0 | 0o100_000 | 0o040_000) || !packed {
            return Err(problem("Unsupported zip member type"));
        }
        let directory = member.name.ends_with('/');
        if member.kind != 0 && directory != (member.kind == 0o040_000) {
            return Err(problem("Ambiguous zip directory"));
        }
        let key = budget.reserve(&member.name, member.size, directory)?;
        if directory {
            if member.size != 0 {
                return Err(problem("Nonempty archive directory"));
            }
            continue;
        }
        let entry = zip.get_entry(member.location).map_err(|_| unreadable())?;
        let local = entry.local_header(&mut buffer).map_err(|_| unreadable())?;
        if local.file_path().as_bytes() != member.raw {
            return Err(unreadable());
        }
        let content: Box<dyn Read + '_> = match member.method {
            CompressionMethod::DEFLATE => {
                Box::new(flate2::read::DeflateDecoder::new(entry.reader()))
            }
            _ => Box::new(entry.reader()),
        };
        // The reader fails as soon as a member holds more than it says or
        // other bytes; the bound and the length are held here as well, so
        // that neither rests on how that reader goes about it.
        let mut body = Vec::new();
        let checked = entry.verifying_reader(content);
        let read = checked.take(bounds.member + 1).read_to_end(&mut body);
        read.map_err(|_| unreadable())?;
        if body.len() as u64 != member.size {
            return Err(problem("Zip member size differs"));
        }
        budget.files.insert(key, body);
    }
    Ok(budget.files)
}

/// The files of the archive `data`, which must be the bytes that `source`
/// pins: the digest is held before anything reads the archive.
pub fn files(source: &Source, data: &[u8], bounds: &Bounds) -> Verdict<Files> {
    if data.len() > bounds.source || digest(data) != source.sha256 {
        return Err(problem(format!(
            "Source SHA-256 or size differs: {}",
            source.identifier
        )));
    }
    match source.format.as_str() {
        "tar.gz" => tar_members(data, bounds),
        "zip" => zip_members(data, bounds),
        _ => Err(problem("A raw file cannot be parsed as an archive")),
    }
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
