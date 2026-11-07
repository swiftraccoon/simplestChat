//! Owned inert fixtures for the native DTLS controller.

use super::*;
use crate::ci::native_dtls::fixtures::DIAGNOSTIC;

fn files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            "meson.build".into(),
            b"production\r\nmediasoup_worker_test = executable(\r\n".to_vec(),
        ),
        (
            "test/src/one.cpp".into(),
            b"[dtls] [media-diagnostics]".to_vec(),
        ),
        (
            "test/src/sub/two.cpp".into(),
            b"[bandwidth-constraints]".to_vec(),
        ),
        ("test/src/ignored space.cpp".into(), b"unrelated".to_vec()),
    ])
}
const SELECTED: [&str; 4] = [
    "test/src/one.cpp",
    "test/src/sub/two.cpp",
    "test/src/testHelpers.cpp",
    "test/src/tests.cpp",
];
fn selected(names: &[&str]) -> Vec<u8> {
    let mut text = String::from("production\ntest_sources = [\n");
    for name in names {
        text.push_str(&format!("  '{name}',\n"));
    }
    text.push_str("]\n\nmediasoup_worker_test = executable(\n");
    text.into_bytes()
}
#[test]
fn tagged_selection_keeps_every_tagged_source_and_refuses_ambiguous_inputs() {
    for case in 0..9 {
        let mut files = files();
        let expected = match case {
            1 => {
                files.insert("test/src/extra.cpp".into(), b"[dtls]".to_vec());
                let [one, two, helpers, tests] = SELECTED;
                Some(selected(&["test/src/extra.cpp", one, two, helpers, tests]))
            }
            2 => {
                files.insert("test/src/.cpp".into(), b"[dtls]".to_vec());
                None
            }
            3 => {
                files.insert("test/src/private space.cpp".into(), b"[dtls]".to_vec());
                None
            }
            4 => {
                files.remove("test/src/sub/two.cpp");
                None
            }
            5 => {
                files.insert("meson.build".into(), b"no anchor".to_vec());
                None
            }
            6 => {
                files.insert("meson.build".into(), ANCHOR.repeat(2).into_bytes());
                None
            }
            7 => {
                files.insert("test/src/unicode.cpp".into(), vec![255]);
                None
            }
            8 => {
                files.insert(
                    "test/src/stray.cpp".into(),
                    b"[dtls]\r[media-diagnostics]".to_vec(),
                );
                let [one, two, helpers, tests] = SELECTED;
                Some(selected(&[one, "test/src/stray.cpp", two, helpers, tests]))
            }
            _ => Some(selected(&SELECTED)),
        };
        assert_eq!(meson(&files).ok(), expected, "case{case}");
    }
}
#[test]
fn fixed_log_assertions_preserve_warning_absence_and_complete_context_bytes() {
    assert!(assertions(DIAGNOSTIC.as_bytes(), b"quiet").is_ok());
    assert!(assertions(DIAGNOSTIC.as_bytes(), b"RTC::DtlsTransport::close warning").is_err());
    assert!(
        assertions(
            DIAGNOSTIC
                .replace("mediaObserved:1", "mediaObserved:0")
                .as_bytes(),
            b""
        )
        .is_err()
    );
    assert!(assertions(b"unrelated", b"").is_err());
}
