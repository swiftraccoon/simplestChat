//! Replays of the licence verdicts in `recorded_verdicts.json`: what the
//! reference in Python made of the licences of an image's packages.

use super::license_verdict;
use crate::{
    document::Document,
    image::secrets::tests::{cases, exception, here, tallied, text, there},
    policy::Exception,
};
use std::path::Path;

/// The reviewed policy, with the allowed licences that a case names.
fn policy(case: &Document) -> Document {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../security/image-policy.json");
    let mut policy = crate::image::policy::load_policy(&path).expect("the reviewed policy");
    let allowed = case
        .get("allowed")
        .filter(|allowed| **allowed != Document::Null);
    if let (Some(allowed), Document::Record(fields)) = (allowed, &mut policy) {
        for (name, value) in fields.iter_mut() {
            if name == "allowedLicenses" {
                *value = allowed.clone();
            }
        }
    }
    policy
}

fn licenses() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("licenses") {
        let reviews = case
            .get("exceptions")
            .and_then(Document::items)
            .expect("reviews");
        let reviews: Vec<Exception> = reviews.iter().map(exception).collect();
        let packages = case
            .get("packages")
            .and_then(Document::items)
            .expect("packages");
        let came = license_verdict(packages, &policy(&case), &reviews);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

/// Packages whose licences the reference failed on in its interpreter's
/// words: records or an address of another kind, a missing field.
const UNREAD_PACKAGES: [(&str, &str); 9] = [
    (
        "a package of an expression that is a number",
        "image_license_shape",
    ),
    ("a package of a record that is text", "image_license_shape"),
    ("a package whose records are no list", "image_license_shape"),
    ("a package without records", "image_license_shape"),
    ("a package whose address is a list", "image_license_shape"),
    (
        "a package without an address or a name",
        "image_license_shape",
    ),
    (
        "a package without an address and a version that is a number",
        "image_license_shape",
    ),
    (
        "a package of an address that is a number",
        "image_license_shape",
    ),
    (
        "a policy whose allowed licences hold a number",
        "image_license_shape",
    ),
];

/// Declarations of which one record is no expression by itself: wrapped in
/// parentheses and joined, it closes its own and opens others. The
/// reference read the joined text, in which such a record makes the
/// conjunction of the records a choice, and approved them all; here each
/// record must be a whole expression, and these are refused.
const SPLIT: [&str; 3] = [
    "a record that closes its parentheses and opens a choice",
    "a lone record that closes its parentheses and opens a choice",
    "a record that closes its parentheses and opens a conjunction",
];

#[test]
fn every_recorded_licence_verdict_is_the_same() {
    let (split, rest): (Vec<_>, Vec<_>) = licenses()
        .into_iter()
        .partition(|(name, _, _)| SPLIT.contains(&name.as_str()));
    assert_eq!(split.len(), SPLIT.len());
    for (name, here, there) in split {
        assert_eq!(here, "refused image_license_trailing_tokens", "{name}");
        assert!(there.starts_with("held {\n  \"passed\": true,"), "{name}");
    }
    assert_eq!(tallied(rest, &UNREAD_PACKAGES), [43, 13, 9]);
}

#[test]
fn the_longest_expression_is_the_references() {
    let recording = Document::read(include_bytes!("recorded_verdicts.json")).expect("a recording");
    let bound = recording
        .get("bounds")
        .and_then(|bounds| bounds.get("MAX_EXPRESSION"));
    assert_eq!(bound.and_then(Document::whole), Some(4096));
}
