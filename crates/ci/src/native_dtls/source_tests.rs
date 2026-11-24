//! Owned inert fixtures for the native DTLS controller.

use super::*;
use crate::native_dtls::fixtures::DIAGNOSTIC;

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

/// What the retired helper's two programs answered: the source selection on
/// recorded worker trees and the log assertions on recorded logs, each case
/// bound by the SHA-256 of its canonical JSON description.
const RECORDED: &str = include_str!("recorded_reference.json");

fn recorded(section: &str) -> Vec<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(RECORDED).unwrap();
    let cases = value[section].as_array().unwrap().clone();
    for case in &cases {
        let description = serde_json::to_vec(&case["description"]).unwrap();
        let name = &case["description"]["name"];
        assert_eq!(case["sha256"], crate::io::sha256(&description), "{name}");
    }
    cases
}

/// The bytes of a recorded content: UTF-8 text, or `{"hex": …}`.
fn content(value: &serde_json::Value) -> Vec<u8> {
    if let Some(text) = value.as_str() {
        return text.as_bytes().to_vec();
    }
    let hex = value["hex"].as_str().unwrap();
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap();
    (0..hex.len()).step_by(2).map(byte).collect()
}

/// A private directory below the system's temporary directory, removed on drop.
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "native-dtls-reference-{}-{next}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// The selected build definition for a recorded worker tree, through the
/// same bounded copy the controller makes, or `None` when it refuses.
fn selected_build(files: &serde_json::Map<String, serde_json::Value>) -> Option<Vec<u8>> {
    use crate::release_controller::files::Directory;
    let scratch = Scratch::new();
    let (source, target) = (scratch.0.join("worker"), scratch.0.join("copy"));
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&target).unwrap();
    for (name, value) in files {
        let path = source.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if value.get("directory").is_some() {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, content(value)).unwrap();
        }
    }
    let (source, target) = (
        Directory::open(&source).unwrap(),
        Directory::open(&target).unwrap(),
    );
    let mut snapshot = super::super::snapshot::Snapshot::default();
    let copied = snapshot
        .copy(source.file(), target.file(), "", 0)
        .and_then(|()| snapshot.verify(source.file()));
    let selected = copied.and_then(|()| meson(&snapshot.files)).ok();
    source.finish().unwrap();
    target.finish().unwrap();
    selected
}

#[test]
fn every_recorded_source_selection_is_made_as_the_reference_made_it() {
    let cases = recorded("selections");
    assert_eq!(cases.len(), 25);
    let mut accepted = 0;
    for case in cases {
        let name = &case["description"]["name"];
        let selected = selected_build(case["description"]["files"].as_object().unwrap());
        if case["ending"] == "accepted" {
            accepted += 1;
            let expected = case["meson"].as_str().unwrap().as_bytes().to_vec();
            assert_eq!(selected, Some(expected), "{name}");
        } else {
            assert_eq!(selected, None, "{name}");
        }
    }
    assert_eq!(accepted, 11);
}

#[test]
fn every_recorded_log_assertion_is_made_as_the_reference_made_it() {
    let cases = recorded("logs");
    assert_eq!(cases.len(), 13);
    let mut accepted = 0;
    for case in cases {
        let description = &case["description"];
        let held = assertions(
            &content(&description["dtls"]),
            &content(&description["orderly"]),
        );
        let name = &description["name"];
        assert_eq!(held.is_ok(), case["ending"] == "accepted", "{name}");
        accepted += usize::from(held.is_ok());
    }
    assert_eq!(accepted, 3);
}
