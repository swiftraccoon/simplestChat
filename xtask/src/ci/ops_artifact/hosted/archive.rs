//! A closed Rust-installer-v3 / Debian filesystem-tar projection. Mature gzip
//! and tar readers decode bytes; this layer bounds metadata and selected paths.
//! No archive installation script, package maintainer script or candidate runs.

use super::{
    catalog::{Artifact, Format},
    failure,
    files::{self, Tree},
};
use crate::tooling::Result;
use flate2::bufread::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
};

const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_HEADERS: usize = 65536;
const MAX_NAMES: usize = 16 * 1024 * 1024;
const MAX_METADATA: usize = 256 * 1024;

#[derive(Default)]
pub(super) struct Graph {
    links: BTreeMap<String, String>,
    entries: usize,
    names: usize,
    pub bytes: u64,
}

struct Limited<'a, R: ?Sized> {
    reader: &'a mut R,
    bytes: u64,
}
impl<R: Read + ?Sized> Read for Limited<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.bytes >= MAX_EXPANDED && !out.is_empty() {
            return Err(std::io::Error::other("closed archive limit"));
        }
        let maximum = out.len().min((MAX_EXPANDED - self.bytes) as usize);
        let n = self.reader.read(&mut out[..maximum])?;
        self.bytes += n as u64;
        Ok(n)
    }
}

