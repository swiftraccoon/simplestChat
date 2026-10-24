//! Bounded raw/effective tar passes, without extraction. The raw pass admits
//! extension allocations before tar-rs interprets names. Every input descriptor
//! is checked and explicitly closed, including after a malformed capture.

use super::scope::{Mode, Scope};
use super::{MAX_FILE, MAX_FILES, MAX_TAR, MAX_TOTAL, Result, fail, model, require};
use crate::{files::Snapshot, release::attestation::control::Owner};
use std::{
    collections::BTreeSet,
    io::{Read, Seek},
    path::Path,
};

const MAX_HEADERS: usize = 1024;
const MAX_EXTENSION: u64 = 8192;
const MAX_NAMES: usize = 128 * 1024;
const MAX_PATH: usize = 7 + 255;

fn extent(offset: u64, size: u64, length: u64) -> Result<()> {
    let padded = size
        .checked_add(511)
        .and_then(|size| (size / 512).checked_mul(512))
        .and_then(|size| offset.checked_add(size));
    require(padded.is_some_and(|end| end <= length), "asset_tar_content")
}

fn preflight(input: &mut (impl Read + Seek), length: u64, scope: &Scope<'_>) -> Result<()> {
    scope.check()?;
    let mut archive = tar::Archive::new(input);
    let entries = archive
        .entries_with_seek()
        .map_err(|_| fail("asset_tar_content"))?
        .raw(true);
    let mut pending: Option<Option<u64>> = None;
    let mut members = 0_usize;
    let mut payload = 0_u64;
    for (number, entry) in entries.enumerate() {
        scope.check()?;
        require(number < MAX_HEADERS, "asset_tar_header_count")?;
        let mut entry = entry.map_err(|_| fail("asset_tar_content"))?;
        extent(entry.raw_file_position(), entry.size(), length)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_local_extensions() || kind.is_gnu_longname() {
            require(
                pending.is_none()
                    && (entry.header().as_ustar().is_some() || entry.header().as_gnu().is_some()),
                "asset_tar_unsupported",
            )?;
            require(entry.size() <= MAX_EXTENSION, "asset_tar_extension")?;
            let expected = entry.size();
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(expected as usize)
                .map_err(|_| fail("asset_tar_extension"))?;
            entry
                .read_to_end(&mut bytes)
                .map_err(|_| fail("asset_tar_content"))?;
            require(bytes.len() as u64 == expected, "asset_tar_content")?;
            let mut size = None;
            if kind.is_pax_local_extensions() {
                require(
                    bytes.first() != Some(&b'\n')
                        && !bytes.windows(2).any(|window| window == b"\n\n"),
                    "asset_tar_unsupported",
                )?;
                let mut keys = BTreeSet::new();
                for field in tar::PaxExtensions::new(&bytes) {
                    let field = field.map_err(|_| fail("asset_tar_unsupported"))?;
                    let key = field.key().map_err(|_| fail("asset_tar_unsupported"))?;
                    let value = field.value().map_err(|_| fail("asset_tar_unsupported"))?;
                    require(
                        !key.is_empty()
                            && !key.starts_with("GNU.sparse")
                            && keys.insert(key.to_owned()),
                        "asset_tar_unsupported",
                    )?;
                    if key == "size" {
                        size = Some(
                            value
                                .parse::<u64>()
                                .map_err(|_| fail("asset_tar_unsupported"))?,
                        );
                    }
                }
            } else {
                let name = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
                require(!name.contains(&0), "asset_tar_unsupported")?;
                std::str::from_utf8(name).map_err(|_| fail("asset_tar_unsupported"))?;
            }
            pending = Some(size);
            continue;
        }
        require(
            kind.is_file() || kind.is_contiguous() || kind.is_dir(),
            "asset_tar_type",
        )?;
        if let Some(Some(size)) = pending.take() {
            require(size == entry.size(), "asset_tar_unsupported")?;
        }
        members += 1;
        require(members <= MAX_FILES + 1, "asset_tar_count")?;
        if kind.is_dir() {
            require(entry.size() == 0, "asset_tar_unsupported")?;
        } else {
            require(
                entry.size() > 0 && entry.size() <= MAX_FILE,
                "asset_tar_file_size",
            )?;
            payload = payload
                .checked_add(entry.size())
                .ok_or_else(|| fail("asset_tar_total"))?;
            require(payload <= MAX_TOTAL, "asset_tar_total")?;
        }
    }
    require(pending.is_none(), "asset_tar_unsupported")
}

