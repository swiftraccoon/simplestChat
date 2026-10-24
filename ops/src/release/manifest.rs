//! The existing seven-field release manifest, validated without rewriting the
//! bytes covered by archive/attestation identities. Calendar acceptance follows
//! Python's `strptime`, including its Unicode decimal-digit year behavior.

use crate::json::{self, Json};
use crate::{Error, files};
use chrono::{NaiveDate, NaiveTime};
use regex_syntax::hir::{Class, HirKind};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

pub const MAX_BYTES: usize = 1024 * 1024;
const KEYS: [&str; 7] = [
    "schemaVersion",
    "revision",
    "platform",
    "archiveSha256",
    "imageTag",
    "migrations",
    "createdAt",
];

/// Validated semantic fields. Serialized spelling remains the existing contract;
/// use [`Manifest::as_json`] when the original bytes, rather than a projection,
/// must be hashed or retained.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fields {
    pub schema_version: u8,
    pub revision: String,
    pub platform: String,
    pub archive_sha256: String,
    pub image_tag: String,
    pub migrations: BTreeMap<String, String>,
    pub created_at: String,
}

#[derive(Debug)]
pub struct Manifest {
    fields: Fields,
    original: Vec<u8>,
    sha256: String,
}
impl Manifest {
    /// Validate a nonempty, at most 1 MiB input in the Unicode-scalar domain,
    /// with explicit limits of 128 nested containers and 100,000 values.
    /// Decoded duplicate keys, nonfinite floats and unknown
    /// fields are refused. No signature, archive or deployment is authenticated.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return Err(Error::Input);
        }
        let Json::Object(value, _) = json::decode(bytes)? else {
            return Err(Error::Fields);
        };
        if value.len() != KEYS.len() || !KEYS.iter().all(|key| value.contains_key(*key)) {
            return Err(Error::Fields);
        }
        if !matches!(&value["schemaVersion"], Json::Integer(number) if number == "1") {
            return Err(Error::Schema);
        }
        let string = |key: &str, error| value[key].string().ok_or(error);
        let revision = string("revision", Error::Revision)?;
        if !hex(revision, 40) {
            return Err(Error::Revision);
        }
        if string("platform", Error::Platform)? != "linux/amd64" {
            return Err(Error::Platform);
        }
        let archive = string("archiveSha256", Error::ArchiveDigest)?;
        if !hex(archive, 64) {
            return Err(Error::ArchiveDigest);
        }
        let tag = string("imageTag", Error::Tag)?;
        if tag != format!("simplestchat-release/production:{revision}") {
            return Err(Error::Tag);
        }
        let Json::Object(entries, order) = &value["migrations"] else {
            return Err(Error::Migrations);
        };
        if entries.is_empty() || entries.len() > 1000 {
            return Err(Error::Migrations);
        }
        let mut migrations = BTreeMap::new();
        for version in order {
            let checksum = &entries[version];
            if version.is_empty()
                || version.len() > 19
                || version.starts_with('0')
                || !version.bytes().all(|byte| byte.is_ascii_digit())
                || version.parse::<i64>().is_err()
            {
                return Err(Error::MigrationVersion);
            }
            let checksum = checksum
                .string()
                .filter(|value| hex(value, 96))
                .ok_or(Error::MigrationDigest)?;
            migrations.insert(version.clone(), checksum.into());
        }
        let created_at = string("createdAt", Error::TimestampShape)?;
        timestamp(created_at)?;
        Ok(Self {
            fields: Fields {
                schema_version: 1,
                revision: revision.into(),
                platform: "linux/amd64".into(),
                archive_sha256: archive.into(),
                image_tag: tag.into(),
                migrations,
                created_at: created_at.into(),
            },
            original: bytes.into(),
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
    }

    /// Read through retained nonlink directory/file descriptors. Changes to the
    /// opened file or its visible binding refuse success; no path is written.
    /// Relative paths are accepted, but parent components and symlink ancestors
    /// are refused rather than resolved. Success includes an explicit file close.
    pub fn read(path: &Path) -> Result<Self, Error> {
        Self::decode(&files::read(path, MAX_BYTES)?)
    }
    pub fn fields(&self) -> &Fields {
        &self.fields
    }
    pub fn as_json(&self) -> &[u8] {
        &self.original
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn decimal(character: char) -> Option<u32> {
    // The maintained regex-syntax Unicode 16 Nd ranges match pinned CPython's
    // Unicode 16 database. Adjacent decimal sets contain complete groups of ten.
    static DIGITS: OnceLock<Vec<(char, char)>> = OnceLock::new();
    let ranges = DIGITS.get_or_init(|| {
        let hir = regex_syntax::Parser::new()
            .parse(r"\d")
            .expect("fixed Unicode decimal class");
        let HirKind::Class(Class::Unicode(class)) = hir.kind() else {
            unreachable!("fixed decimal class is Unicode")
        };
        class
            .ranges()
            .iter()
            .map(|range| (range.start(), range.end()))
            .collect()
    });
    ranges.iter().find_map(|(start, end)| {
        (*start <= character && character <= *end)
            .then(|| (u32::from(character) - u32::from(*start)) % 10)
    })
}

fn timestamp(value: &str) -> Result<(), Error> {
    let chars: Vec<_> = value.chars().collect();
    if chars.len() != 20
        || [4, 7].iter().any(|i| chars[*i] != '-')
        || chars[10] != 'T'
        || chars[13] != ':'
        || chars[16] != ':'
        || chars[19] != 'Z'
    {
        return Err(Error::TimestampShape);
    }
    let mut digits = [0_u32; 20];
    for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        digits[index] = decimal(chars[index]).ok_or(Error::TimestampShape)?;
    }
    // CPython _strptime uses ASCII ranges in these positions, even though the
    // enclosing release regex accepts Unicode Nd in every numeric position.
    if [5, 6, 8, 11, 14, 17]
        .iter()
        .any(|i| !chars[*i].is_ascii_digit())
        || chars[8] == '3' && !chars[9].is_ascii_digit()
        || chars[11] == '2' && !chars[12].is_ascii_digit()
    {
        return Err(Error::Timestamp);
    }
    let year = digits[0] * 1000 + digits[1] * 100 + digits[2] * 10 + digits[3];
    let pair = |i| digits[i] * 10 + digits[i + 1];
    if year == 0
        || NaiveDate::from_ymd_opt(year as i32, pair(5), pair(8)).is_none()
        || NaiveTime::from_hms_opt(pair(11), pair(14), pair(17)).is_none()
    {
        return Err(Error::Timestamp);
    }
    Ok(())
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
