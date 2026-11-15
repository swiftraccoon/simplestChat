//! Replays of `recorded_vendor.json`: what the reference in Python made of
//! paths, digests, addresses and whole records, with the words of every
//! complaint. Where the reference failed with words of its interpreter or
//! of a library, `REWORDED` gives the words here.

use super::{
    Bounds, Problem, Verdict,
    compare::{self, Files},
    manifest::{self, Change, Manifest, Tree},
    wrap,
};
use crate::{document::Document, scratch::base64};

const RECORDING: &[u8] = include_bytes!("recorded_vendor.json");

/// Complaints that are worded otherwise here, by the case or the value: a
/// character that is no ASCII is shown as its escape, a port that is no
/// port is refused as every other address is, and bytes that are no JSON
/// are said to be none without the interpreter's account of where.
const REWORDED: [(&str, &str); 8] = [
    ("https://[example]/a", NO_ADDRESS),
    ("https://[::1/a", NO_ADDRESS),
    ("https://::1]/a", NO_ADDRESS),
    ("caf\u{e9}", "Unsafe or noncanonical path: 'caf\\xe9'"),
    (
        "https://example.invalid:99999/a",
        "Source URLs must be credential-free HTTPS URLs",
    ),
    (
        "https://example.invalid:port/a",
        "Source URLs must be credential-free HTTPS URLs",
    ),
    ("no JSON", "Invalid JSON"),
    ("no bytes", "Invalid JSON"),
];
const NO_ADDRESS: &str = "Source URLs must be credential-free HTTPS URLs";

/// An address the reference took and that is refused here: between
/// brackets only an IPv6 address is a host.
const STRICTER: [&str; 1] = ["https://[v1.x]/a"];

/// Wrap files that the reference's library refused in its own words, and
/// one that is no text: here they are said to be no configuration, or no
/// UTF-8 text.
const UNREAD_WRAPS: [(&str, &str); 7] = [
    ("no section", NO_CONFIGURATION),
    ("the section named twice", NO_CONFIGURATION),
    ("a key named twice", NO_CONFIGURATION),
    ("a key named twice in other letters", NO_CONFIGURATION),
    ("a line without a value", NO_CONFIGURATION),
    ("a line that begins with a value", NO_CONFIGURATION),
    (
        "bytes that are no UTF-8",
        "Native wrap is no UTF-8 text: vendor/tree/subprojects/native.wrap",
    ),
];
const NO_CONFIGURATION: &str =
    "Native wrap is no configuration: vendor/tree/subprojects/native.wrap";

/// Vendored files the reference failed on in its interpreter's words:
/// native component metadata that is no JSON, and metadata that the record
/// lists and that is not there, which is said here as every missing file
/// is.
const UNCOVERED: [(&str, &str); 2] = [
    ("native components that are no JSON", "Invalid JSON"),
    (
        "native components that are not there",
        "Unlisted or missing vendor files: ['vendor/native-components.json']",
    ),
];

fn recording() -> Document {
    Document::read(RECORDING).expect("the recording is a document")
}

fn field<'a>(document: &'a Document, name: &str) -> &'a Document {
    document.get(name).unwrap_or(&Document::Null)
}

fn text<'a>(document: &'a Document, name: &str) -> &'a str {
    field(document, name)
        .text()
        .unwrap_or_else(|| panic!("no text {name}"))
}

fn list<'a>(document: &'a Document, name: &str) -> &'a [Document] {
    field(document, name)
        .items()
        .unwrap_or_else(|| panic!("no list {name}"))
}

/// Hold a verdict to what the reference did in the case `name`, and answer
/// whether it held, was refused alike, or is reworded here.
fn alike<T>(name: &str, verdict: &Verdict<T>, recorded: &Document) -> usize {
    worded(&REWORDED, name, verdict, recorded)
}

fn worded<T>(
    reworded: &[(&str, &str)],
    name: &str,
    verdict: &Verdict<T>,
    recorded: &Document,
) -> usize {
    let said = recorded.get("refused").or_else(|| recorded.get("said"));
    let said = said.and_then(Document::text);
    let here = verdict.as_ref().err().map(|Problem(words)| words.as_str());
    if let Some((_, words)) = reworded.iter().find(|(other, _)| *other == name) {
        assert_eq!(here, Some(*words), "{name}");
        assert!(said.is_some_and(|said| said != *words), "{name}");
        return 2;
    }
    assert_eq!(here, said, "{name}");
    usize::from(here.is_some())
}

