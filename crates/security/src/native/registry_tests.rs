//! Replays of `recorded_registries.json`: what the reference in Python
//! made of the wrapper crate of the build, over the fixture of its own
//! tests.

use super::registry_component;
use crate::{
    document::Document,
    native::{
        read,
        tests::{built, graph, replay, tallied},
    },
    vendor::manifest,
};

const RECORDING: &[u8] = include_bytes!("recorded_registries.json");

fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "registry", &|root| {
        let graph = graph(root)?;
        let record = read(&root.join("vendor/native-components.json"))?;
        let record = Document::read(&record).map_err(manifest::unread)?;
        let wrapper = record.get("registry_component").expect("the wrapper");
        built(root, |build| registry_component(build, &graph, wrapper))
    })
}

/// Wrappers that both refuse in other words: the reference in those of
/// its interpreter's system errors and readers, and of a field or a file
/// of the crate that is not there.
const REWORDED: [(&str, &str); 15] = [
    (
        "a wrapper record without a crate version",
        "Missing field: crate_version",
    ),
    (
        "the wrapper from a manifest that is not there",
        "Missing file: <root>/cargo-home/registry/src/index/aws-lc-sys-0.45.0/Missing.toml",
    ),
    (
        "no archive of the wrapper in the cache",
        "Unreadable file: <root>/cargo-home/registry/cache/index/aws-lc-sys-0.45.0.crate",
    ),
    (
        "an unpacked link",
        "Unreadable file: <root>/cargo-home/registry/src/index/aws-lc-sys-0.45.0/link",
    ),
    (
        "a wrapper without its original manifest",
        "Missing crate file: Cargo.toml.orig",
    ),
    (
        "an original manifest that is no TOML",
        "Invalid TOML: Cargo.toml.orig",
    ),
    (
        "an original manifest without a package",
        "Missing field: package",
    ),
    (
        "an original manifest without metadata",
        "Missing field: metadata",
    ),
    (
        "an original manifest without the wrapper's metadata",
        "Missing field: aws-lc-sys",
    ),
    (
        "an original manifest without a commit",
        "Missing field: commit-hash",
    ),
    (
        "a wrapper without its bundled header",
        "Missing crate file: aws-lc/include/openssl/base.h",
    ),
    (
        "a bundled header that is no text",
        "No text: aws-lc/include/openssl/base.h",
    ),
    (
        "a wrapper whose manifest is no TOML",
        "Invalid TOML: Cargo.toml",
    ),
    (
        "a wrapper whose manifest has no package",
        "Missing field: package",
    ),
    (
        "a wrapper without its manifest",
        "Missing file: <root>/cargo-home/registry/src/index/aws-lc-sys-0.45.0/Cargo.toml",
    ),
];

#[test]
fn every_recorded_wrapper_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [4, 29, 15]);
}