fn decoded<T>(
    file: &File,
    format: Format,
    checkpoint: &mut impl FnMut() -> Result<()>,
    operation: impl FnOnce(&mut dyn Read, &mut dyn FnMut() -> Result<()>) -> Result<T>,
) -> Result<T> {
    let mut source = file;
    source.seek(SeekFrom::Start(0)).map_err(|_| failure(1))?;
    match format {
        Format::RustGzip => {
            let mut buffered = BufReader::new(source);
            let value = {
                let mut gzip = GzDecoder::new(&mut buffered);
                let mut limited = Limited {
                    reader: &mut gzip,
                    bytes: 0,
                };
                let value = operation(&mut limited, checkpoint)?;
                zero_tail(&mut limited, checkpoint)?;
                value
            };
            if !buffered.fill_buf().map_err(|_| failure(1))?.is_empty() {
                return Err(failure(1));
            }
            Ok(value)
        }
        Format::UbuntuDeb => {
            let mut limited = Limited {
                reader: &mut source,
                bytes: 0,
            };
            let value = operation(&mut limited, checkpoint)?;
            zero_tail(&mut limited, checkpoint)?;
            Ok(value)
        }
    }
}
fn zero_tail(
    reader: &mut (dyn Read + '_),
    checkpoint: &mut (dyn FnMut() -> Result<()> + '_),
) -> Result<()> {
    let mut block = [0u8; 65536];
    loop {
        checkpoint()?;
        let n = reader.read(&mut block).map_err(|_| failure(1))?;
        if n == 0 {
            return Ok(());
        }
        if block[..n].iter().any(|b| *b != 0) {
            return Err(failure(1));
        }
    }
}
fn discard(
    reader: &mut (dyn Read + '_),
    mut size: u64,
    checkpoint: &mut (dyn FnMut() -> Result<()> + '_),
) -> Result<()> {
    let mut block = [0u8; 65536];
    while size > 0 {
        checkpoint()?;
        let count = size.min(block.len() as u64) as usize;
        reader
            .read_exact(&mut block[..count])
            .map_err(|_| failure(1))?;
        size -= count as u64;
    }
    Ok(())
}
fn preflight(
    reader: &mut (dyn Read + '_),
    checkpoint: &mut (dyn FnMut() -> Result<()> + '_),
) -> Result<()> {
    let mut headers = 0usize;
    let mut extension = false;
    loop {
        checkpoint()?;
        let mut block = [0u8; 512];
        reader.read_exact(&mut block).map_err(|_| failure(1))?;
        if block == [0; 512] {
            if extension {
                return Err(failure(1));
            }
            reader.read_exact(&mut block).map_err(|_| failure(1))?;
            return if block == [0; 512] {
                zero_tail(reader, checkpoint)
            } else {
                Err(failure(1))
            };
        }
        headers += 1;
        if headers > MAX_HEADERS {
            return Err(failure(1));
        }
        let header = tar::Header::from_byte_slice(&block);
        let size = header.entry_size().map_err(|_| failure(1))?;
        match header.entry_type().as_byte() {
            0 | b'0' | b'5' | b'2' | b'1' => {
                extension = false;
                if size > files::MAX_FILE {
                    return Err(failure(1));
                }
            }
            b'L' | b'K' => {
                if extension || size == 0 || size > 8192 {
                    return Err(failure(1));
                }
                extension = true;
            }
            _ => return Err(failure(1)),
        }
        discard(
            reader,
            size.checked_add(511).ok_or_else(|| failure(1))? / 512 * 512,
            checkpoint,
        )?;
    }
}
fn name(raw: &[u8], directory: bool) -> Result<String> {
    let raw = std::str::from_utf8(raw).map_err(|_| failure(1))?;
    let raw = raw.strip_prefix("./").unwrap_or(raw);
    let raw = if directory {
        raw.trim_end_matches('/')
    } else {
        raw
    };
    if directory && raw.is_empty() {
        return Ok(String::new());
    }
    if !files::safe(raw) {
        return Err(failure(1));
    }
    Ok(raw.to_owned())
}
fn selected(artifact: &Artifact, raw: &str) -> Result<Option<String>> {
    match artifact.format {
        Format::RustGzip => {
            if raw == artifact.archive_root {
                return Ok(None);
            }
            let within = raw
                .strip_prefix(&format!("{}/", artifact.archive_root))
                .ok_or_else(|| failure(1))?;
            if within == artifact.component
                || within == format!("{}/manifest.in", artifact.component)
            {
                return Ok(None);
            }
            Ok(within
                .strip_prefix(&format!("{}/", artifact.component))
                .map(|p| format!("toolchain/{p}")))
        }
        Format::UbuntuDeb => {
            if [
                "usr/bin",
                "usr/lib",
                "usr/lib64",
                "usr/libexec",
                "usr/include",
                "lib",
                "lib64",
            ]
            .iter()
            .any(|prefix| {
                raw == *prefix || raw.strip_prefix(prefix).is_some_and(|v| v.starts_with('/'))
            }) {
                Ok(Some(format!("host-tools/sysroot/{raw}")))
            } else {
                Ok(None)
            }
        }
    }
}
fn target(artifact: &Artifact, destination: &str, raw: &str, hard: bool) -> Result<String> {
    if raw.is_empty() || raw.len() > 4096 || raw.contains(['\\', '\0', '\n', '\r']) {
        return Err(failure(1));
    }
    if hard {
        return selected(artifact, &name(raw.as_bytes(), false)?)?.ok_or_else(|| failure(1));
    }
    let base = match artifact.format {
        Format::RustGzip => "toolchain",
        Format::UbuntuDeb => "host-tools/sysroot",
    };
    let joined = if raw.starts_with('/') {
        if artifact.format == Format::RustGzip {
            return Err(failure(1));
        }
        format!("{base}/{}", raw.trim_start_matches('/'))
    } else {
        format!(
            "{}/{raw}",
            destination.rsplit_once('/').ok_or_else(|| failure(1))?.0
        )
    };
    let mut parts = Vec::new();
    for p in joined.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(failure(1));
                }
            }
            _ => parts.push(p),
        }
    }
    let normalized = parts.join("/");
    if !files::safe(&normalized) || !normalized.starts_with(&format!("{base}/")) {
        return Err(failure(1));
    }
    Ok(normalized)
}

impl Graph {
    pub fn import(
        &mut self,
        file: &File,
        artifact: &Artifact,
        tree: &mut Tree,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        decoded(file, artifact.format, checkpoint, preflight)?;
        let mut seen = BTreeSet::new();
        let mut metadata = BTreeMap::new();
        let mut installed = BTreeSet::new();
        decoded(file, artifact.format, checkpoint, |reader, checkpoint| {
            let mut archive = tar::Archive::new(reader);
            for entry in archive.entries().map_err(|_| failure(1))? {
                checkpoint()?;
                let mut entry = entry.map_err(|_| failure(1))?;
                let kind = entry.header().entry_type();
                let raw = name(&entry.path_bytes(), kind.is_dir())?;
                if !seen.insert(raw.clone()) {
                    return Err(failure(1));
                }
                self.entries = self.entries.checked_add(1).ok_or_else(|| failure(1))?;
                self.names = self
                    .names
                    .checked_add(raw.len())
                    .ok_or_else(|| failure(1))?;
                if self.entries > 131072 || self.names > MAX_NAMES {
                    return Err(failure(1));
                }
                if artifact.format == Format::RustGzip
                    && [
                        format!("{}/components", artifact.archive_root),
                        format!("{}/rust-installer-version", artifact.archive_root),
                        format!(
                            "{}/{}/manifest.in",
                            artifact.archive_root, artifact.component
                        ),
                    ]
                    .contains(&raw)
                {
                    if !kind.is_file() || entry.size() > MAX_METADATA as u64 {
                        return Err(failure(1));
                    }
                    let mut data = Vec::new();
                    entry
                        .take(MAX_METADATA as u64 + 1)
                        .read_to_end(&mut data)
                        .map_err(|_| failure(1))?;
                    metadata.insert(raw, data);
                    continue;
                }
                let Some(destination) = selected(artifact, &raw)? else {
                    continue;
                };
                if !files::safe(&destination) {
                    return Err(failure(1));
                }
                installed.insert(destination.clone());
                if kind.is_dir() {
                    tree.directory(&destination)?;
                    continue;
                }
                if kind.is_symlink() || kind.is_hard_link() {
                    if entry.size() != 0 || tree.records.contains_key(&destination) {
                        return Err(failure(1));
                    }
                    let link = entry.link_name_bytes().ok_or_else(|| failure(1))?;
                    let link = std::str::from_utf8(&link).map_err(|_| failure(1))?;
                    let resolved = target(artifact, &destination, link, kind.is_hard_link())?;
                    if self.links.insert(destination, resolved).is_some() {
                        return Err(failure(1));
                    }
                    continue;
                }
                if !kind.is_file() && kind.as_byte() != b'7' {
                    return Err(failure(1));
                }
                if self.links.contains_key(&destination) {
                    return Err(failure(1));
                }
                let executable = entry.header().mode().map_err(|_| failure(1))? & 0o111 != 0;
                let size = entry.size();
                if let Some(old) = tree.records.get(&destination) {
                    if old.bytes != size || (old.stamp.mode() & 0o111 != 0) != executable {
                        return Err(failure(1));
                    }
                    let mut hash = Sha256::new();
                    let mut count = 0u64;
                    let mut block = [0u8; 65536];
                    loop {
                        checkpoint()?;
                        let n = entry.read(&mut block).map_err(|_| failure(1))?;
                        if n == 0 {
                            break;
                        }
                        count += n as u64;
                        if count > size {
                            return Err(failure(1));
                        }
                        hash.update(&block[..n]);
                    }
                    if count != size || super::super::hex(&hash.finalize()) != old.sha256 {
                        return Err(failure(1));
                    }
                } else {
                    self.bytes = self.bytes.checked_add(size).ok_or_else(|| failure(1))?;
                    if self.bytes > 8 * 1024 * 1024 * 1024 {
                        return Err(failure(1));
                    }
                    tree.stream(&destination, size, executable, &mut entry, &mut || {
                        checkpoint()
                    })?;
                }
            }
            Ok(())
        })?;
        if artifact.format == Format::RustGzip {
            if metadata
                .get(&format!("{}/rust-installer-version", artifact.archive_root))
                .map(Vec::as_slice)
                != Some(b"3\n")
                || metadata
                    .get(&format!("{}/components", artifact.archive_root))
                    .map(Vec::as_slice)
                    != Some(format!("{}\n", artifact.component).as_bytes())
            {
                return Err(failure(1));
            }
            let manifest = metadata
                .get(&format!(
                    "{}/{}/manifest.in",
                    artifact.archive_root, artifact.component
                ))
                .ok_or_else(|| failure(1))?;
            let text = std::str::from_utf8(manifest).map_err(|_| failure(1))?;
            let mut declarations = BTreeMap::new();
            for line in text.lines() {
                let (kind, path) = line.split_once(':').ok_or_else(|| failure(1))?;
                if !matches!(kind, "file" | "dir")
                    || !files::safe(path)
                    || declarations
                        .insert(format!("toolchain/{path}"), kind)
                        .is_some()
                {
                    return Err(failure(1));
                }
            }
            if declarations.is_empty() || declarations.len() > 8192 {
                return Err(failure(1));
            }
            for path in &installed {
                if !declarations
                    .iter()
                    .any(|(p, k)| path == p || *k == "dir" && path.starts_with(&format!("{p}/")))
                {
                    return Err(failure(1));
                }
            }
            for (path, kind) in declarations {
                if !installed.contains(&path)
                    && !(kind == "dir"
                        && installed.iter().any(|p| p.starts_with(&format!("{path}/"))))
                {
                    return Err(failure(1));
                }
            }
        }
        Ok(())
    }
    pub fn resolve(
        &mut self,
        tree: &mut Tree,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        while !self.links.is_empty() {
            let mut progress = false;
            for (name, target) in self.links.clone() {
                checkpoint()?;
                if tree.directory_names().any(|p| p == &target) {
                    if name == target
                        || name.starts_with(&format!("{target}/"))
                        || target.starts_with(&format!("{name}/"))
                    {
                        return Err(failure(1));
                    }
                    // A directory alias is materialized as independent ordinary
                    // files. Resolve its source subtree first; never emit a
                    // symlink into the producer's no-link material namespace.
                    if self
                        .links
                        .keys()
                        .any(|p| p.starts_with(&format!("{target}/")))
                    {
                        continue;
                    }
                    let directories = tree
                        .directory_names()
                        .filter(|p| p.starts_with(&format!("{target}/")))
                        .cloned()
                        .collect::<Vec<_>>();
                    let records = tree
                        .records
                        .keys()
                        .filter(|p| p.starts_with(&format!("{target}/")))
                        .cloned()
                        .collect::<Vec<_>>();
                    tree.directory(&name)?;
                    for directory in directories {
                        tree.directory(&format!("{name}{}", &directory[target.len()..]))?;
                    }
                    for source in records {
                        self.copy(
                            tree,
                            &source,
                            &format!("{name}{}", &source[target.len()..]),
                            checkpoint,
                        )?;
                    }
                    self.links.remove(&name);
                    progress = true;
                    continue;
                }
                let Some(record) = tree.records.get(&target).cloned() else {
                    continue;
                };
                let _ = record;
                self.copy(tree, &target, &name, checkpoint)?;
                self.links.remove(&name);
                progress = true;
            }
            if !progress {
                return Err(failure(1));
            }
        }
        Ok(())
    }
    pub fn copy(
        &mut self,
        tree: &mut Tree,
        source: &str,
        destination: &str,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let record = tree
            .records
            .get(source)
            .cloned()
            .ok_or_else(|| failure(1))?;
        if let Some(existing) = tree.records.get(destination) {
            return if existing.bytes == record.bytes
                && existing.sha256 == record.sha256
                && existing.stamp.mode() & 0o7777 == record.stamp.mode() & 0o7777
            {
                Ok(())
            } else {
                Err(failure(1))
            };
        }
        self.bytes = self
            .bytes
            .checked_add(record.bytes)
            .ok_or_else(|| failure(1))?;
        if self.bytes > 8 * 1024 * 1024 * 1024 {
            return Err(failure(1));
        }
        let file = tree.open(source)?;
        let mut reader = file.file();
        let copied = tree.stream(
            destination,
            record.bytes,
            record.stamp.mode() & 0o111 != 0,
            &mut reader,
            checkpoint,
        );
        let closed = file.finish();
        copied?;
        closed?;
        if tree.records[destination].sha256 != record.sha256 {
            return Err(failure(1));
        }
        Ok(())
    }
}
