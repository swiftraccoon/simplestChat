//! Replays of `recorded_record.json`: what the reference in Python made of
//! the reviewed record of the native components, over the fixture of its
//! own tests. The fixture is kept once and every case as the files it
//! changes; each is built again here and must come to the same.

use super::validate_manifest;
use crate::{
    document::Document,
    scratch::{Scratch, base64},
    vendor::{Problem, Verdict},
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

/// Build every case of a recording again and answer its name with what
/// `work` came to over it here and with what the recording says of it, as
/// lines that can be compared. The root's own path is spelled `<root>`.
pub(super) fn replay(
    recording: &[u8],
    place: &str,
    work: &dyn Fn(&Path) -> Verdict<Document>,
) -> Vec<(String, String, String)> {
    let recording = Document::read(recording).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let mut all = Vec::new();
    for (index, case) in cases.expect("cases").iter().enumerate() {
        let scratch = Scratch::new(&format!("native-{place}-{index}"));
        written(&scratch.0, fields(&recording, "base"));
        written(&scratch.0, fields(case, "delta"));
        let shown = scratch.0.to_str().expect("a path");
        for (path, target) in fields(case, "links") {
            let target = target.text().expect("a target").replace("<root>", shown);
            std::os::unix::fs::symlink(target, scratch.0.join(path)).expect("a link");
        }
        let here = match work(&scratch.0) {
            Ok(held) => format!("held {}", held.pretty().replace(shown, "<root>")),
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

/// A list of names.
pub(super) fn names<'a>(names: impl Iterator<Item = &'a str>) -> Document {
    Document::List(names.map(Document::from).collect())
}

/// Hold every case to the recording, but for those that `reworded` names
/// with the words they have here, and answer how many were held alike,
/// refused alike and refused in other words.
pub(super) fn tallied(all: Vec<(String, String, String)>, reworded: &[(&str, &str)]) -> [usize; 3] {
    let mut tally = [0; 3];
    for (name, here, there) in all {
        if let Some((_, words)) = reworded.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {words}"), "{name}");
            assert!(!there.starts_with("held ") && there != here, "{name}");
            tally[2] += 1;
            continue;
        }
        assert_eq!(here, there, "{name}");
        tally[usize::from(here.starts_with("refused "))] += 1;
    }
    tally
}

/// What the reviewed record came to.
fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "record", &|root| {
        let (record, integrity) = validate_manifest(root)?;
        let fields = record.fields().expect("a record").iter();
        let mut sources: Vec<&str> = integrity
            .sources
            .iter()
            .map(|source| source.identifier.as_str())
            .collect();
        sources.sort_unstable();
        Ok(Document::record([
            ("fields", names(fields.map(|(name, _)| name.as_str()))),
            ("sources", names(sources.into_iter())),
            (
                "wraps",
                names(integrity.wraps.iter().map(|wrap| wrap.source.as_str())),
            ),
        ]))
    })
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
    let tally = tallied(recorded(), &REWORDED);
    assert_eq!(tally, [29, 110, 11]);
}
