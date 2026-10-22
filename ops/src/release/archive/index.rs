//! Two mature tar passes: raw entries bound extension allocation first, then
//! interpreted entries supply effective paths and offsets. No unpack API is used.

use super::{ArchiveError as E, Limits, Result, checkpoint};
use crate::json::{self, Json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};

pub(super) struct Member {
    offset: u64,
    pub(super) size: u64,
    pub(super) file: bool,
    trailing_slash: bool,
}
pub(super) struct Index {
    members: BTreeMap<String, Member>,
    metadata: u64,
    extent: u64,
}

/// Match PurePosixPath canonicality without asking the host OS to resolve a tar
/// name. Trailing slashes normalize for both files and directories; internal
/// empty/dot/parent components and leading slashes are refused.
pub(super) fn member_name(name: &str) -> Result<&str> {
    if name.is_empty() || name.contains('\\') || name.chars().any(|c| u32::from(c) < 32) {
        return Err(E::Path);
    }
    let normalized = name.trim_end_matches('/');
    if normalized.is_empty()
        || name.starts_with('/')
        || normalized
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(E::NoncanonicalPath);
    }
    Ok(normalized)
}

fn bounds(offset: u64, size: u64, total: u64) -> Result<()> {
    let padded = size.checked_add(511).ok_or(E::Read)? / 512 * 512;
    if offset.checked_add(padded).is_none_or(|end| end > total) {
        return Err(E::Read);
    }
    Ok(())
}

