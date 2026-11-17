//! Replays of `recorded_coverage.json`: what `build/security_codeql.py`
//! made of query results and of its command lines.

use super::{command, verify};
use crate::{
    codeql::tests::{Tally, items, recording, text, whole},
    document::Document,
    scratch::{Scratch, base64},
};
use std::{fs, os::unix::fs::symlink, path::PathBuf};

const RECORDED: &[u8] = include_bytes!("recorded_coverage.json");

/// The refusal here of what the reference's interpreter raised.
fn raised(class: &str, _: &str) -> String {
    match class {
        "KeyError" => "codeql_coverage_field",
        "JSONDecodeError" | "UnicodeDecodeError" => "codeql_coverage_json",
        "FileNotFoundError" => "tool_entry_missing",
        "OSError" => "tool_entry_unavailable",
        "IsADirectoryError" => "nonregular_tool_file",
        other => panic!("unexpected {other}"),
    }
    .to_owned()
}

/// What the reference's reader took and this one refuses: a number that is
/// not finite, and a document in UTF-16.
const STRICTER: [(&str, &str); 2] = [
    ("a field that is not a number", "codeql_coverage_json"),
    ("a document in UTF-16", "codeql_coverage_json"),
];

/// The query result of `case`, built in `directory` as the recorder built
/// it.
fn built(directory: &std::path::Path, case: &Document) -> PathBuf {
    let path = directory.join("result.json");
    if case.get("missing").is_some() {
        return path;
    }
    if case.get("directory").is_some() {
        fs::create_dir(&path).expect("a directory");
        return path;
    }
    if let Some(linked) = case.get("link") {
        let target = directory.join("target.json");
        fs::write(&target, base64(linked.text().expect("bytes"))).expect("a target");
        symlink(&target, &path).expect("a link");
        return path;
    }
    let data = match case.get("pad") {
        Some(pad) => {
            let mut data = base64(text(pad, "base64"));
            let size = usize::try_from(whole(pad, "size")).expect("a size");
            data.resize(size, b' ');
            data
        }
        None => base64(text(case, "base64")),
    };
    fs::write(&path, data).expect("a result");
    path
}

#[test]
fn every_recorded_query_result_is_judged_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "verify") {
        let scratch = Scratch::new("coverage");
        let path = built(&scratch.0, case);
        let came = verify(&path)
            .map(Document::from)
            .map_err(|fault| fault.0.to_owned());
        tally.judge(case, came, &raised, &STRICTER);
    }
    let expected = Tally {
        held: 15,
        refused: 25,
        raised: 10,
        stricter: 2,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_command_line_is_answered_alike() {
    let recorded = recording(RECORDED);
    let scratch = Scratch::new("coverage-command");
    let place = scratch.0.to_string_lossy().into_owned();
    let row = |name: &str| format!("[\"{name}\"]");
    let rows: Vec<String> = super::REQUIRED.iter().map(|name| row(name)).collect();
    let document =
        |rows: &[String]| format!("{{\"#select\": {{\"tuples\": [{}]}}}}", rows.join(", "));
    let mut good = rows.clone();
    good.push(row("src/a.cpp"));
    fs::write(scratch.0.join("good.json"), document(&good)).expect("a result");
    fs::write(scratch.0.join("bad.json"), document(&rows[..3])).expect("a result");
    let mut answered = 0;
    for case in items(&recorded, "commands") {
        let argv: Vec<String> = items(case, "argv")
            .iter()
            .map(|word| word.text().expect("a word").replace("<place>", &place))
            .map(|word| match word.strip_prefix('/') {
                Some(_) => word,
                None if word.is_empty() || word.starts_with('-') => word,
                None => format!("{place}/{word}"),
            })
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = command(&argv, &mut out, &mut err);
        let result = case.get("result").expect("a result");
        let name = text(case, "name");
        assert_eq!(i128::from(status), whole(result, "status"), "{name}");
        assert_eq!(
            String::from_utf8(out).expect("text"),
            text(result, "stdout"),
            "{name}"
        );
        assert_eq!(
            String::from_utf8(err).expect("text"),
            text(result, "stderr"),
            "{name}"
        );
        answered += 1;
    }
    assert_eq!(answered, 11);
}
