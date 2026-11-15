//! Replays of `recorded_archive.json`: what the reference in Python made
//! of member names, link targets, raw tar headers, layer files, single
//! layers and whole saved images.

use super::{Entry, Limits, canonical, layer_file, link_target, physical_headers, read_layer};
use crate::{
    digest,
    document::Document,
    scratch::{Scratch, base64},
};
use std::{
    io::{Cursor, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

#[path = "archive_direct_tests.rs"]
mod direct;

const RECORDING: &[u8] = include_bytes!("recorded_archive.json");

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

fn cases(name: &str) -> Vec<Document> {
    let recording = Document::read(RECORDING).expect("a recording");
    let held = recording.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no cases {name}")).to_vec()
}

/// Bytes that the recording keeps deflated.
fn inflated(kept: &Document) -> Vec<u8> {
    let mut data = Vec::new();
    let packed = base64(text(kept, "deflated"));
    let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut data);
    read.expect("deflated bytes");
    assert_eq!(digest(&data), text(kept, "sha256"));
    data
}

/// What a case came to there: what was held, the refusal, or the class of
/// what the reference's interpreter raised.
fn there(case: &Document) -> String {
    let result = case.get("result").expect("a result");
    if let Some(held) = result.get("held") {
        return format!("held {}", held.pretty());
    }
    if let Some(words) = result.get("refused").and_then(Document::text) {
        return format!("refused {words}");
    }
    format!("RAISED {}", text(result, "raised"))
}

fn here(came: crate::Result<Document>) -> String {
    match came {
        Ok(held) => format!("held {}", held.pretty()),
        Err(fault) => format!("refused {fault}"),
    }
}

/// What is below a directory, as the recorder lists it: every entry's
/// kind, a link's target, a file's size, digest, mode and number of names.
fn listing(root: &Path) -> Document {
    fn walk(root: &Path, directory: &Path, found: &mut Vec<(String, Document)>) {
        let mut entries: Vec<_> = std::fs::read_dir(directory)
            .expect("a directory")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path
                .strip_prefix(root)
                .expect("below")
                .to_str()
                .expect("a name")
                .to_owned();
            let held = path.symlink_metadata().expect("an entry");
            if held.file_type().is_symlink() {
                let target = std::fs::read_link(&path).expect("a link");
                found.push((
                    name,
                    Document::List(vec![
                        "link".into(),
                        target.to_str().expect("a target").into(),
                    ]),
                ));
            } else if held.is_dir() {
                found.push((
                    name,
                    Document::List(vec![
                        "directory".into(),
                        ((held.permissions().mode() & 0o7777) as usize).into(),
                    ]),
                ));
                walk(root, &path, found);
            } else {
                let body = std::fs::read(&path).expect("a file");
                found.push((
                    name,
                    Document::List(vec![
                        "file".into(),
                        body.len().into(),
                        digest(&body).into(),
                        ((held.permissions().mode() & 0o7777) as usize).into(),
                        (held.nlink() as usize).into(),
                    ]),
                ));
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Document::Record(found)
}

fn names() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("names") {
        let name = match case.get("name").and_then(Document::text) {
            Some(name) => name.to_owned(),
            None => {
                let long = case
                    .get("long")
                    .and_then(Document::whole)
                    .expect("a length");
                let prefix = text(&case, "prefix");
                let filler = if prefix == "./" { "x" } else { &prefix[..1] };
                let rest = usize::try_from(long).expect("a length") - prefix.len();
                format!("{prefix}{}", filler.repeat(rest))
            }
        };
        let directory = case.get("directory") == Some(&Document::Bool(true));
        let came = canonical(&name, directory, &Limits::REVIEWED).map(Document::from);
        let shown: String = name.chars().take(40).collect();
        all.push((format!("{shown:?} {directory}"), here(came), there(&case)));
    }
    all
}

fn targets() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("targets") {
        let (name, target) = (text(&case, "name"), text(&case, "target"));
        let hard = case.get("hard") == Some(&Document::Bool(true));
        let came = link_target(name, target, hard).map(Document::from);
        all.push((
            format!("{name} {target:?} {hard}"),
            here(came),
            there(&case),
        ));
    }
    all
}

fn headers() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("headers") {
        let data = inflated(case.get("archive").expect("an archive"));
        let outer = case.get("outer") == Some(&Document::Bool(true));
        let came = physical_headers(&mut Cursor::new(&data), outer, &Limits::REVIEWED);
        let name = format!("{} {outer}", text(&case, "name"));
        all.push((name, here(came.map(|()| Document::Null)), there(&case)));
    }
    all
}

fn files() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for (index, case) in cases("files").iter().enumerate() {
        let scratch = Scratch::new(&format!("image-layer-file-{index}"));
        let target = scratch.0.join("layer.tar");
        let data = inflated(case.get("data").expect("data"));
        let came = layer_file(
            &data[..],
            &target,
            text(case, "expected"),
            text(case, "blob"),
            &Limits::REVIEWED,
        );
        let came = came.map(|size| {
            let written = std::fs::read(&target).expect("a layer");
            let mode = target.metadata().expect("a layer").permissions().mode() & 0o7777;
            Document::List(vec![
                size.into(),
                digest(&written).into(),
                (mode as usize).into(),
            ])
        });
        all.push((text(case, "name").to_owned(), here(came), there(case)));
    }
    all
}