#[test]
fn paths_digests_and_addresses_are_judged_alike() {
    let recording = recording();
    let mut tally = [[0; 3]; 3];
    let mut stricter = 0;
    for case in list(&recording, "paths") {
        let value = text(case, "value");
        tally[0][alike(
            value,
            &manifest::relative_path(value),
            field(case, "result"),
        )] += 1;
    }
    for case in list(&recording, "digests") {
        let value = field(case, "value");
        let name = value.text().unwrap_or("no text");
        tally[1][alike(name, &manifest::digest(value), field(case, "result"))] += 1;
    }
    for case in list(&recording, "urls") {
        let value = text(case, "value");
        let judged = manifest::https_url(value);
        if STRICTER.contains(&value) {
            assert_eq!(judged, Err(Problem(NO_ADDRESS.to_owned())), "{value}");
            assert!(field(case, "result").get("held").is_some(), "{value}");
            stricter += 1;
            continue;
        }
        tally[2][alike(value, &judged, field(case, "result"))] += 1;
    }
    assert_eq!(tally, [[18, 21, 1], [2, 12, 0], [18, 24, 5]]);
    assert_eq!(stricter, 1);
}

/// A record in the recording's shape.
fn shape(manifest: &Manifest) -> Document {
    let words = |words: &[&str]| Document::List(words.iter().map(|word| (*word).into()).collect());
    let side = |side: &Option<String>| Document::from(side.as_deref());
    let sources = manifest.sources.iter().map(|source| {
        words(&[
            &source.identifier,
            &source.url,
            &source.sha256,
            &source.format,
        ])
    });
    let trees = manifest.trees.iter().map(|tree| {
        let changes = tree.changes.iter().map(|change| {
            Document::List(vec![
                change.path.as_str().into(),
                side(&change.upstream),
                side(&change.vendored),
            ])
        });
        Document::List(vec![
            tree.path.as_str().into(),
            tree.source.as_str().into(),
            tree.prefix.as_str().into(),
            changes.collect::<Vec<_>>().into(),
        ])
    });
    let files = manifest
        .files
        .iter()
        .map(|(path, source)| words(&[path, source]));
    let maintained: Vec<&str> = manifest
        .maintained_files
        .iter()
        .map(String::as_str)
        .collect();
    let wraps = manifest.wraps.iter().map(|wrap| {
        let fallbacks: Vec<&str> = wrap.fallback_urls.iter().map(String::as_str).collect();
        Document::List(vec![
            wrap.path.as_str().into(),
            wrap.source.as_str().into(),
            side(&wrap.patch_source),
            side(&wrap.patch_directory),
            words(&fallbacks),
        ])
    });
    Document::record([
        ("sources", sources.collect::<Vec<_>>().into()),
        ("trees", trees.collect::<Vec<_>>().into()),
        ("files", files.collect::<Vec<_>>().into()),
        ("maintained", words(&maintained)),
        ("wraps", wraps.collect::<Vec<_>>().into()),
    ])
}

#[test]
fn every_recorded_record_is_read_alike() {
    let recording = recording();
    let mut tally = [0; 3];
    for case in list(&recording, "manifests") {
        let name = text(case, "name");
        let read = manifest::parse(&base64(text(case, "data")));
        let recorded = field(case, "result");
        if let (Ok(manifest), Some(held)) = (&read, recorded.get("held")) {
            assert_eq!(&shape(manifest), held, "{name}");
        }
        tally[alike(name, &read, recorded)] += 1;
    }
    assert_eq!(tally, [15, 76, 2]);
}

