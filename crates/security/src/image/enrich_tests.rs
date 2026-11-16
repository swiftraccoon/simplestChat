//! Replays of `recorded_enrich.json`: what the reference in Python made of
//! the join of the Rust licences to the binary's crates, of the identity of
//! the static C++ runtime's rpm, of the native inventory and of whole
//! enrichments of the scanner's inventory.

use super::{enrich_sbom, native_packages, rust_license_join, static_rpm};
use crate::{
    document::Document,
    image::{
        policy::{
            Limits,
            tests::{There, tallied},
        },
        secrets::tests::{here, text, there},
    },
};

const RECORDING: &[u8] = include_bytes!("recorded_enrich.json");
/// The scanner release that the recorded inventories name.
const SYFT: &str = "1.54.0";

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn cases(name: &str) -> Vec<Document> {
    let held = recording()
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

fn joins() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("joins") {
        let packages = case.get("packages").and_then(Document::items);
        let mut packages = packages.expect("packages").to_vec();
        let native = case.get("native").expect("a native record");
        let elf = case.get("elf").expect("binary evidence");
        let came = rust_license_join(&mut packages, native, elf).map(|()| Document::List(packages));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn statics() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("statics") {
        let mut package = case.get("package").expect("a package").clone();
        let owner = case
            .get("owner")
            .and_then(Document::items)
            .expect("an owner");
        let owner: Vec<&str> = owner
            .iter()
            .map(|field| field.text().expect("text"))
            .collect();
        let owner: [&str; 3] = owner.try_into().expect("three fields");
        let came = static_rpm(&mut package, owner).map(|()| package);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn natives() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("natives") {
        let native = case.get("native").expect("a native record");
        let came = native_packages(native).map(Document::List);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn enrichments() -> Vec<(String, String, String)> {
    let recording = recording();
    let policy = recording.get("policy").expect("the image policy");
    let mut all = Vec::new();
    for case in cases("enriched") {
        let value = case.get("value").expect("an inventory").clone();
        let native = case.get("native").expect("a native record");
        let elf = case.get("elf").expect("binary evidence");
        let came = enrich_sbom(value, native, elf, policy, SYFT, &Limits::REVIEWED);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

/// Joins that the reference failed on in its interpreter's words: a field
/// that is missing or of another kind.
const UNREAD_JOINS: [(&str, &str); 6] = [
    ("binary metadata without its format", "image_evidence_shape"),
    ("a declaration that is a number", "image_evidence_shape"),
    ("a crate observed from None", "image_evidence_shape"),
    ("a crate without a name", "image_evidence_shape"),
    ("a third party without a licence", "image_evidence_shape"),
    (
        "a licence record without its source",
        "image_evidence_shape",
    ),
];

/// Identities of the static runtime's rpm that the reference took and that
/// are refused here: its interpreter read the decimal digits of any script
/// as an epoch and held numbers of any length, and an epoch here is digits
/// of ASCII that a machine word holds.
const STRICTER_STATICS: [(&str, &str); 2] = [
    (
        "the static runtime with an epoch of other digits",
        "image_static_rpm_nevra",
    ),
    (
        "the static runtime with an epoch past the machine word",
        "image_static_rpm_nevra",
    ),
];

/// Native records that the reference failed on in its interpreter's words.
const UNREAD_NATIVES: [(&str, &str); 9] = [
    ("a wrapped input without its use", "image_evidence_shape"),
    (
        "a wrapped input without its version",
        "image_evidence_shape",
    ),
    (
        "a wrapped input whose licence is a number",
        "image_evidence_shape",
    ),
    ("a wrapped input that is no record", "image_evidence_shape"),
    (
        "an OpenSSL record without a version",
        "image_evidence_shape",
    ),
    (
        "an adapted input without its licence",
        "image_evidence_shape",
    ),
    (
        "a static runtime whose licence is a number",
        "image_evidence_shape",
    ),
    (
        "a static runtime whose owner is a number",
        "image_evidence_shape",
    ),
    ("no toolchain", "image_evidence_shape"),
];

/// Inventories whose source the reference failed on in its interpreter's
/// words.
const UNREAD_ENRICHMENTS: [(&str, &str); 3] = [
    ("an inventory without its source", "image_evidence_shape"),
    ("a source without its metadata", "image_evidence_shape"),
    ("metadata that is no record", "image_evidence_shape"),
];

#[test]
fn every_recorded_join_is_the_same() {
    let tables = [(&UNREAD_JOINS[..], There::Raised)];
    assert_eq!(tallied(joins(), &tables), [25, 36, 6, 0, 0]);
}

#[test]
fn every_recorded_static_rpm_is_the_same() {
    let tables = [(&STRICTER_STATICS[..], There::Held)];
    assert_eq!(tallied(statics(), &tables), [12, 11, 0, 2, 0]);
}

#[test]
fn every_recorded_native_inventory_is_the_same() {
    let tables = [(&UNREAD_NATIVES[..], There::Raised)];
    assert_eq!(tallied(natives(), &tables), [7, 4, 9, 0, 0]);
}

#[test]
fn every_recorded_enrichment_is_the_same() {
    let tables = [(&UNREAD_ENRICHMENTS[..], There::Raised)];
    assert_eq!(tallied(enrichments(), &tables), [3, 3, 3, 0, 0]);
}
