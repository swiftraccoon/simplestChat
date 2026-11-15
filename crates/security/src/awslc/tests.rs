//! Replays of `recorded_awslc.json`: what the reference in Python made of
//! reviewed records and installers, release archives, sources, build
//! caches and whole installations. Every case is built again here and must
//! come to the same. Where the reference failed in the words of its
//! interpreter, the tables give the words here.

use super::{Limits, evidence, release_files, source_files, validate, verify_options};
use crate::{
    digest,
    document::Document,
    scratch::{Scratch, base64},
    vendor::{Problem, Verdict, compare::Files, manifest::Source},
};
use std::{
    io::Read,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

const RECORDING: &[u8] = include_bytes!("recorded_awslc.json");

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

fn cases<'a>(recording: &'a Document, name: &str) -> &'a [Document] {
    let held = recording.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

/// Bytes that the recording keeps deflated.
fn inflated(kept: &Document) -> Vec<u8> {
    let mut data = Vec::new();
    let packed = base64(text(kept, "deflated"));
    let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut data);
    read.expect("deflated bytes");
    data
}

fn put(path: &Path, content: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, content).expect("a file");
}

/// What the files of a source came to, as the recording lists them.
fn listed(files: &Files) -> Document {
    let identity = |(path, body): (&String, &Vec<u8>)| {
        let identity = vec![body.len().into(), digest(body).into()];
        (path.clone(), Document::List(identity))
    };
    Document::Record(files.iter().map(identity).collect())
}

/// One record with its fields in the order of their names.
fn ordered(document: &Document) -> Document {
    document.clone().sorted()
}

/// What a case came to here, and what the recording says of it: both as a
/// line that can be compared.
fn outcome<T>(root: Option<&Path>, came: &Verdict<T>, held: impl Fn(&T) -> Document) -> String {
    match came {
        Ok(value) => format!("held {}", held(value).pretty()),
        Err(Problem(words)) => {
            let shown = root.map_or_else(String::new, |root| root.display().to_string());
            let words = if shown.is_empty() {
                words.clone()
            } else {
                words.replace(&shown, "<root>")
            };
            format!("refused {words}")
        }
    }
}

fn recorded(case: &Document, sorted: bool) -> String {
    let result = case.get("result").expect("a result");
    if let Some(held) = result.get("held") {
        let held = if sorted { ordered(held) } else { held.clone() };
        return format!("held {}", held.pretty());
    }
    if let Some(words) = result.get("refused").and_then(Document::text) {
        return format!("refused {words}");
    }
    // What its interpreter said, where it was the interpreter that failed.
    format!("refused {}", text(result, "said"))
}

