//! The identity under which raw licence records are reviewed.
//!
//! A package whose licence the inventory could not put into one
//! expression is reviewed by its whole ordered records. A rebuild of the
//! image gives the layers other digests, so the one field that names a
//! layer, a location's `layerID`, is replaced by a marker once it is seen
//! to be a digest; every other field, the presence of that one, and every
//! order stay part of the identity. The records as they were given keep a
//! digest of their own.

use crate::{Fault, Result, digest, document::Document, record::quoted, require};

/// What stands for the digest of a layer.
pub const LAYER: &str = "sha256:<image-layer>";
/// Records that are no list of records with lists of locations.
const RECORDS: Fault = Fault("image_license_records");

/// A fingerprint to review by, and the digest of the records as given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawLicenseIdentity {
    /// What a review names.
    pub fingerprint: String,
    /// The SHA-256 of the unchanged records.
    pub records_sha256: String,
}

/// JSON as the reference writes it on one line, ASCII only: with sorted
/// names and nothing between its parts to be hashed, or in the order
/// given with a space after every comma and colon to be measured. A
/// number that is no whole number of a machine word has more than one
/// spelling and is refused.
fn written(value: &Document, hashed: bool, text: &mut String) -> Result<()> {
    let (between, after) = if hashed { (",", ":") } else { (", ", ": ") };
    match value {
        Document::Null => text.push_str("null"),
        Document::Bool(flag) => text.push_str(if *flag { "true" } else { "false" }),
        Document::Number(_) => {
            let whole = value.whole().ok_or(Fault("image_license_number"))?;
            text.push_str(&whole.to_string());
        }
        Document::Text(string) => text.push_str(&quoted(string)),
        Document::List(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                text.push_str(if index == 0 { "" } else { between });
                written(item, hashed, text)?;
            }
            text.push(']');
        }
        Document::Record(fields) => {
            // What is measured is as long in any order.
            let mut fields: Vec<&(String, Document)> = fields.iter().collect();
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            text.push('{');
            for (index, (name, value)) in fields.into_iter().enumerate() {
                text.push_str(if index == 0 { "" } else { between });
                text.push_str(&quoted(name));
                text.push_str(after);
                written(value, hashed, text)?;
            }
            text.push('}');
        }
    }
    Ok(())
}

/// A value in its one spelling, as it is hashed: sorted names, nothing
/// between its parts, ASCII only. A number in it that has no one spelling
/// leaves it without one.
pub(super) fn spelled(value: &Document) -> Option<String> {
    let mut text = String::new();
    written(value, true, &mut text).ok()?;
    Some(text)
}

/// The SHA-256 of a value in its one spelling.
fn hashed(value: &Document) -> Result<String> {
    let mut text = String::new();
    written(value, true, &mut text)?;
    Ok(digest(text.as_bytes()))
}

/// How many bytes the reference's plain spelling of a value has.
pub(crate) fn measured(value: &Document) -> Result<usize> {
    let mut text = String::new();
    written(value, false, &mut text)?;
    Ok(text.len())
}

/// The identity of the licence `records` of one package.
pub fn raw_license_identity(records: &[Document]) -> Result<RawLicenseIdentity> {
    let mut normalized = Vec::with_capacity(records.len());
    for item in records {
        let mut record = item.fields().ok_or(RECORDS)?.to_vec();
        for (_, locations) in record.iter_mut().filter(|(name, _)| name == "locations") {
            let mut marked = Vec::new();
            for location in locations.items().ok_or(RECORDS)? {
                let mut location = location.fields().ok_or(RECORDS)?.to_vec();
                for (_, layer) in location.iter_mut().filter(|(name, _)| name == "layerID") {
                    let digest = layer.text().and_then(|layer| layer.strip_prefix("sha256:"));
                    let digest = digest.is_some_and(|digest| super::small_hex(digest, 64));
                    require(digest, "image_license_layer_identity")?;
                    *layer = LAYER.into();
                }
                marked.push(Document::Record(location));
            }
            *locations = Document::List(marked);
        }
        normalized.push(Document::Record(record));
    }
    Ok(RawLicenseIdentity {
        fingerprint: format!("license-raw-v2:{}", hashed(&Document::List(normalized))?),
        records_sha256: hashed(&Document::List(records.to_vec()))?,
    })
}

#[cfg(test)]
#[path = "license_identity_tests.rs"]
mod tests;
