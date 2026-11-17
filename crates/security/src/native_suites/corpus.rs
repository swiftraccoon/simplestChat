//! The reviewed inputs of the replay: every family's, with exact bytes, and
//! nothing beside them.

use super::{FAMILIES, MIB, read_regular};
use crate::{Fault, Result, digest, document::Document, require};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// The most bytes an input may have.
pub const MAX_INPUT: usize = 65_536;
/// The most inputs of one family.
const MAX_CASES: usize = 64;
/// The most characters of an input's provenance.
const MAX_PROVENANCE: usize = 1024;
/// The unit tests' fixtures among the inputs, with the names they are read
/// by.
pub const UNIT_FIXTURES: [(&str, &str); 3] = [
    ("rtp-unit-packet1", "packet1.raw"),
    ("rtp-unit-packet2", "packet2.raw"),
    ("rtp-unit-packet3", "packet3.raw"),
];
/// A manifest, or an item of it, that is no JSON or not of its kind.
const UNREAD: Fault = Fault("native_corpus_unread");

/// One reviewed input, decoded, and the family whose parser it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    /// Its name.
    pub identifier: String,
    /// The family whose parser replays it.
    pub family: String,
    /// Its file, below `security/native`.
    pub relative: String,
    /// Its bytes.
    pub data: Vec<u8>,
    /// The SHA-256 of its bytes.
    pub digest: String,
}

/// Whether `text` is a name: a small letter, then at most 79 small letters,
/// digits and hyphens.
fn name(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && text.len() <= 80
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

/// Whether `encoded` is pairs of small hexadecimal digits, each followed by
/// any spaces and line ends, and at least one pair.
fn hexadecimal(encoded: &[u8]) -> bool {
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    let mut rest = encoded;
    let mut pairs = 0;
    while let [high, low, after @ ..] = rest {
        if !digit(*high) || !digit(*low) {
            return false;
        }
        pairs += 1;
        let spaced = after.iter().take_while(|byte| matches!(byte, b' ' | b'\n'));
        rest = &after[spaced.count()..];
    }
    pairs > 0 && rest.is_empty()
}

/// The bytes that the hexadecimal digits of `encoded` spell, spaces and
/// line ends between their pairs left out.
fn decoded(encoded: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = encoded
        .iter()
        .copied()
        .filter(|byte| byte.is_ascii_hexdigit())
        .collect();
    let value = |byte: u8| (byte as char).to_digit(16).map_or(0, |value| value as u8);
    digits
        .chunks(2)
        .map(|pair| value(pair[0]) << 4 | value(pair[1]))
        .collect()
}

/// The text of the field `key` of a record.
fn text<'a>(fields: &'a Document, key: &str) -> Result<&'a str> {
    fields.get(key).and_then(Document::text).ok_or(UNREAD)
}

/// Whether a record's fields are exactly `names`.
fn exactly(fields: &[(String, Document)], names: &[&str]) -> bool {
    let held: BTreeSet<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    held == names.iter().copied().collect()
}

/// Every regular file, link and other thing below `directory`, never
/// following a link, named relative to `base`.
fn listed(directory: &Path, base: &Path, found: &mut BTreeSet<String>) -> Result<()> {
    let entries = std::fs::read_dir(directory).map_err(|_| UNREAD)?;
    for entry in entries {
        let path = entry.map_err(|_| UNREAD)?.path();
        let metadata = path.symlink_metadata().map_err(|_| UNREAD)?;
        if metadata.is_dir() {
            listed(&path, base, found)?;
        } else {
            let relative = path.strip_prefix(base).map_err(|_| UNREAD)?;
            found.insert(relative.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

/// The reviewed inputs below `root`, every family's, each with the bytes,
/// size and digest its record names, and no file beside them.
pub fn corpus(root: &Path) -> Result<Vec<Case>> {
    let directory = root.join("security/native");
    let manifest = read_regular(&directory.join("corpus.json"), MIB)?;
    let manifest = Document::read(&manifest).map_err(|_| UNREAD)?;
    let fields = manifest.fields().ok_or(UNREAD)?;
    require(
        exactly(fields, &["schemaVersion", "description", "cases"]),
        "corpus_schema",
    )?;
    let version = manifest.get("schemaVersion").and_then(Document::whole);
    require(version.ok_or(UNREAD)? == 1, "corpus_version")?;
    require(
        !text(&manifest, "description")?.is_empty(),
        "corpus_description",
    )?;
    let listed_cases = manifest.get("cases").and_then(Document::items);
    let mut cases = Vec::new();
    let (mut identifiers, mut paths) = (BTreeSet::new(), BTreeSet::new());
    let mut counts: BTreeMap<&str, usize> = FAMILIES.iter().map(|family| (*family, 0)).collect();
    for item in listed_cases.ok_or(UNREAD)? {
        let fields = item.fields().ok_or(UNREAD)?;
        let wanted = ["id", "family", "path", "sha256", "bytes", "provenance"];
        require(exactly(fields, &wanted), "corpus_case_schema")?;
        let (identifier, family) = (text(item, "id")?, text(item, "family")?);
        let (relative, digested) = (text(item, "path")?, text(item, "sha256")?);
        require(FAMILIES.contains(&family), "corpus_family")?;
        require(name(identifier), "corpus_case_id")?;
        let new = !identifiers.contains(identifier) && !paths.contains(relative);
        require(new, "corpus_duplicate")?;
        let file = relative
            .strip_prefix("corpus/")
            .and_then(|rest| rest.strip_prefix(family))
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.strip_suffix(".hex"));
        require(file.is_some_and(name), "corpus_path")?;
        let family_directory = directory.join("corpus").join(family);
        let linked = family_directory
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        require(!linked, "corpus_symlink")?;
        let encoded = read_regular(&directory.join(relative), (MAX_INPUT * 3) as u64)?;
        require(hexadecimal(&encoded), "corpus_encoding")?;
        let data = decoded(&encoded);
        require((1..=MAX_INPUT).contains(&data.len()), "corpus_input_size")?;
        let size = item.get("bytes").and_then(Document::whole).ok_or(UNREAD)?;
        require(size == data.len() as i128, "corpus_size_mismatch")?;
        require(digest(&data) == digested, "corpus_digest_mismatch")?;
        let provenance = text(item, "provenance")?.chars().count();
        require(
            (1..=MAX_PROVENANCE).contains(&provenance),
            "corpus_provenance",
        )?;
        let count = counts.get_mut(family).ok_or(UNREAD)?;
        *count += 1;
        require(*count <= MAX_CASES, "corpus_case_count")?;
        identifiers.insert(identifier.to_owned());
        paths.insert(relative.to_owned());
        cases.push(Case {
            identifier: identifier.to_owned(),
            family: family.to_owned(),
            relative: relative.to_owned(),
            data,
            digest: digested.to_owned(),
        });
    }
    require(
        counts.values().all(|count| *count > 0),
        "corpus_missing_family",
    )?;
    let fixtures = UNIT_FIXTURES
        .iter()
        .all(|(fixture, _)| identifiers.contains(*fixture));
    require(fixtures, "missing_native_unit_fixture")?;
    let mut actual = BTreeSet::new();
    listed(&directory.join("corpus"), &directory, &mut actual)?;
    let paths: BTreeSet<String> = paths.into_iter().collect();
    require(paths == actual, "corpus_unlisted_file")?;
    Ok(cases)
}
