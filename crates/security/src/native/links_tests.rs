//! Replays of `recorded_links.json`: what the reference in Python made of
//! the libraries that the build scripts linked, over the fixture of its
//! own tests, with the fixture's `system` directory as the system root.

use super::link_archives;
use crate::{
    document::Document,
    native::{
        read,
        tests::{built, graph, replay, tallied},
    },
    vendor::manifest,
};

const RECORDING: &[u8] = include_bytes!("recorded_links.json");

fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "links", &|root| {
        let graph = graph(root)?;
        let record = read(&root.join("vendor/native-components.json"))?;
        let record = Document::read(&record).map_err(manifest::unread)?;
        let system = root.join("system");
        let (records, runtime) =
            built(root, |build| link_archives(build, &graph, &record, &system))?;
        Ok(Document::record([
            ("records", Document::List(records)),
            ("runtime", runtime.display().to_string().into()),
        ]))
    })
}

/// Builds that both refuse in other words: the reference in those of its
/// interpreter, of a field or a directory that is not there.
const REWORDED: [(&str, &str); 2] = [
    (
        "a build script without its libraries",
        "Missing field: linked_libs",
    ),
    (
        "a build script whose directory is not there",
        "Missing file: <root>/target/release/build/openssl-sys/out-absent",
    ),
];

#[test]
fn every_recorded_set_of_links_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [8, 24, 2]);
}
