//! Bounded Git plumbing output, with no checkout/attribute transformation.

use super::*;
use std::collections::BTreeMap;

pub(super) const BLOB: u64 = 16 * 1024 * 1024;
pub(super) const TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const CATALOG: u64 = 4 * 1024 * 1024;
pub(super) const ENTRIES: usize = 8192;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub oid: String,
    pub bytes: u64,
    pub directory: bool,
    pub executable: bool,
}
pub(super) type Catalog = BTreeMap<String, Entry>;
pub(super) fn oid(bytes: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    if value.len() != 40
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(failure(1));
    }
    Ok(value.into())
}
fn path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value.split('/').count() <= 16
        && value.split('/').all(|part| {
            !part.is_empty()
                && part.len() <= 255
                && !matches!(part, "." | ".." | ".git")
                && !part.bytes().any(|b| matches!(b, 0..=31 | 127))
        })
}
pub(super) fn parse(bytes: &[u8]) -> Result<Catalog> {
    if bytes.len() as u64 > CATALOG || (!bytes.is_empty() && !bytes.ends_with(&[0])) {
        return Err(failure(1));
    }
    let mut result: Catalog = BTreeMap::new();
    let mut total = 0u64;
    let mut names = 0usize;
    for line in bytes.strip_suffix(&[0]).unwrap_or(bytes).split(|b| *b == 0) {
        if bytes.is_empty() {
            break;
        }
        let tab = line
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| failure(1))?;
        if tab > 128 {
            return Err(failure(1));
        }
        let name = std::str::from_utf8(&line[tab + 1..]).map_err(|_| failure(1))?;
        let header = std::str::from_utf8(&line[..tab]).map_err(|_| failure(1))?;
        let parts: Vec<_> = header.split_ascii_whitespace().collect();
        let [mode, kind, object, size] = parts.as_slice() else {
            return Err(failure(1));
        };
        let object = oid(object.as_bytes())?;
        let directory = *mode == "040000" && *kind == "tree" && *size == "-";
        let executable = *mode == "100755";
        if !path(name) || (!directory && (*kind != "blob" || !matches!(*mode, "100644" | "100755")))
        {
            return Err(failure(1));
        }
        let count = if directory {
            0
        } else {
            if size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
                return Err(failure(1));
            }
            size.parse::<u64>().map_err(|_| failure(1))?
        };
        total = total.checked_add(count).ok_or_else(|| failure(1))?;
        names = names.checked_add(name.len()).ok_or_else(|| failure(1))?;
        if count > BLOB
            || total > TOTAL_BYTES
            || names > 2 * 1024 * 1024
            || result.len() >= ENTRIES
            || result
                .insert(
                    name.into(),
                    Entry {
                        oid: object,
                        bytes: count,
                        directory,
                        executable,
                    },
                )
                .is_some()
        {
            return Err(failure(1));
        }
    }
    for name in result.keys() {
        if let Some((parent, _)) = name.rsplit_once('/')
            && !result.get(parent).is_some_and(|entry| entry.directory)
        {
            return Err(failure(1));
        }
    }
    Ok(result)
}