fn parse_scoped(
    input: &mut (impl Read + Seek),
    length: u64,
    scope: &Scope<'_>,
) -> Result<model::Capture> {
    preflight(input, length, scope)?;
    input.rewind().map_err(|_| fail("asset_tar_content"))?;
    let mut archive = tar::Archive::new(input);
    let entries = archive
        .entries_with_seek()
        .map_err(|_| fail("asset_tar_content"))?;
    let mut seen = BTreeSet::new();
    let mut names = 0_usize;
    let mut total = 0_u64;
    let mut assets = Vec::new();
    for entry in entries {
        scope.check()?;
        require(seen.len() <= MAX_FILES, "asset_tar_count")?;
        let mut entry = entry.map_err(|_| fail("asset_tar_content"))?;
        let bytes = entry.path_bytes();
        let original = std::str::from_utf8(&bytes).map_err(|_| fail("asset_tar_path"))?;
        let kind = entry.header().entry_type();
        // Python normalizes directory trailing slashes before duplicate checks.
        // Its obsolete null-type/slash special case is outside this profile.
        require(
            !(entry.header().as_bytes()[156] == 0 && original.ends_with('/')),
            "asset_tar_unsupported",
        )?;
        let name = if kind.is_dir() {
            original.trim_end_matches('/')
        } else {
            original
        };
        require(name.len() <= MAX_PATH, "asset_tar_names")?;
        names = names
            .checked_add(name.len())
            .ok_or_else(|| fail("asset_tar_names"))?;
        require(names <= MAX_NAMES, "asset_tar_names")?;
        require(seen.insert(name.to_owned()), "asset_tar_duplicate")?;
        if name == "assets" {
            require(kind.is_dir(), "asset_tar_root")?;
            continue;
        }
        let name = name
            .strip_prefix("assets/")
            .filter(|name| model::name(name))
            .ok_or_else(|| fail("asset_tar_path"))?
            .to_owned();
        require(
            (kind.is_file() || kind.is_contiguous())
                && entry.link_name_bytes().is_none_or(|name| name.is_empty()),
            "asset_tar_type",
        )?;
        require(
            entry.size() > 0 && entry.size() <= MAX_FILE,
            "asset_tar_file_size",
        )?;
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| fail("asset_tar_total"))?;
        require(
            total <= MAX_TOTAL && assets.len() < MAX_FILES,
            "asset_tar_total",
        )?;
        let mut content = Vec::new();
        content
            .try_reserve_exact(entry.size() as usize)
            .map_err(|_| fail("asset_tar_total"))?;
        content.resize(entry.size() as usize, 0);
        entry
            .read_exact(&mut content)
            .map_err(|_| fail("asset_tar_content_size"))?;
        assets.push((name, content));
    }
    require(!assets.is_empty(), "asset_tar_empty")?;
    Ok(model::Capture(assets))
}

struct OwnedStream<'a, 'flow, 'owner> {
    input: &'a mut Snapshot,
    scope: &'flow Scope<'owner>,
}
impl Read for OwnedStream<'_, '_, '_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let end = bytes.len().min(65536);
        self.input.read_checked(&mut bytes[..end], |event, value| {
            self.scope.snapshot(event, value)
        })
    }
}
impl Seek for OwnedStream<'_, '_, '_> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.input
            .seek_checked(position, |event, value| self.scope.snapshot(event, value))
    }
}

pub(super) fn read(path: &Path) -> Result<model::Capture> {
    read_scoped(path, &Scope::standalone())
}
pub(super) fn read_owned(path: &Path, owner: &mut Owner) -> Result<model::Capture> {
    read_scoped(path, &Scope::owned(owner, Mode::Body))
}
fn read_scoped(path: &Path, scope: &Scope<'_>) -> Result<model::Capture> {
    scope.check()?;
    let mut snapshot =
        Snapshot::open_checked(path, MAX_TAR, |event, value| scope.snapshot(event, value))
            .map_err(|_| fail("asset_tar_size"))?;
    let length = snapshot.length();
    let captured = parse_scoped(
        &mut OwnedStream {
            input: &mut snapshot,
            scope,
        },
        length,
        scope,
    );
    scope.observed(&captured);
    let closed = snapshot
        .finish_checked(|event, value| scope.snapshot(event, value))
        .map_err(|_| fail("asset_tar_changed"));
    scope.observed(&closed);
    let captured = captured?;
    closed?;
    scope.result(Ok(captured))
}

#[cfg(test)]
#[path = "assets_capture_tests.rs"]
mod tests;