fn layers() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for (index, case) in cases("layers").iter().enumerate() {
        let scratch = Scratch::new(&format!("image-layer-{index}"));
        let path = scratch.0.join("layer.tar");
        std::fs::write(&path, inflated(case.get("layer").expect("a layer"))).expect("a layer");
        let destination = scratch.0.join("kept");
        std::fs::create_dir(&destination).expect("a directory");
        let came = read_layer(&path, &destination, &Limits::REVIEWED).map(|layer| {
            let entry = |(name, entry): (&String, &Entry)| {
                let (kind, link, kept) = match entry {
                    Entry::Directory => ("directory", "", None),
                    Entry::File(content) => (
                        "file",
                        "",
                        content
                            .strip_prefix(&destination)
                            .ok()
                            .and_then(Path::to_str),
                    ),
                    Entry::Symlink(link) => ("symlink", link.as_str(), None),
                    Entry::Hardlink(link) => ("hardlink", link.as_str(), None),
                    Entry::Whiteout => ("whiteout", "", None),
                };
                (
                    name.clone(),
                    Document::List(vec![
                        kind.into(),
                        link.into(),
                        kept.map(str::to_owned).into(),
                    ]),
                )
            };
            let list = |names: &[String]| {
                Document::List(names.iter().map(|name| name.as_str().into()).collect())
            };
            Document::record([
                (
                    "entries",
                    Document::Record(layer.entries.iter().map(entry).collect()),
                ),
                ("whiteouts", list(&layer.whiteouts)),
                ("opaque", list(&layer.opaque)),
                ("count", layer.count.into()),
                ("kept", listing(&destination)),
            ])
        });
        // The recording keeps a layer's entries in the archive's order.
        let sorted = |text: String| text;
        let recorded = case
            .get("result")
            .and_then(|result| result.get("held"))
            .map(|held| {
                let mut fields = held.fields().expect("a record").to_vec();
                for (name, value) in &mut fields {
                    if name == "entries" {
                        *value = value.clone().sorted();
                    }
                }
                format!("held {}", Document::Record(fields).pretty())
            });
        all.push((
            text(case, "name").to_owned(),
            sorted(here(came)),
            recorded.unwrap_or_else(|| there(case)),
        ));
    }
    all
}

/// A listing with every directory private: the reference made only the
/// last directory of a new chain private.
fn private(listing: &Document) -> Document {
    let entry = |(name, value): &(String, Document)| {
        let directory = value
            .items()
            .is_some_and(|items| items.first() == Some(&"directory".into()));
        let value = if directory {
            Document::List(vec!["directory".into(), 0o700_usize.into()])
        } else {
            value.clone()
        };
        (name.clone(), value)
    };
    Document::Record(
        listing
            .fields()
            .expect("a listing")
            .iter()
            .map(entry)
            .collect(),
    )
}

/// Every whole image: what extracting it came to, with what it left, and
/// its identity.
fn images() -> Vec<(String, String, String)> {
    let all_cases = cases("images");
    let whole = |name: &str| {
        let case = all_cases
            .iter()
            .find(|case| text(case, "name") == name)
            .expect("a case");
        inflated(case.get("image").expect("an image"))
    };
    let mut all = Vec::new();
    for (index, case) in all_cases.iter().enumerate() {
        let name = text(case, "name");
        let scratch = Scratch::new(&format!("image-whole-{index}"));
        let (image, manifest, output) = (
            scratch.0.join("image.tar"),
            scratch.0.join("release.json"),
            scratch.0.join("evidence"),
        );
        if let Some(kept) = case.get("image").filter(|kept| **kept != Document::Null) {
            std::fs::write(&image, inflated(kept)).expect("an image");
        } else if let Some(target) = case.get("imageLink").and_then(Document::text) {
            std::fs::write(scratch.0.join(target), whole("one layer")).expect("an image");
            std::os::unix::fs::symlink(target, &image).expect("a link");
        } else if name == "an archive that is a directory" {
            std::fs::create_dir(&image).expect("a directory");
        }
        if let Some(text) = case.get("manifest").and_then(Document::text) {
            std::fs::write(&manifest, text).expect("a manifest");
        }
        let existed = case.get("existed") == Some(&Document::Bool(true));
        if existed {
            std::fs::create_dir(&output).expect("a directory");
        }
        let came = super::extract(&image, &manifest, &output, &Limits::REVIEWED).map(|report| {
            let written = std::fs::read(output.join("report.json")).expect("a report");
            assert_eq!(written, report.pretty().into_bytes(), "{name}");
            let tree = listing(&output);
            assert_eq!(tree, private(&tree), "{name}");
            let mode = output
                .metadata()
                .expect("the evidence")
                .permissions()
                .mode();
            assert_eq!(mode & 0o7777, 0o700, "{name}");
            Document::record([
                ("report", report.clone()),
                ("written", report),
                ("tree", tree),
            ])
        });
        let recorded = case
            .get("result")
            .and_then(|result| result.get("held"))
            .map(|held| {
                let mut fields = held.fields().expect("a record").to_vec();
                for (field, value) in &mut fields {
                    if field == "tree" {
                        *value = private(value);
                    }
                }
                format!("held {}", Document::Record(fields).pretty())
            });
        all.push((
            name.to_owned(),
            here(came),
            recorded.unwrap_or_else(|| there(case)),
        ));
        let identity = case.get("identity").expect("an identity");
        let said = identity.get("held").map_or_else(
            || format!("RAISED {}", text(identity, "raised")),
            |held| format!("held {}", held.pretty()),
        );
        let came = super::image_identity(&image, &Limits::REVIEWED).map(Document::from);
        all.push((format!("the identity of {name}"), here(came), said));
    }
    all
}