/// Run every case and answer its name with what it came to here and there.
fn replayed() -> Vec<(String, String, String)> {
    let recording = Document::read(RECORDING).expect("a recording");
    let mut all = Vec::new();
    let mut note = |section: &str, case: &Document, here: String, there: String| {
        all.push((format!("{section}: {}", text(case, "name")), here, there));
    };
    let nothing = |(): &()| Document::Null;
    for (index, case) in cases(&recording, "validations").iter().enumerate() {
        let scratch = Scratch::new(&format!("awslc-validate-{index}"));
        let installer = scratch.0.join("build/install-aws-lc.sh");
        let body = case
            .get("installer")
            .filter(|held| **held != Document::Null);
        let body = body.map(inflated);
        std::fs::create_dir_all(scratch.0.join("build")).expect("a directory");
        match (text(case, "state"), body) {
            ("file", Some(body)) => put(&installer, &body),
            ("link", Some(body)) => {
                put(&scratch.0.join("build/real.sh"), &body);
                symlink("real.sh", &installer).expect("a link");
            }
            ("directory", _) => std::fs::create_dir(&installer).expect("a directory"),
            _ => {}
        }
        let component = Document::read(text(case, "component").as_bytes()).expect("a component");
        let came = validate(&scratch.0, &component);
        note(
            "validations",
            case,
            outcome(Some(&scratch.0), &came, nothing),
            recorded(case, false),
        );
    }
    for case in cases(&recording, "releases") {
        let data = inflated(case.get("archive").expect("an archive"));
        let pinned = case.get("digest").and_then(Document::text);
        let source = Source {
            identifier: "aws-lc-5.11.0".to_owned(),
            url: "https://example.invalid/a".to_owned(),
            sha256: pinned.map_or_else(|| digest(&data), str::to_owned),
            format: "tar.gz".to_owned(),
        };
        let came = release_files(&source, &data, &Limits::REVIEWED);
        note(
            "releases",
            case,
            outcome(None, &came, listed),
            recorded(case, true),
        );
    }
    for case in cases(&recording, "sources") {
        let data = inflated(case.get("archive").expect("an archive"));
        let source = Source {
            identifier: text(case, "id").to_owned(),
            url: "https://example.invalid/a".to_owned(),
            sha256: digest(&data),
            format: text(case, "format").to_owned(),
        };
        let release = case.get("release") == Some(&Document::Bool(true));
        let came = source_files(&source, &data, release);
        note(
            "sources",
            case,
            outcome(None, &came, listed),
            recorded(case, true),
        );
    }
    for case in cases(&recording, "options") {
        let came = verify_options(&base64(text(case, "cache")));
        note(
            "options",
            case,
            outcome(None, &came, nothing),
            recorded(case, false),
        );
    }
    for (index, case) in cases(&recording, "installations").iter().enumerate() {
        let scratch = Scratch::new(&format!("awslc-install-{index}"));
        let prefix = scratch.0.join("aws-lc");
        let files = case.get("files").and_then(Document::fields).expect("files");
        for (path, body) in files {
            put(&prefix.join(path), &base64(body.text().expect("bytes")));
        }
        if let Some(receipt) = case.get("receipt").and_then(Document::text) {
            put(
                &prefix.join("share/simplestchat/build.json"),
                receipt.as_bytes(),
            );
        }
        let links = case.get("links").and_then(Document::fields).expect("links");
        for (path, target) in links {
            let _ = std::fs::remove_file(prefix.join(path));
            symlink(target.text().expect("a target"), prefix.join(path)).expect("a link");
        }
        let installed = files
            .iter()
            .find(|(path, _)| path == "share/rust/aws_lc_bindings.rs");
        let installed =
            installed.map_or_else(Vec::new, |(_, body)| base64(body.text().expect("bytes")));
        let mut generated: Vec<PathBuf> = Vec::new();
        for (at, body) in cases(case, "bindings").iter().enumerate() {
            let body = body.text().expect("bindings");
            let path = scratch.0.join(format!("generated-{at}.rs"));
            let same = body == "same";
            put(
                &path,
                &if same {
                    installed.clone()
                } else {
                    base64(body)
                },
            );
            generated.push(path);
        }
        let component = Document::read(text(case, "component").as_bytes()).expect("a component");
        let archives = Document::read(text(case, "archives").as_bytes()).expect("archives");
        let archives = archives.items().expect("a list");
        let came = evidence(&prefix, &component, archives, &generated);
        let held = |held: &Document| held.clone();
        note(
            "installations",
            case,
            outcome(Some(&scratch.0), &came, held),
            recorded(case, false),
        );
    }
    all
}

