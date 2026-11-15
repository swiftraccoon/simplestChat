//! Replays of `recorded_binaries.json`: what the reference in Python made
//! of the server and the graph it carries, over the fixture of its own
//! tests.

use super::binary_dependencies;
use crate::{
    document::Document,
    native::tests::{replay, tallied},
};

const RECORDING: &[u8] = include_bytes!("recorded_binaries.json");

fn recorded() -> Vec<(String, String, String)> {
    replay(RECORDING, "binary", &|root| {
        let server = root.join("target/release/simplestChat");
        let (binary, metadata) = binary_dependencies(&server)?;
        Ok(Document::record([
            ("binary", binary),
            ("metadata", metadata),
        ]))
    })
}

/// Servers that both refuse in other words: the reference in those of
/// its interpreter's system errors.
const REWORDED: [(&str, &str); 3] = [
    (
        "no server",
        "Unreadable file: <root>/target/release/simplestChat",
    ),
    (
        "a server that is a link",
        "Unreadable file: <root>/target/release/simplestChat",
    ),
    (
        "a server that is a directory",
        "Missing or excessive native artifact",
    ),
];

#[test]
fn every_recorded_server_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [2, 6, 3]);
}

#[test]
fn a_static_library_holds_its_own_members() {
    let scratch = crate::scratch::Scratch::new("native-archive");
    let refused = |words: &str| Err(crate::vendor::Problem(words.to_owned()));
    let thin = "Native library must be a regular self-contained ar archive";
    for (name, content, held) in [
        ("whole.a", &b"!<arch>\nmembers"[..], true),
        ("only.a", b"!<arch>\n", true),
        ("thin.a", b"!<thin>\nmembers", false),
        ("short.a", b"!<arch>", false),
        ("other.a", b"members!<arch>\n", false),
    ] {
        let path = scratch.0.join(name);
        std::fs::write(&path, content).expect("a file");
        let record = super::record_hash(&path, true);
        if held {
            let record = record.expect("an archive");
            assert_eq!(record.get("size"), Some(&content.len().into()), "{name}");
            let sha256 = record.get("sha256").and_then(Document::text);
            assert_eq!(sha256, Some(crate::digest(content).as_str()), "{name}");
            let shown = path.display().to_string();
            assert_eq!(
                record.get("path").and_then(Document::text),
                Some(shown.as_str())
            );
        } else {
            assert_eq!(record, refused(thin), "{name}");
        }
        // Any file with bytes has a record when no library is asked for.
        assert!(super::record_hash(&path, false).is_ok(), "{name}");
    }
    // A library of more than one block of the reader begins once.
    let large = scratch.0.join("large.a");
    let mut content = b"!<arch>\n".to_vec();
    content.resize(2 * 1024 * 1024 + 9, 0);
    std::fs::write(&large, &content).expect("a file");
    let record = super::record_hash(&large, true).expect("an archive");
    assert_eq!(record.get("size"), Some(&content.len().into()));
    // A link to a library is none.
    let link = scratch.0.join("link.a");
    std::os::unix::fs::symlink("whole.a", &link).expect("a link");
    let unreadable = format!("Unreadable file: {}", link.display());
    assert_eq!(super::record_hash(&link, true), refused(&unreadable));
    assert_eq!(super::record_hash(&link, false), refused(&unreadable));
}

#[test]
fn a_file_of_the_build_is_held_to_its_bound_at_its_edge() {
    let scratch = crate::scratch::Scratch::new("native-bound");
    let path = scratch.0.join("file");
    std::fs::write(&path, vec![b'x'; 3 * 1024 * 1024 + 5]).expect("a file");
    let most = 3 * 1024 * 1024 + 5;
    let record = super::hashed(&path, false, most).expect("a record");
    assert_eq!(record.get("size"), Some(&(most as usize).into()));
    let excess = crate::vendor::Problem("Missing or excessive native artifact".to_owned());
    assert_eq!(super::hashed(&path, false, most - 1), Err(excess));
    assert_eq!(super::MAX_ARTIFACT, 512 * 1024 * 1024);
}
