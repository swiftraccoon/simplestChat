//! Ordered public identities and exact Python-compatible index serialization.
use super::{
    MAX_AGE, MAX_CAPTURES, MAX_FILE, MAX_FILES, MAX_GENERATIONS, MAX_INDEX, MAX_TOTAL, Result,
    fail, require,
};
use crate::json::{self, Json};
use serde::{Serialize, Serializer, ser::SerializeMap};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub(super) fn revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// ASCII fingerprint grammar from release_assets.NAME; the 255-byte component
/// cap additionally admits only representable managed Unix filenames.
pub(super) fn name(value: &str) -> bool {
    if value.len() > 255 {
        return false;
    }
    let Some((stem, extension)) = value.rsplit_once('.') else {
        return false;
    };
    if !matches!(
        extension,
        "js" | "css" | "woff" | "woff2" | "svg" | "png" | "jpg" | "jpeg" | "webp" | "avif" | "ico"
    ) {
        return false;
    }
    let token = |bytes: &[u8]| {
        bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    // The regex permits hyphens within both groups. Some earlier hyphen may
    // delimit the suffix even when the final one has fewer than six bytes.
    stem.bytes().enumerate().any(|(index, byte)| {
        byte == b'-'
            && index > 0
            && (6..=64).contains(&(stem.len() - index - 1))
            && token(&stem.as_bytes()[..index])
            && token(&stem.as_bytes()[index + 1..])
    })
}
/// Only read_capture constructs this ordered, bounded public payload. Debug
/// discloses counts, never names or bytes.
pub struct Capture(pub(super) Vec<(String, Vec<u8>)>);
impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetCapture")
            .field("files", &self.0.len())
            .finish()
    }
}
impl Capture {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        super::capture::read(path)
    }
    pub fn files(&self) -> usize {
        self.0.len()
    }
    pub fn bytes(&self) -> u64 {
        self.0.iter().map(|(_, bytes)| bytes.len() as u64).sum()
    }
    pub(super) fn records(&self) -> Result<Vec<(String, Record)>> {
        require(
            !self.0.is_empty() && self.0.len() <= MAX_FILES,
            "asset_capture_revision",
        )?;
        let mut names = BTreeSet::new();
        self.0
            .iter()
            .map(|(name, bytes)| {
                require(
                    self::name(name)
                        && !bytes.is_empty()
                        && bytes.len() as u64 <= MAX_FILE
                        && names.insert(name),
                    "asset_capture_file",
                )?;
                Ok((
                    name.clone(),
                    Record {
                        sha256: sha(bytes),
                        size: bytes.len() as u64,
                    },
                ))
            })
            .collect()
    }
}
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct Record {
    pub sha256: String,
    pub size: u64,
}
fn ordered<S: Serializer, T: Serialize>(
    values: &[(String, T)],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let mut output = serializer.serialize_map(Some(values.len()))?;
    for (name, value) in values {
        output.serialize_entry(name, value)?;
    }
    output.end()
}
#[derive(Clone, Serialize)]
pub(super) struct Generation {
    pub revision: String,
    #[serde(rename = "createdAt")]
    pub created: u64,
    #[serde(rename = "expiresAt")]
    pub expires: u64,
    #[serde(serialize_with = "ordered")]
    pub assets: Vec<(String, Record)>,
}
impl Generation {
    pub fn bytes(&self) -> u64 {
        self.assets.iter().map(|(_, value)| value.size).sum()
    }
}
pub(super) struct Index {
    pub generations: Vec<Generation>,
    pub original: Vec<u8>,
}
fn object(value: &Json) -> Result<&BTreeMap<String, Json>> {
    value.object().ok_or_else(|| fail("asset_operation_failed"))
}
fn exact(value: &BTreeMap<String, Json>, keys: &[&str], code: &'static str) -> Result<()> {
    require(
        value.len() == keys.len() && keys.iter().all(|key| value.contains_key(*key)),
        code,
    )
}
fn integer(value: &Json) -> Option<u64> {
    if let Json::Integer(value) = value {
        if value == "-0" {
            Some(0)
        } else {
            value.parse().ok()
        }
    } else {
        None
    }
}
pub(super) fn ordered_object(value: &Json) -> Result<Vec<(&str, &Json)>> {
    if let Json::Object(values, order) = value {
        Ok(order
            .iter()
            .map(|key| (key.as_str(), &values[key]))
            .collect())
    } else {
        Err(fail("asset_operation_failed"))
    }
}
pub(super) fn version(bytes: &[u8]) -> Result<String> {
    let value = json::decode_limited(bytes, super::MAX_VERSION as usize)
        .map_err(|_| fail("asset_version_schema"))?;
    let value = object(&value)?;
    exact(value, &["revision"], "asset_version_schema")?;
    let selected = value["revision"]
        .string()
        .filter(|value| revision(value))
        .ok_or_else(|| fail("asset_version_schema"))?;
    Ok(selected.into())
}
pub(super) trait Inspect {
    fn directory(&mut self, revision: &str) -> Result<()>;
    fn members(&mut self, revision: &str, names: &[&str]) -> Result<()>;
    fn asset(&mut self, revision: &str, name: &str, record: &Record) -> Result<()>;
}
pub(super) fn parse(bytes: &[u8], inspect: &mut impl Inspect) -> Result<Index> {
    require(bytes.len() as u64 <= MAX_INDEX, "asset_index_size")?;
    let value = json::decode_limited(bytes, MAX_INDEX as usize)
        .map_err(|_| fail("asset_operation_failed"))?;
    let values = object(&value)?;
    exact(
        values,
        &["schemaVersion", "generations"],
        "asset_index_schema",
    )?;
    require(
        values["schemaVersion"].semantic_eq(&Json::Integer("1".into())),
        "asset_index_schema",
    )?;
    let entries = values["generations"]
        .array()
        .ok_or_else(|| fail("asset_operation_failed"))?;
    require(entries.len() <= MAX_GENERATIONS, "asset_index_generations")?;
    // Admit all declared payload bytes before any caller opens an asset.
    let mut total = 0u64;
    for entry in entries {
        let entry = object(entry)?;
        let assets = entry
            .get("assets")
            .ok_or_else(|| fail("asset_generation_fields"))?;
        let assets = object(assets)?;
        require(
            !assets.is_empty() && assets.len() <= MAX_FILES,
            "asset_generation_count",
        )?;
        for value in assets.values() {
            let item = object(value)?;
            let size = item
                .get("size")
                .and_then(integer)
                .filter(|size| *size > 0 && *size <= MAX_FILE)
                .ok_or_else(|| fail("asset_record"))?;
            total = total
                .checked_add(size)
                .ok_or_else(|| fail("asset_index_total"))?;
            require(total <= MAX_TOTAL, "asset_index_total")?;
        }
    }
    let mut revisions = BTreeSet::new();
    let mut generations = vec![];
    for entry in entries {
        let item = object(entry)?;
        exact(
            item,
            &["revision", "createdAt", "expiresAt", "assets"],
            "asset_generation_fields",
        )?;
        let revision = item["revision"]
            .string()
            .filter(|value| self::revision(value))
            .ok_or_else(|| fail("asset_revision"))?;
        let created = integer(&item["createdAt"]).ok_or_else(|| fail("asset_lifetime"))?;
        let expires = integer(&item["expiresAt"]).ok_or_else(|| fail("asset_lifetime"))?;
        require(
            created.checked_add(MAX_AGE) == Some(expires),
            "asset_lifetime",
        )?;
        inspect.directory(revision)?;
        let ordered = ordered_object(&item["assets"])?;
        inspect.members(
            revision,
            &ordered.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        )?;
        let mut assets = vec![];
        for (name, value) in ordered {
            require(self::name(name), "asset_name")?;
            let value = object(value)?;
            exact(value, &["sha256", "size"], "asset_record_fields")?;
            let sha256 = value["sha256"]
                .string()
                .filter(|s| digest(s))
                .ok_or_else(|| fail("asset_record"))?;
            let size = integer(&value["size"])
                .filter(|size| *size > 0 && *size <= MAX_FILE)
                .ok_or_else(|| fail("asset_record"))?;
            let record = Record {
                sha256: sha256.into(),
                size,
            };
            inspect.asset(revision, name, &record)?;
            assets.push((name.into(), record));
        }
        revisions.insert(revision);
        generations.push(Generation {
            revision: revision.into(),
            created,
            expires,
            assets,
        });
    }
    require(
        revisions.len() == generations.len(),
        "asset_index_duplicate",
    )?;
    Ok(Index {
        generations,
        original: bytes.into(),
    })
}
pub(super) fn index(generations: &[Generation]) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Wire<'a> {
        #[serde(rename = "schemaVersion")]
        schema: u8,
        generations: &'a [Generation],
    }
    let mut bytes = serde_json::to_vec(&Wire {
        schema: 1,
        generations,
    })
    .map_err(|_| fail("asset_index_size"))?;
    bytes.push(b'\n');
    require(bytes.len() as u64 <= MAX_INDEX, "asset_index_size")?;
    Ok(bytes)
}
pub(super) fn identities(generations: &[Generation]) -> Result<Vec<(String, String, Record)>> {
    let mut result: Vec<(String, String, Record)> = vec![];
    for generation in generations {
        for (name, record) in &generation.assets {
            if let Some(existing) = result.iter_mut().find(|item| item.0 == *name) {
                require(existing.2 == *record, "asset_immutable_collision")?;
                existing.1 = generation.revision.clone();
            } else {
                result.push((name.clone(), generation.revision.clone(), record.clone()));
            }
        }
    }
    Ok(result)
}
pub(super) fn select(
    prior: &[Generation],
    captures: &[(String, Capture)],
    timestamp: u64,
) -> Result<Vec<Generation>> {
    require(
        !captures.is_empty() && captures.len() <= MAX_CAPTURES,
        "asset_capture_count",
    )?;
    let expires = timestamp
        .checked_add(MAX_AGE)
        .ok_or_else(|| fail("asset_capture_count"))?;
    let mut selected = vec![];
    let mut revisions = BTreeSet::new();
    for (revision, capture) in captures {
        require(
            self::revision(revision) && revisions.insert(revision),
            "asset_capture_revision",
        )?;
        let records = capture.records()?;
        if let Some(old) = prior.iter().find(|item| item.revision == *revision) {
            require(
                old.assets.len() == records.len()
                    && old
                        .assets
                        .iter()
                        .all(|item| records.iter().any(|other| other == item)),
                "asset_revision_changed",
            )?;
        }
        selected.push(Generation {
            revision: revision.clone(),
            created: timestamp,
            expires,
            assets: records,
        });
    }
    let mut total = selected.iter().map(Generation::bytes).sum::<u64>();
    require(total <= MAX_TOTAL, "asset_required_capacity")?;
    let mut history = prior.iter().collect::<Vec<_>>();
    history.sort_by_key(|item| std::cmp::Reverse(item.created));
    for entry in history {
        if !revisions.contains(&entry.revision)
            && entry.expires > timestamp
            && selected.len() < MAX_GENERATIONS
            && total + entry.bytes() <= MAX_TOTAL
        {
            total += entry.bytes();
            selected.push(entry.clone());
        }
    }
    Ok(selected)
}