#[test]
fn every_recorded_wrap_is_held_alike() {
    let recording = recording();
    let mut tally = [0; 3];
    for case in list(&recording, "wraps") {
        let name = text(case, "name");
        let manifest = manifest::parse(&base64(text(case, "manifest"))).expect("a record");
        let declared = &manifest.wraps[0];
        let mut local: Vec<&str> = list(case, "local")
            .iter()
            .map(|path| path.text().expect("a path"))
            .collect();
        local.push(&declared.path);
        let held = wrap::validate(
            declared,
            &manifest,
            &base64(text(case, "wrap")),
            local.into_iter(),
        );
        tally[worded(&UNREAD_WRAPS, name, &held, field(case, "result"))] += 1;
    }
    assert_eq!(tally, [16, 40, 7]);
}

fn files(recorded: &Document) -> Files {
    let fields = recorded.fields().expect("files");
    fields
        .iter()
        .map(|(path, body)| (path.clone(), base64(body.text().expect("bytes"))))
        .collect()
}

#[test]
fn every_recorded_tree_is_compared_alike() {
    let recording = recording();
    let (mut tally, mut edges) = ([0; 3], 0);
    for case in list(&recording, "trees") {
        let name = text(case, "name");
        let side = |side: &Document| side.text().map(str::to_owned);
        let change = |change: &Document| {
            let change = change.items().expect("a change");
            Change {
                path: change[0].text().expect("a path").to_owned(),
                upstream: side(&change[1]),
                vendored: side(&change[2]),
            }
        };
        let tree = Tree {
            path: "vendor/tree".to_owned(),
            source: "upstream".to_owned(),
            prefix: text(case, "prefix").to_owned(),
            changes: list(case, "changes").iter().map(change).collect(),
        };
        let compared = compare::tree(
            &tree,
            &files(field(case, "upstream")),
            &files(field(case, "local")),
            &Bounds::REVIEWED,
        );
        let recorded = field(case, "result");
        if let (Ok(compared), Some(held)) = (&compared, recorded.get("held")) {
            assert_eq!(&compared.report, field(held, "report"), "{name}");
            assert_eq!(compared.diff, base64(text(held, "diff")), "{name}");
            let errors: Vec<Document> = compared
                .errors
                .iter()
                .map(|error| error.as_str().into())
                .collect();
            assert_eq!(&Document::List(errors), field(held, "errors"), "{name}");
        }
        // The differences are no more than their bound.
        let whole = compared.as_ref().map_or(0, |compared| compared.diff.len());
        if whole > 0 {
            let bounded = |differences: usize| {
                let bounds = Bounds {
                    differences,
                    ..Bounds::REVIEWED
                };
                let (upstream, local) = (field(case, "upstream"), field(case, "local"));
                compare::tree(&tree, &files(upstream), &files(local), &bounds)
            };
            assert_eq!(bounded(whole), compared, "{name}");
            let budget = Problem("Vendor diff budget exceeded".to_owned());
            assert_eq!(bounded(whole - 1), Err(budget), "{name}");
            edges += 1;
        }
        tally[alike(name, &compared, recorded)] += 1;
    }
    assert_eq!((tally, edges), ([16, 4, 0], 12));
}

#[test]
fn every_recorded_set_of_files_is_covered_alike() {
    let recording = recording();
    let mut tally = [0; 3];
    for case in list(&recording, "coverage") {
        let name = text(case, "name");
        let manifest = manifest::parse(&base64(text(case, "manifest"))).expect("a record");
        let covered = compare::coverage(&manifest, &files(field(case, "local")));
        tally[worded(&UNCOVERED, name, &covered, field(case, "result"))] += 1;
    }
    assert_eq!(tally, [2, 23, 2]);
}

#[test]
fn the_first_field_named_again_is_the_one_that_is_said() {
    use crate::document::Unread;
    let read = |text: &str| Document::read(text.as_bytes());
    let twice = |name: &str| Err(Unread::Duplicate(name.to_owned()));
    assert_eq!(read(r#"{"a": 1, "b": 2, "b": 3, "a": 4}"#), twice("b"));
    assert_eq!(read(r#"{"a": 1, "b": 2, "a": 3, "b": 4}"#), twice("a"));
    assert_eq!(read(r#"{"a": {"c": 1, "c": 2}, "a": 3}"#), twice("c"));
    let said = manifest::parse(br#"{"sources": [], "trees": [], "trees": [], "sources": []}"#);
    assert_eq!(said, Err(Problem("Duplicate JSON key: trees".to_owned())));
}
