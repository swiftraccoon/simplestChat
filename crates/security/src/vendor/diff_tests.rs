//! Replays of `recorded_diff.json`: what the reference in Python rendered
//! as the difference of two versions of a file, for written cases and for
//! seeded random edits of short, long and repetitive files.

use super::{base64, lines, rendered};
use crate::{digest, document::Document, scratch};

const RECORDING: &[u8] = include_bytes!("recorded_diff.json");

#[test]
fn every_recorded_difference_is_rendered_alike() {
    assert_eq!(rendered_alike(RECORDING, "cases"), (126, 0));
}

/// The deleted tests' differences: a binary change shown whole in base64,
/// an empty file that is added, and a missing line end.
#[test]
fn the_reference_tests_differences_are_rendered_alike() {
    let recording = crate::vendor::tests::TESTS;
    assert_eq!(rendered_alike(recording, "diffs"), (3, 0));
}

/// Render every case of the section, and count those held to the text and
/// those held to its length and digest.
fn rendered_alike(recording: &[u8], section: &str) -> (usize, usize) {
    let recording = Document::read(recording).expect("a recording");
    let cases = recording
        .get(section)
        .and_then(Document::items)
        .expect("cases");
    let (mut text, mut long) = (0, 0);
    for case in cases {
        let field = |name: &str| case.get(name).and_then(Document::text);
        let name = field("name").expect("a name");
        let (old, new) = (
            field("old").map(scratch::base64),
            field("new").map(scratch::base64),
        );
        let written = rendered(
            field("path").expect("a path"),
            old.as_deref(),
            new.as_deref(),
        );
        if let Some(recorded) = field("text") {
            assert_eq!(String::from_utf8_lossy(&written), recorded, "{name}");
            text += 1;
        } else {
            let bytes = case.get("bytes").and_then(Document::whole);
            assert_eq!(Some(written.len() as i128), bytes, "{name}");
            assert_eq!(Some(digest(&written).as_str()), field("sha256"), "{name}");
            long += 1;
        }
    }
    (text, long)
}

#[test]
fn lines_end_where_the_reference_ends_them() {
    assert_eq!(lines(""), Vec::<&str>::new());
    assert_eq!(lines("a"), ["a"]);
    assert_eq!(lines("a\n"), ["a\n"]);
    assert_eq!(lines("a\r\nb\rc\n\rd"), ["a\r\n", "b\r", "c\n", "\r", "d"]);
    assert_eq!(lines("\n\n"), ["\n", "\n"]);
    assert_eq!(
        lines("a\u{b}b\u{c}c\u{1c}d\u{1d}e\u{1e}f\u{85}g\u{2028}h\u{2029}i"),
        [
            "a\u{b}",
            "b\u{c}",
            "c\u{1c}",
            "d\u{1d}",
            "e\u{1e}",
            "f\u{85}",
            "g\u{2028}",
            "h\u{2029}",
            "i"
        ]
    );
    assert_eq!(
        lines("a\u{1f}b\u{a0}c\u{2027}d\te"),
        ["a\u{1f}b\u{a0}c\u{2027}d\te"]
    );
}

#[test]
fn bytes_are_written_in_base64_as_they_are_read() {
    for bytes in [
        &b""[..],
        b"a",
        b"ab",
        b"abc",
        b"abcd",
        &[0, 255, 254, 1, 128],
    ] {
        assert_eq!(scratch::base64(&base64(bytes)), bytes);
    }
    assert_eq!(base64(b"a"), "YQ==");
    assert_eq!(base64(b"ab"), "YWI=");
    assert_eq!(base64(&[255, 239, 190]), "/+++");
}
