//! Replays of the licence identities in `recorded_notices.json`: what the
//! reference in Python made of raw licence records.

use super::raw_license_identity;
use crate::document::Document;

const RECORDING: &[u8] = include_bytes!("recorded_notices.json");

/// Every recorded identity: its name, what it came to here and there.
fn identities() -> Vec<(String, String, String)> {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording
        .get("identities")
        .and_then(Document::items)
        .expect("cases");
    let mut all = Vec::new();
    for case in cases {
        let name = case.get("name").and_then(Document::text).expect("a name");
        let records = case
            .get("records")
            .and_then(Document::items)
            .expect("records");
        let here = match raw_license_identity(records) {
            Ok(identity) => {
                let held = Document::record([
                    ("fingerprint", identity.fingerprint.into()),
                    ("recordsSha256", identity.records_sha256.into()),
                ]);
                format!("held {}", held.pretty())
            }
            Err(fault) => format!("refused {fault}"),
        };
        let result = case.get("result").expect("a result");
        let there = if let Some(held) = result.get("held") {
            format!("held {}", held.pretty())
        } else if let Some(words) = result.get("refused").and_then(Document::text) {
            format!("refused {words}")
        } else {
            let raised = result.get("raised").and_then(Document::text);
            format!("RAISED {}", raised.expect("what was raised"))
        };
        all.push((name.to_owned(), here, there));
    }
    all
}

/// Records that the reference failed on in its interpreter's words: a
/// record, a list of locations, a location or a layer of another kind.
const MISSHAPEN: [(&str, &str); 14] = [
    ("a layer that is nothing", "image_license_layer_identity"),
    ("a layer that is a number", "image_license_layer_identity"),
    ("a layer that is a list", "image_license_layer_identity"),
    ("a record that is text", "image_license_records"),
    ("a record that is a number", "image_license_records"),
    ("a record that is nothing", "image_license_records"),
    ("a record that is a list", "image_license_records"),
    ("locations that are a record", "image_license_records"),
    ("locations that are text", "image_license_records"),
    ("locations that are nothing", "image_license_records"),
    ("locations that are a number", "image_license_records"),
    ("a location that is text", "image_license_records"),
    ("a location that is nothing", "image_license_records"),
    ("a location that is a list", "image_license_records"),
];

/// Records with a number that is no whole number of a machine word. The
/// reference wrote a fraction as its interpreter prints one and a larger
/// whole number digit for digit; such a number has no one spelling that
/// every reader of JSON keeps, so no identity is made of it here.
const UNSPELLED: [(&str, &str); 7] = [
    ("a number that is past a word", "image_license_number"),
    ("a number that is far past a word", "image_license_number"),
    ("a number that is one and a half", "image_license_number"),
    ("a number that is a whole fraction", "image_license_number"),
    ("a number that is a large fraction", "image_license_number"),
    ("a number that is a small fraction", "image_license_number"),
    (
        "a number that is a negative nothing",
        "image_license_number",
    ),
];

#[test]
fn every_recorded_identity_is_made_alike() {
    let (mut held, mut refused, mut reworded, mut unspelled) = (0, 0, 0, 0);
    for (name, here, there) in identities() {
        let named = |table: &[(&str, &str)]| {
            let found = table.iter().find(|(other, _)| *other == name);
            found.map(|(_, code)| format!("refused {code}"))
        };
        if let Some(code) = named(&MISSHAPEN) {
            assert_eq!(here, code, "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            reworded += 1;
        } else if let Some(code) = named(&UNSPELLED) {
            assert_eq!(here, code, "{name}");
            assert!(there.starts_with("held "), "{name}");
            unspelled += 1;
        } else {
            assert_eq!(here, there, "{name}");
            held += usize::from(here.starts_with("held "));
            refused += usize::from(here.starts_with("refused "));
        }
    }
    assert_eq!((held, refused, reworded, unspelled), (36, 8, 14, 7));
}
