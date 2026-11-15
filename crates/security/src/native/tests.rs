//! Replays of `recorded_record.json`: what the reference in Python made of
//! the reviewed record of the native components, over the fixture of its
//! own tests. The fixture is kept once and every case as the files it
//! changes; each is built again here and must come to the same.

use super::validate_manifest;
use crate::{
    document::Document,
    scratch::{Scratch, base64},
    vendor::Problem,
};
use std::{io::Read, path::Path};

const RECORDING: &[u8] = include_bytes!("recorded_record.json");

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

fn fields<'a>(case: &'a Document, name: &str) -> &'a [(String, Document)] {
    let held = case.get(name).and_then(Document::fields);
    held.unwrap_or_else(|| panic!("no record {name}"))
}

/// Write the recorded files below `root`: a path that ends with a slash is
/// an empty directory, `absent` takes a file away, and the root's own path
/// is put where the recording spells it.
pub(super) fn written(root: &Path, files: &[(String, Document)]) {
    let shown = root.to_str().expect("a path");
    for (path, body) in files {
        let target = root.join(path);
        if body.text() == Some("absent") {
            std::fs::remove_file(&target).expect("a file to take away");
            continue;
        }
        if path.ends_with('/') {
            std::fs::create_dir_all(&target).expect("a directory");
            continue;
        }
        let mut content = Vec::new();
        let packed = base64(text(body, "deflated"));
        let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut content);
        read.expect("deflated bytes");
        if body.get("spelled") == Some(&Document::Bool(true)) {
            let spelled = String::from_utf8(content).expect("text");
            content = spelled.replace("<root>", shown).into_bytes();
        }
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("a directory");
        std::fs::write(&target, content).expect("a file");
    }
}

/// What every case came to here and there, as lines that can be compared.
fn replayed() -> Vec<(String, String, String)> {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let mut all = Vec::new();
    for (index, case) in cases.expect("cases").iter().enumerate() {
        let scratch = Scratch::new(&format!("native-record-{index}"));
        written(&scratch.0, fields(&recording, "base"));
        written(&scratch.0, fields(case, "delta"));
        let shown = scratch.0.to_str().expect("a path");
        let here = match validate_manifest(&scratch.0) {
            Ok((record, integrity)) => {
                let names = |names: Vec<&str>| {
                    Document::List(names.into_iter().map(Document::from).collect())
                };
                let fields = record.fields().expect("a record").iter();
                let mut sources: Vec<&str> = integrity
                    .sources
                    .iter()
                    .map(|source| source.identifier.as_str())
                    .collect();
                sources.sort_unstable();
                let wraps = integrity.wraps.iter().map(|wrap| wrap.source.as_str());
                let held = Document::record([
                    (
                        "fields",
                        names(fields.map(|(name, _)| name.as_str()).collect()),
                    ),
                    ("sources", names(sources)),
                    ("wraps", names(wraps.collect())),
                ]);
                format!("held {}", held.pretty())
            }
            Err(Problem(words)) => format!("refused {}", words.replace(shown, "<root>")),
        };
        let result = case.get("result").expect("a result");
        let there = if let Some(held) = result.get("held") {
            format!("held {}", held.pretty())
        } else if let Some(words) = result.get("refused").and_then(Document::text) {
            format!("refused {words}")
        } else {
            // What its interpreter said, where it was the interpreter that failed.
            format!("refused {}", text(result, "said"))
        };
        all.push((text(case, "name").to_owned(), here, there));
    }
    all
}

/// Cases that both refuse in other words: the reference in those of its
/// interpreter's system errors and readers, and of a field that is not
/// there.
const REWORDED: [(&str, &str); 11] = [
    (
        "no record",
        "Unreadable file: <root>/vendor/native-components.json",
    ),
    ("a record that is no JSON", "Invalid JSON"),
    (
        "no record of the vendored sources",
        "Unreadable file: <root>/vendor/integrity.json",
    ),
    (
        "a record of the vendored sources that is no JSON",
        "Invalid JSON",
    ),
    (
        "an adapted component without its notes",
        "Unreadable file: <root>/vendor/mediasoup-sys-0.19.0/deps/libwebrtc/README.md",
    ),
    (
        "an adapted component whose notes are no text",
        "No text: <root>/vendor/mediasoup-sys-0.19.0/deps/libwebrtc/README.md",
    ),
    (
        "no OpenSSL installer",
        "Unreadable file: <root>/build/install-openssl.sh",
    ),
    ("no lock", "Unreadable file: <root>/Cargo.lock"),
    ("a lock that is no TOML", "Invalid TOML: <root>/Cargo.lock"),
    ("a lock without packages", "Missing field: package"),
    ("a lock that is no text", "No text: <root>/Cargo.lock"),
];

#[test]
fn every_recorded_record_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    let mut tally = [0; 3];
    for (name, here, there) in replayed() {
        if let Some((_, words)) = REWORDED.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {words}"), "{name}");
            assert!(!there.starts_with("held ") && there != here, "{name}");
            tally[2] += 1;
            continue;
        }
        assert_eq!(here, there, "{name}");
        tally[usize::from(here.starts_with("refused "))] += 1;
    }
    assert_eq!(tally, [29, 110, 11]);
}