fn preflight(
    file: &mut (impl Read + Seek),
    total: u64,
    limits: Limits,
    check: &mut impl FnMut() -> bool,
) -> Result<u64> {
    let mut archive = tar::Archive::new(file);
    let entries = archive.entries_with_seek().map_err(|_| E::Read)?.raw(true);
    let mut pending: Option<Option<u64>> = None;
    let mut extent = 0;
    for (number, entry) in entries.enumerate() {
        checkpoint(check)?;
        if number >= limits.headers {
            return Err(E::PhysicalCount);
        }
        let mut entry = entry.map_err(|_| E::Read)?;
        bounds(entry.raw_file_position(), entry.size(), total)?;
        extent = entry.raw_file_position() + entry.size().div_ceil(512) * 512;
        let kind = entry.header().entry_type();
        if kind.is_pax_local_extensions() || kind.is_gnu_longname() {
            if entry.header().as_gnu().is_none() && entry.header().as_ustar().is_none() {
                return Err(E::UnsupportedTar);
            }
            if pending.is_some() {
                return Err(E::UnsupportedTar);
            }
            if entry.size() > limits.extension {
                return Err(E::ExtensionBudget);
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(entry.size() as usize)
                .map_err(|_| E::ExtensionBudget)?;
            let expected = entry.size();
            entry.read_to_end(&mut bytes).map_err(|_| E::Read)?;
            if bytes.len() as u64 != expected {
                return Err(E::Read);
            }
            let mut size = None;
            if kind.is_pax_local_extensions() {
                // PaxExtensions ends at an empty line; refuse hidden records
                // after it instead of accepting a differently interpreted body.
                if bytes.first() == Some(&b'\n') || bytes.windows(2).any(|w| w == b"\n\n") {
                    return Err(E::UnsupportedTar);
                }
                let mut keys = BTreeSet::new();
                for field in tar::PaxExtensions::new(&bytes) {
                    let field = field.map_err(|_| E::UnsupportedTar)?;
                    let key = field.key().map_err(|_| E::UnsupportedTar)?;
                    let value = field.value().map_err(|_| E::UnsupportedTar)?;
                    if key.is_empty()
                        || !keys.insert(key.to_owned())
                        || key.starts_with("GNU.sparse")
                    {
                        return Err(E::UnsupportedTar);
                    }
                    if key == "size" {
                        size = Some(value.parse::<u64>().map_err(|_| E::UnsupportedTar)?);
                    }
                }
            } else {
                std::str::from_utf8(bytes.strip_suffix(&[0]).unwrap_or(&bytes))
                    .map_err(|_| E::UnsupportedTar)?;
            }
            pending = Some(size);
        } else {
            if kind.is_gnu_sparse() || kind.is_pax_global_extensions() || kind.is_gnu_longlink() {
                return Err(E::UnsupportedTar);
            }
            if kind.is_dir() && entry.size() != 0 {
                return Err(E::UnsupportedTar);
            }
            if let Some(Some(size)) = pending.take() {
                // Raw iteration and normal PAX interpretation must agree on the
                // next header location. Size-changing PAX is outside our domain.
                if size != entry.size() {
                    return Err(E::UnsupportedTar);
                }
            }
        }
    }
    if pending.is_some() {
        return Err(E::UnsupportedTar);
    }
    checkpoint(check)?;
    Ok(extent)
}

impl Index {
    pub(super) fn read(file: &mut (impl Read + Seek), total: u64, limits: Limits) -> Result<Self> {
        Self::read_with_checkpoint(file, total, limits, &mut || true)
    }
    pub(super) fn read_with_checkpoint(
        file: &mut (impl Read + Seek),
        total: u64,
        limits: Limits,
        check: &mut impl FnMut() -> bool,
    ) -> Result<Self> {
        checkpoint(check)?;
        let extent = preflight(file, total, limits, check)?;
        file.rewind().map_err(|_| E::Read)?;
        let mut archive = tar::Archive::new(file);
        let entries = archive.entries_with_seek().map_err(|_| E::Read)?;
        let mut members = BTreeMap::new();
        let mut names = 0_usize;
        for entry in entries {
            checkpoint(check)?;
            let entry = entry.map_err(|_| E::Read)?;
            let bytes = entry.path_bytes();
            let name = std::str::from_utf8(&bytes).map_err(|_| E::UnsupportedTar)?;
            // Python gives the old V7 null-type/trailing-slash combination a
            // special directory meaning that tar-rs does not expose as a type.
            if entry.header().as_bytes()[156] == 0 && name.ends_with('/') {
                return Err(E::UnsupportedTar);
            }
            let trailing_slash = name.ends_with('/');
            let name = member_name(name)?;
            if name.len() > limits.name {
                return Err(E::NameBudget);
            }
            names = names.checked_add(name.len()).ok_or(E::NameBudget)?;
            if names > limits.names {
                return Err(E::NameBudget);
            }
            let kind = entry.header().entry_type();
            if members.contains_key(name)
                || !(kind.is_file() || kind.is_contiguous() || kind.is_dir())
            {
                return Err(E::Member);
            }
            bounds(entry.raw_file_position(), entry.size(), total)?;
            members.insert(
                name.into(),
                Member {
                    offset: entry.raw_file_position(),
                    size: entry.size(),
                    file: kind.is_file() || kind.is_contiguous(),
                    trailing_slash,
                },
            );
            if members.len() > limits.members {
                return Err(E::MemberCount);
            }
        }
        Ok(Self {
            members,
            metadata: limits.metadata,
            extent,
        })
    }
    pub(super) fn has(&self, name: &str) -> bool {
        self.members.contains_key(name)
    }
    pub(super) fn member(&self, name: &str) -> Option<&Member> {
        self.members.get(name)
    }
    pub(super) fn read_bytes(&self, file: &mut (impl Read + Seek), name: &str) -> Result<Vec<u8>> {
        let member = self.members.get(member_name(name)?).ok_or(E::Metadata)?;
        if !member.file || member.size == 0 || member.size > self.metadata {
            return Err(E::Metadata);
        }
        let size = usize::try_from(member.size).map_err(|_| E::Metadata)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(|_| E::Metadata)?;
        bytes.resize(size, 0);
        file.seek(SeekFrom::Start(member.offset))
            .map_err(|_| E::Read)?;
        file.read_exact(&mut bytes).map_err(|_| E::Read)?;
        Ok(bytes)
    }
    pub(super) fn read_json(&self, file: &mut (impl Read + Seek), name: &str) -> Result<Json> {
        Ok(json::decode(&self.read_bytes(file, name)?)?)
    }

    /// New generator-only domain: file names are literal, parent file conflicts
    /// are refused, and exactly terminal zero blocks/padding follow the last raw
    /// entry. The signed-server profile does not call this additional policy.
    pub(super) fn generator_profile(
        &self,
        file: &mut (impl Read + Seek),
        total: u64,
        check: &mut impl FnMut() -> bool,
    ) -> Result<()> {
        if !total.is_multiple_of(512) || total.checked_sub(self.extent).is_none_or(|n| n < 1024) {
            return Err(E::UnsupportedTar);
        }
        for (name, member) in &self.members {
            checkpoint(check)?;
            if member.file && member.trailing_slash {
                return Err(E::NoncanonicalPath);
            }
            let mut parent = name.as_str();
            while let Some((next, _)) = parent.rsplit_once('/') {
                if self.members.get(next).is_some_and(|member| member.file) {
                    return Err(E::Member);
                }
                parent = next;
            }
        }
        file.seek(SeekFrom::Start(self.extent))
            .map_err(|_| E::Read)?;
        let mut remaining = total - self.extent;
        let mut bytes = [0; 65536];
        while remaining != 0 {
            checkpoint(check)?;
            let count = remaining.min(bytes.len() as u64) as usize;
            file.read_exact(&mut bytes[..count]).map_err(|_| E::Read)?;
            if bytes[..count].iter().any(|byte| *byte != 0) {
                return Err(E::UnsupportedTar);
            }
            remaining -= count as u64;
        }
        checkpoint(check)
    }

    /// Hash the stored member bytes, never a metadata filename or descriptor.
    /// Large layers use a fixed buffer and cannot enter a metadata allocation.
    pub(super) fn hash_member(
        &self,
        file: &mut (impl Read + Seek),
        name: &str,
        check: &mut impl FnMut() -> bool,
    ) -> Result<(String, u64)> {
        let member = self
            .members
            .get(member_name(name)?)
            .filter(|m| m.file)
            .ok_or(E::Layer)?;
        checkpoint(check)?;
        file.seek(SeekFrom::Start(member.offset))
            .map_err(|_| E::Read)?;
        let mut digest = Sha256::new();
        let mut bytes = [0; 65536];
        let mut remaining = member.size;
        while remaining != 0 {
            checkpoint(check)?;
            let count = remaining.min(bytes.len() as u64) as usize;
            file.read_exact(&mut bytes[..count]).map_err(|_| E::Read)?;
            checkpoint(check)?;
            digest.update(&bytes[..count]);
            remaining -= count as u64;
        }
        Ok((
            digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            member.size,
        ))
    }
}