/// Cases that both refuse in other words: the reference in those of its
/// interpreter's system errors, readers and missing fields, and with a
/// name that is no ASCII shown as it is. A sparse member and a header
/// without its file are found here by the reader of the archive.
const REWORDED: [(&str, &str); 31] = [
    ("validations: no version", "Missing field: version"),
    ("validations: no system", "Missing field: system"),
    (
        "validations: no installer",
        "Unreadable file: <root>/build/install-aws-lc.sh",
    ),
    (
        "validations: an installer that is a link",
        "Unreadable file: <root>/build/install-aws-lc.sh",
    ),
    (
        "validations: an installer that is a directory",
        "Not a bounded regular file: <root>/build/install-aws-lc.sh",
    ),
    ("releases: nothing", "Unreadable gzip stream"),
    ("releases: no gzip", "Unreadable gzip stream"),
    ("releases: text", "Unreadable gzip stream"),
    ("releases: gzip of text", "Unreadable tar archive"),
    ("releases: gzip of nothing", "Unreadable tar archive"),
    ("releases: gzip cut short", "Unreadable gzip stream"),
    (
        "releases: gzip with a wrong check",
        "Unreadable gzip stream",
    ),
    ("releases: gzip and more bytes", "Unreadable gzip stream"),
    (
        "releases: a tar cut short inside a file",
        "Unreadable tar archive",
    ),
    (
        "releases: a tar cut short after a header",
        "AWS-LC archive member size differs",
    ),
    (
        "releases: a tar with a damaged header check",
        "Unreadable tar archive",
    ),
    (
        "releases: a name that is no ASCII",
        "Unsafe or noncanonical path: 'pkg-1.0/caf\\xe9'",
    ),
    (
        "releases: a name that is no ASCII in the GNU format",
        "Unsafe or noncanonical path: 'pkg-1.0/caf\\xe9'",
    ),
    (
        "releases: a sparse file of the GNU kind",
        "Unreadable tar archive",
    ),
    (
        "releases: a file larger than its bytes",
        "Unreadable tar archive",
    ),
    (
        "sources: a crate said to be a zip",
        "Unreadable zip archive",
    ),
    (
        "options: bytes that are no text",
        "AWS-LC build cache is no text",
    ),
    (
        "installations: no receipt",
        "Unreadable file: <root>/aws-lc/share/simplestchat/build.json",
    ),
    ("installations: a receipt that is no JSON", "Invalid JSON"),
    (
        "installations: a file that is a link",
        "Unreadable file: <root>/aws-lc/share/simplestchat/symbols.txt",
    ),
    (
        "installations: a component without a version",
        "Missing field: version",
    ),
    (
        "installations: an archive of the provider without a library",
        "Missing field: library",
    ),
    (
        "installations: an archive of the provider without a digest",
        "Missing field: sha256",
    ),
    (
        "installations: a system without its options",
        "Missing field: configure_options",
    ),
    (
        "installations: a system without its source",
        "Missing field: source",
    ),
    (
        "installations: a system without its licence digest",
        "Missing field: source_license_sha256",
    ),
];

/// Cases the reference held and that are refused here: it took a whole
/// number for the same number with a fraction and a truth value for one
/// or nought, where a receipt must say what the record says; and it put a
/// version that is a number into the line the header must have, where a
/// version is text.
const STRICTER: [(&str, &str); 3] = [
    (
        "installations: a whole number where the record has it with a fraction",
        "AWS-LC installed build receipt differs",
    ),
    (
        "installations: a truth value where the record has a number",
        "AWS-LC installed build receipt differs",
    ),
    (
        "installations: a version that is a number, as the header says it",
        "AWS-LC installed version header differs from authenticated source",
    ),
];

#[test]
fn every_recorded_case_comes_to_the_same() {
    let sections = [
        "validations",
        "releases",
        "sources",
        "options",
        "installations",
    ];
    let mut tally = [[0; 4]; 5];
    for (name, here, there) in replayed() {
        let section = name.split(':').next().expect("a section");
        let section = sections.iter().position(|held| *held == section);
        let counted = &mut tally[section.expect("a section")];
        if let Some((_, words)) = REWORDED.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {words}"), "{name}");
            assert!(!there.starts_with("held ") && there != here, "{name}");
            counted[2] += 1;
            continue;
        }
        if let Some((_, words)) = STRICTER.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {words}"), "{name}");
            assert!(there.starts_with("held "), "{name}");
            counted[3] += 1;
            continue;
        }
        assert_eq!(here, there, "{name}");
        counted[usize::from(here.starts_with("refused "))] += 1;
    }
    assert_eq!(
        tally,
        [
            [4, 45, 5, 0],
            [33, 23, 15, 0],
            [5, 5, 1, 0],
            [16, 16, 1, 0],
            [7, 56, 9, 3]
        ]
    );
}