/// Raw headers that the reference's tar library refused in its own
/// words: a block that is no header, a damaged check, a field that cannot
/// be read.
const UNREAD_HEADERS: [(&str, &str); 8] = [
    ("a block that is no header false", "image_tar_header"),
    ("a block that is no header true", "image_tar_header"),
    ("a header with a damaged check false", "image_tar_header"),
    ("a header with a damaged check true", "image_tar_header"),
    (
        "a file whose header says more than is there false",
        "image_tar_header",
    ),
    (
        "a file whose header says more than is there true",
        "image_tar_header",
    ),
    ("a header whose name is no text false", "image_tar_header"),
    ("a header whose name is no text true", "image_tar_header"),
];

/// Layer files that the reference's decompressor refused in its own
/// words.
const UNPACKED: [(&str, &str); 2] = [
    (
        "a packed layer with a wrong check",
        "image_layer_compression",
    ),
    (
        "text after the magic of a packed layer",
        "image_layer_compression",
    ),
];

/// Layers with a name that the host cannot hold.
const UNKEPT: [(&str, &str); 3] = [
    ("a long name in the newer format", "image_filesystem_error"),
    ("a long name in the GNU format", "image_filesystem_error"),
    ("the longest name", "image_filesystem_error"),
];

/// Images that the release verifier refuses, in its own words there and
/// by its codes here, and what the host refuses.
const UNVERIFIED: [(&str, &str); 8] = [
    ("no layer", "archive_layers"),
    (
        "the identity of an archive that is not there",
        "image_filesystem_error",
    ),
    (
        "the identity of an archive that is a directory",
        "image_tar_header",
    ),
    (
        "an archive changed after its manifest",
        "archive_digest_mismatch",
    ),
    ("no manifest", "release_manifest_invalid"),
    ("a manifest that is no JSON", "release_manifest_invalid"),
    ("a manifest of another revision", "release_manifest_invalid"),
    ("evidence where some is already", "image_filesystem_error"),
];

/// Layers with a global extended header that would rename or resize what
/// follows it, or call it sparse. The reference's library applied such a
/// header to every member after it and a container runtime applies none
/// of it, so what the reference scanned was not what would run: here no
/// layer may say it.
const AMBIGUOUS: [&str; 4] = [
    "a global extended header that renames the file after it",
    "a global extended header that renames the files after it",
    "a global extended header that gives the file after it no bytes",
    "a global extended header with a sparse note",
];

/// Hold every case of a section to the recording, but for those that
/// `reworded` names with the code they have here and those that are
/// `refused` here whatever they came to there, and answer how many were
/// held alike, refused alike, refused in other words and refused only here.
fn tallied(
    all: Vec<(String, String, String)>,
    reworded: &[(&str, &str)],
    refused: &[&str],
) -> [usize; 4] {
    let mut tally = [0; 4];
    for (name, here, there) in all {
        if let Some((_, code)) = reworded.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {code}"), "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            tally[2] += 1;
            continue;
        }
        if refused.contains(&name.as_str()) {
            assert_eq!(here, "refused image_global_header", "{name}");
            assert!(!there.starts_with("RAISED "), "{name}");
            tally[3] += 1;
            continue;
        }
        assert_eq!(here, there, "{name}");
        tally[usize::from(here.starts_with("refused "))] += 1;
    }
    tally
}

#[test]
fn every_recorded_name_and_link_target_is_judged_alike() {
    assert_eq!(tallied(names(), &[], &[]), [30, 36, 0, 0]);
    assert_eq!(tallied(targets(), &[], &[]), [30, 20, 0, 0]);
}

#[test]
fn every_recorded_raw_archive_is_walked_alike() {
    assert_eq!(tallied(headers(), &UNREAD_HEADERS, &[]), [21, 33, 8, 0]);
}

#[test]
fn every_recorded_layer_file_is_unpacked_alike() {
    assert_eq!(tallied(files(), &UNPACKED, &[]), [8, 10, 2, 0]);
}

#[test]
fn every_recorded_layer_is_read_alike() {
    assert_eq!(tallied(layers(), &UNKEPT, &AMBIGUOUS), [23, 36, 3, 4]);
}

#[test]
fn every_recorded_image_is_extracted_alike() {
    assert_eq!(tallied(images(), &UNVERIFIED, &[]), [55, 15, 8, 0]);
}
