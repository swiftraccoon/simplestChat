//! Replays of `recorded_openssl.json`: what the reference in Python made
//! of the OpenSSL that was built, over the fixture of its own tests. The
//! archives are those that the build linked before the case changed
//! anything; the recording keeps them with each case.

use super::openssl_build_evidence;
use crate::{
    document::Document,
    native::{
        read,
        tests::{replay_with, tallied},
    },
    vendor::manifest,
};

const RECORDING: &[u8] = include_bytes!("recorded_openssl.json");

fn recorded() -> Vec<(String, String, String)> {
    replay_with(RECORDING, "openssl", &|root, case| {
        let record = read(&root.join("vendor/native-components.json"))?;
        let record = Document::read(&record).map_err(manifest::unread)?;
        let component = record.get("openssl").expect("the component");
        let shown = root.to_str().expect("a path");
        let archives = case.get("archives").expect("archives").pretty();
        let archives = Document::read(archives.replace("<root>", shown).as_bytes());
        let archives = archives.expect("archives");
        let archives = archives.items().expect("a list");
        openssl_build_evidence(&root.join("openssl"), component, archives)
    })
}

/// Installations that both refuse in other words: the reference in those
/// of its interpreter's system errors and readers, and of a field that is
/// not there.
const REWORDED: [(&str, &str); 9] = [
    (
        "a version header that is no text",
        "No text: <root>/openssl/include/openssl/opensslv.h",
    ),
    (
        "no version header",
        "Unreadable file: <root>/openssl/include/openssl/opensslv.h",
    ),
    (
        "a configuration that is no text",
        "No text: <root>/openssl/include/openssl/configuration.h",
    ),
    (
        "no configuration",
        "Unreadable file: <root>/openssl/include/openssl/configuration.h",
    ),
    (
        "no record of what is disabled",
        "Unreadable file: <root>/openssl/share/simplestchat/openssl-disabled.txt",
    ),
    (
        "no library after it was linked",
        "Unreadable file: <root>/openssl/lib/libcrypto.a",
    ),
    ("a linked archive without a size", "Missing field: size"),
    (
        "a linked archive without a provider",
        "Missing field: provider",
    ),
    (
        "a linked archive without a library",
        "Missing field: library",
    ),
];

#[test]
fn every_recorded_installation_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [14, 27, 9]);
}
