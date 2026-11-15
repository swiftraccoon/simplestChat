//! Replays of `recorded_builds.json`: what the reference in Python made of
//! Cargo's messages, over the fixture of its own tests.

use super::{cargo_build, package_name};
use crate::{
    document::Document,
    native::tests::{names, replay, tallied},
};

const RECORDING: &[u8] = include_bytes!("recorded_builds.json");

fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "build", &|root| {
        let messages = std::fs::read(root.join("cargo-build.json")).expect("messages");
        let graph = cargo_build(root, &messages)?;
        let pair = |(package, directory): &(String, String)| {
            Document::List(vec![package.as_str().into(), directory.as_str().into()])
        };
        let from = |(package, artifact): (&String, &Document)| {
            let manifest = artifact.get("manifest_path").cloned();
            (package.clone(), manifest.unwrap_or(Document::Null))
        };
        Ok(Document::record([
            ("binary", graph.binary.to_str().expect("a path").into()),
            (
                "artifacts",
                names(graph.artifacts.keys().map(String::as_str)),
            ),
            (
                "scripts",
                Document::List(graph.scripts.keys().map(pair).collect()),
            ),
            (
                "manifests",
                Document::Record(graph.artifacts.iter().map(from).collect()),
            ),
        ]))
    })
}

/// Messages that both refuse in other words: the reference in those of
/// its interpreter's readers, and of a field or a file that is not there.
const REWORDED: [(&str, &str); 13] = [
    ("an empty line among the messages", "Invalid JSON"),
    ("a line that is no JSON", "Invalid JSON"),
    ("a message without a reason", "Missing field: reason"),
    (
        "a build script without its directory",
        "Missing field: out_dir",
    ),
    (
        "a build script without its package",
        "Missing field: package_id",
    ),
    (
        "an artifact without its package",
        "Missing field: package_id",
    ),
    ("an artifact without a target", "Missing field: target"),
    (
        "an artifact whose target has no name",
        "Missing field: name",
    ),
    (
        "an artifact whose target has no kind",
        "Missing field: kind",
    ),
    ("the server without a profile", "Missing field: profile"),
    (
        "the server that is not there",
        "Missing file: <root>/target/release/simplestChat",
    ),
    (
        "the server at another path and none at its own",
        "Missing file: <root>/target/release/simplestChat",
    ),
    (
        "another artifact whose target has no name",
        "Missing field: name",
    ),
];

#[test]
fn every_recorded_build_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [14, 41, 13]);
}

#[test]
fn every_recorded_identifier_names_the_same_package() {
    let recording = Document::read(RECORDING).expect("a recording");
    let packages = recording.get("packages").and_then(Document::items);
    let (mut named, mut refused) = (0, 0);
    for package in packages.expect("identifiers") {
        let identifier = package.get("identifier").and_then(Document::text);
        let identifier = identifier.expect("an identifier");
        let result = package.get("result").expect("a result");
        let there = result.get("held").and_then(Document::text);
        let here = package_name(identifier);
        assert_eq!(here.as_ref().ok().copied(), there, "{identifier:?}");
        if let Err(problem) = here {
            let words = result.get("refused").and_then(Document::text);
            assert_eq!(Some(problem.0.as_str()), words, "{identifier:?}");
            refused += 1;
        } else {
            named += 1;
        }
    }
    assert_eq!((named, refused), (10, 12));
}

#[test]
fn the_messages_are_held_to_their_bound_at_its_edge() {
    // The messages of the whole build, with one line padded: spaces after
    // a message are no part of it.
    let recording = Document::read(RECORDING).expect("a recording");
    let scratch = crate::scratch::Scratch::new("native-build-bound");
    let base = recording.get("base").and_then(Document::fields);
    crate::native::tests::written(&scratch.0, base.expect("the fixture"));
    let messages = std::fs::read(scratch.0.join("cargo-build.json")).expect("messages");
    let padded = |size: usize| {
        let mut data = b"{\"reason\": \"compiler-message\"}".to_vec();
        data.resize(size - messages.len() - 1, b' ');
        data.push(b'\n');
        data.extend(&messages);
        assert_eq!(data.len(), size);
        cargo_build(&scratch.0, &data).map(|graph| graph.artifacts.len())
    };
    assert_eq!(super::MAX_MESSAGES, 32 * 1024 * 1024);
    assert_eq!(padded(super::MAX_MESSAGES), Ok(4));
    let budget = crate::vendor::Problem("Cargo message budget exceeded".to_owned());
    assert_eq!(padded(super::MAX_MESSAGES + 1), Err(budget));
}

#[test]
fn a_path_that_does_not_begin_at_the_root_is_below_no_directory() {
    // The file is there from where the tests run, and it lies below the
    // directory when it is read from there.
    let here = std::env::current_dir().expect("a working directory");
    let escaped = "Native artifact escaped its declared directory".to_owned();
    let relative = std::path::Path::new("src/lib.rs");
    assert!(here.join(relative).is_file());
    let refused = super::beneath(relative, &here.join("src"));
    assert_eq!(refused, Err(crate::vendor::Problem(escaped)));
    let whole = here.join(relative);
    let real = whole.canonicalize().expect("the file");
    assert_eq!(super::beneath(&whole, &here.join("src")), Ok(real));
}
