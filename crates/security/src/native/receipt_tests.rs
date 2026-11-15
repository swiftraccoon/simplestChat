//! Replays of `recorded_receipts.json`: what the reference in Python made
//! of receipts of the vendor verification, over the fixture of its own
//! tests.

use super::validate_vendor_receipt;
use crate::{
    document::Document,
    native::{
        read,
        tests::{replay, tallied},
    },
    vendor::manifest,
};

const RECORDING: &[u8] = include_bytes!("recorded_receipts.json");

fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "receipt", &|root| {
        let integrity = manifest::parse(&read(&root.join("vendor/integrity.json"))?)?;
        let receipt = root.join("vendor-evidence/report.json");
        validate_vendor_receipt(root, &receipt, &integrity).map(Document::Text)
    })
}

/// Receipts that both refuse in other words: the reference in those of
/// its interpreter's system errors and readers, and of a field or a
/// vendored file that is not there.
const REWORDED: [(&str, &str); 9] = [
    (
        "no receipt",
        "Unreadable file: <root>/vendor-evidence/report.json",
    ),
    ("a receipt that is no JSON", "Invalid JSON"),
    (
        "no differences beside the receipt",
        "Unreadable file: <root>/vendor-evidence/vendor.diff",
    ),
    ("a source without a name", "Missing field: id"),
    (
        "a vendored file removed after its receipt",
        "Missing vendor file: vendor/mediasoup-sys-0.19.0/deps/libwebrtc/README.md",
    ),
    ("a tree without a path", "Missing field: path"),
    ("a tree without files", "Missing field: files"),
    (
        "a tree file more",
        "Missing vendor file: vendor/mediasoup-sys-0.19.0/more.c",
    ),
    ("a maintained file without a path", "Missing field: path"),
];

#[test]
fn every_recorded_receipt_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [6, 54, 9]);
}
