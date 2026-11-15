//! Replays of `recorded_licences.json`: what the reference in Python made
//! of the licences of the build's Rust packages, over the fixture of its
//! own tests, and of single packages.

use super::{inherit_workspace_package, package_license, rust_licenses};
use crate::{
    document::Document,
    native::{
        artifact::binary_dependencies,
        read,
        tests::{built, graph, replay, tallied},
    },
    vendor::{Verdict, compare::Files, manifest},
};

const RECORDING: &[u8] = include_bytes!("recorded_licences.json");

fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "licence", &|root| {
        let integrity = manifest::parse(&read(&root.join("vendor/integrity.json"))?)?;
        let graph = graph(root)?;
        let (_, embedded) = binary_dependencies(&graph.binary)?;
        let licences = built(root, |build| {
            rust_licenses(build, &graph, &integrity, &embedded)
        });
        licences.map(Document::List)
    })
}

/// Builds that both refuse in other words: the reference in those of its
/// interpreter's system errors and readers, and of a field that is not
/// there.
const REWORDED: [(&str, &str); 7] = [
    (
        "a registry package without its archive",
        "Unreadable file: <root>/cargo-home/registry/cache/index/openssl-sys-0.9.117.crate",
    ),
    (
        "a registry package without a checksum",
        "Missing field: checksum",
    ),
    (
        "a registry package whose archive has no manifest",
        "Missing file: <root>/cargo-home/registry/src/index/openssl-sys-0.9.117/Cargo.toml",
    ),
    (
        "a registry package whose manifest is no TOML",
        "Invalid TOML: Cargo.toml",
    ),
    (
        "a registry package whose manifest has no package",
        "Missing field: package",
    ),
    (
        "the server with a licence file that is not there",
        "Missing file: <root>/LICENSE",
    ),
    (
        "the server's manifest without a package",
        "Missing field: package",
    ),
];

#[test]
fn every_recorded_set_of_licences_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [13, 41, 7]);
}

/// What a single case came to here and what the recording says of it.
fn judged(came: Verdict<Document>, result: &Document) -> (String, String) {
    let here = match came {
        Ok(held) => format!("held {}", held.pretty()),
        Err(problem) => format!("refused {problem}"),
    };
    let held = result
        .get("held")
        .map(|held| format!("held {}", held.pretty()));
    let refused = result.get("refused").and_then(Document::text);
    (
        here,
        held.unwrap_or_else(|| format!("refused {}", refused.expect("a refusal"))),
    )
}

fn parsed(case: &Document, name: &str) -> Document {
    let text = case.get(name).and_then(Document::text).expect("a value");
    Document::read(text.as_bytes()).expect("a document")
}

#[test]
fn every_recorded_package_declares_its_licence_alike() {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("pure").and_then(|pure| pure.get("licences"));
    let files: Files = [("LICENSE", "l"), ("docs/LICENSE", "d")]
        .into_iter()
        .map(|(path, body)| (path.to_owned(), body.as_bytes().to_vec()))
        .collect();
    let (mut held, mut refused) = (0, 0);
    for case in cases.and_then(Document::items).expect("cases") {
        let package = parsed(case, "package");
        let (here, there) = judged(
            package_license(&package, &files),
            case.get("result").expect("a result"),
        );
        assert_eq!(here, there, "{}", package.pretty());
        if here.starts_with("held ") {
            held += 1;
        } else {
            refused += 1;
        }
    }
    assert_eq!((held, refused), (12, 12));
}

/// The one package that the reference filled in from the workspace and
/// that is left as it is here: its field says `workspace = 1`, which the
/// reference's interpreter took for true.
const OTHERWISE: &str = "{\"name\": \"x\", \"version\": {\"workspace\": 1}}";

#[test]
fn every_recorded_package_takes_from_its_workspace_alike() {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("pure").and_then(|pure| pure.get("inherited"));
    let (mut held, mut refused, mut otherwise) = (0, 0, 0);
    for case in cases.and_then(Document::items).expect("cases") {
        let (package, root) = (parsed(case, "package"), parsed(case, "root"));
        let came = inherit_workspace_package(&package, &root);
        let (here, there) = judged(came, case.get("result").expect("a result"));
        if case.get("package").and_then(Document::text) == Some(OTHERWISE) {
            assert_eq!(here, format!("held {}", package.pretty()));
            assert_ne!(here, there);
            otherwise += 1;
            continue;
        }
        assert_eq!(here, there, "{}", package.pretty());
        if here.starts_with("held ") {
            held += 1;
        } else {
            refused += 1;
        }
    }
    assert_eq!((held, refused, otherwise), (5, 5, 1));
}

#[test]
fn the_reviewed_bounds_of_the_licences_are_the_references() {
    assert_eq!((super::MAX_RECORDS, super::MAX_LICENSE), (20000, 4096));
}