#[test]
fn the_release_archive_is_held_to_every_limit_at_its_edge() {
    const REVIEWED: Limits = Limits::REVIEWED;
    let recording = Document::read(RECORDING).expect("a recording");
    let named = |case: &&Document| text(case, "name") == "what is kept and what is only named";
    let case = cases(&recording, "releases").iter().find(named);
    let data = inflated(case.expect("a case").get("archive").expect("an archive"));
    let source = Source {
        identifier: "aws-lc-5.11.0".to_owned(),
        url: "https://example.invalid/a".to_owned(),
        sha256: digest(&data),
        format: "tar.gz".to_owned(),
    };
    let read = |limits: Limits| {
        let read = release_files(&source, &data, &limits);
        read.map(|files| files.len())
    };
    let refused = |words: &str| Err(Problem(words.to_owned()));
    // Nine files and a directory; the largest file is the header.
    assert_eq!(read(REVIEWED), Ok(9));
    let archive = data.len();
    assert_eq!(
        read(Limits {
            archive,
            ..REVIEWED
        }),
        Ok(9)
    );
    let archive = archive - 1;
    let differs = refused("AWS-LC source SHA-256 or size differs");
    assert_eq!(
        read(Limits {
            archive,
            ..REVIEWED
        }),
        differs
    );
    let mut raw = Vec::new();
    let unpacked = flate2::read::GzDecoder::new(&data[..]).read_to_end(&mut raw);
    let expanded = unpacked.expect("a gzip stream") as u64;
    assert_eq!(
        read(Limits {
            expanded,
            ..REVIEWED
        }),
        Ok(9)
    );
    let expanded = expanded - 1;
    let budget = refused("AWS-LC archive expansion budget exceeded");
    assert_eq!(
        read(Limits {
            expanded,
            ..REVIEWED
        }),
        budget
    );
    let member = 70;
    assert_eq!(read(Limits { member, ..REVIEWED }), Ok(9));
    let member = member - 1;
    let large = refused("AWS-LC archive member exceeds its budget");
    assert_eq!(read(Limits { member, ..REVIEWED }), large);
    let entries = 10;
    assert_eq!(
        read(Limits {
            entries,
            ..REVIEWED
        }),
        Ok(9)
    );
    let entries = entries - 1;
    let many = refused("AWS-LC duplicate or excessive archive members");
    assert_eq!(
        read(Limits {
            entries,
            ..REVIEWED
        }),
        many
    );
}

#[test]
fn the_reviewed_limits_options_and_files_are_the_references() {
    let megabyte = 1024 * 1024;
    let reviewed = Limits {
        archive: 192 * megabyte,
        expanded: 384 * megabyte as u64,
        member: 64 * megabyte as u64,
        entries: 20000,
    };
    assert_eq!(Limits::REVIEWED, reviewed);
    let recording = Document::read(RECORDING).expect("a recording");
    let there = recording.get("reviewed").expect("what is reviewed");
    let whole = |name: &str| there.get(name).and_then(Document::whole);
    assert_eq!(whole("archive"), Some(192 * 1024 * 1024));
    assert_eq!(whole("expanded"), Some(384 * 1024 * 1024));
    assert_eq!(whole("member"), Some(64 * 1024 * 1024));
    let options = there.get("options").and_then(Document::fields);
    let options: Vec<(&str, &str)> = options
        .expect("options")
        .iter()
        .map(|(name, value)| (name.as_str(), value.text().expect("a value")))
        .collect();
    assert_eq!(options, super::OPTIONS);
    let files: Vec<&str> = cases(there, "files")
        .iter()
        .filter_map(Document::text)
        .collect();
    assert_eq!(files, super::FILES);
    let namespace = super::OPTIONS
        .iter()
        .find(|(name, _)| *name == "BORINGSSL_PREFIX");
    assert_eq!(namespace.map(|(_, value)| *value), Some(super::PREFIX));
}
