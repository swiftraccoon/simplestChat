//! Replays of `recorded_rust_sources.json`: what the reference made of
//! Rust texts, of their literal includes, of committed include closures
//! read through a `git show` of the recorded commit, and of include paths.

use super::{Token, include_path, included_sources, literal_includes, tokens};
use crate::{
    codeql::tests::{Committed, Tally, items, planted, recording, refusal, text, whole},
    document::Document,
    scratch::Scratch,
};
use std::path::Path;

const RECORDED: &[u8] = include_bytes!("recorded_rust_sources.json");

/// The refusal here of what the reference's interpreter raised.
fn raised(class: &str, _: &str) -> String {
    match class {
        "FileNotFoundError" => "tool_entry_missing",
        "UnicodeDecodeError" => "codeql_rust_source_text",
        other => panic!("unexpected {other}"),
    }
    .to_owned()
}

/// The text of `case`: given whole, or one piece repeated so many times.
fn written(case: &Document) -> String {
    if let Some(given) = case.get("text").and_then(Document::text) {
        return given.to_owned();
    }
    let times = usize::try_from(whole(case, "times")).expect("a count");
    text(case, "repeat").repeat(times)
}

/// A token as the recording writes one: its kind and its text.
fn shown(token: &Token) -> Document {
    let (kind, value) = match token {
        Token::Identifier(value) => ("identifier", value.as_str()),
        Token::Literal(value) => ("literal", value.as_str()),
        Token::Opaque(value) => ("opaque", value.as_str()),
        Token::Other(value) => ("other", value.as_str()),
    };
    Document::List(vec![kind.into(), value.into()])
}

#[test]
fn every_recorded_text_is_tokenized_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    let mut counted = 0;
    for case in items(&recorded, "tokens") {
        let nowhere = Path::new("/nowhere");
        let found = tokens(&written(case)).map_err(|stop| refusal(&stop, nowhere));
        let result = case.get("result").expect("a result");
        if let Some(count) = result.get("count") {
            let length = found.expect("tokens").len();
            assert_eq!(
                Some(length as i128),
                count.whole(),
                "{}",
                text(case, "name")
            );
            counted += 1;
            continue;
        }
        let found = found.map(|found| Document::List(found.iter().map(shown).collect()));
        tally.judge(case, found, &raised, &[]);
    }
    let expected = Tally {
        held: 53,
        refused: 9,
        raised: 0,
        stricter: 0,
    };
    assert_eq!((tally, counted), (expected, 2));
}

#[test]
fn every_recorded_text_is_held_to_its_literal_includes_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "includes") {
        let found = literal_includes(&written(case));
        let found = found
            .map(|paths| Document::List(paths.into_iter().map(Document::from).collect()))
            .map_err(|stop| refusal(&stop, Path::new("/nowhere")));
        tally.judge(case, found, &raised, &[]);
    }
    let expected = Tally {
        held: 20,
        refused: 22,
        raised: 0,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_include_closure_is_bound_alike() {
    let recorded = recording(RECORDED);
    let revision = text(&recorded, "revision");
    let mut tally = Tally::default();
    for case in items(&recorded, "closures") {
        let scratch = Scratch::new("codeql-rust-closure");
        let place = &scratch.0;
        planted(place, case.get("tree").expect("a tree"));
        let git = Committed::new(place, revision, case, place);
        let found = included_sources(place, revision, text(case, "start"), &git);
        let found = found
            .map(|hashes| {
                let fields = hashes
                    .into_iter()
                    .map(|(name, hash)| (name, Document::from(hash)));
                Document::Record(fields.collect())
            })
            .map_err(|stop| refusal(&stop, place));
        tally.judge(case, found, &raised, &[]);
        assert_eq!(
            git.taken(),
            *case.get("asked").expect("asked"),
            "{}",
            text(case, "name")
        );
    }
    let expected = Tally {
        held: 8,
        refused: 14,
        raised: 4,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_include_path_is_taken_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "paths") {
        let scratch = Scratch::new("codeql-rust-path");
        planted(&scratch.0, case.get("tree").expect("a tree"));
        let (selected, relative) = (text(case, "selected"), text(case, "relative"));
        let found = include_path(&scratch.0, selected, relative);
        let found = found
            .map(Document::from)
            .map_err(|stop| refusal(&stop, &scratch.0));
        tally.judge(case, found, &raised, &[]);
    }
    let expected = Tally {
        held: 13,
        refused: 8,
        raised: 0,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_reviewed_rust_file_of_the_checkout_includes_nothing() {
    // The exact Rust reviews stay valid under the include binding only while
    // none of their files includes another.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = std::collections::BTreeSet::new();
    for review in [
        "security/codeql-review-2026-09-30.json",
        "security/codeql-native-review-2026-09-30.json",
    ] {
        let raw = std::fs::read(root.join(review)).expect("a review");
        let raw = Document::read(&raw).expect("a review document");
        for alert in items(&raw, "alerts") {
            let path = text(alert, "path");
            if path.ends_with(".rs")
                && !path.starts_with("vendor/")
                && alert.get("includedSources").is_none()
            {
                paths.insert(path.to_owned());
            }
        }
    }
    assert!(!paths.is_empty());
    for name in paths {
        let source = std::fs::read_to_string(root.join(&name)).expect("a reviewed file");
        assert_eq!(literal_includes(&source), Ok(Vec::new()), "{name}");
    }
}
