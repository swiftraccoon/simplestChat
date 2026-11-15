//! Replays of the recorded lint of new migrations.
//!
//! `recorded_migrations.json` holds what the gate's reference in Python
//! did: which mode it chose for a migration, what it read of the
//! maintenance connection's timeouts in the server's source, and whole
//! lints with a scanner that logs how it was run and answers from files.
//! Where the reference failed with an exception of its interpreter, `named`
//! gives the code here. `STRICTER` and `OTHERWISE` name the cases that end
//! differently here and say how.

use super::{lint, mode, prefix};
use crate::{
    context::{Context, Failure, Limits, Outcome},
    fast::Programs,
    scratch::{Scratch, asked, content, kept, said, scripted, written},
};
use serde_json::{Value, json};
use std::{ffi::OsString, fs::DirBuilder, os::unix::fs::DirBuilderExt, path::PathBuf};

const RECORDED: &str = include_str!("recorded_migrations.json");

/// Migrations that end differently here, with the code of each. A double
/// quote after the marker is refused wherever it stands: a quoted name may
/// hold a single quote, and the reference read that as the start of a text
/// and took a second statement inside it for one index. So is a carriage
/// return: the database ends a comment there, and the reference read on to
/// the line feed and took a second statement for part of the comment. A
/// text that never
/// ends is not read: the reference's pattern passed over its opening quote
/// and took what followed for statements. And the marker's word ends
/// before anything but an ASCII letter, a digit or an underscore, where
/// the reference let any letter or digit continue it.
const STRICTER: [(&str, &str); 15] = [
    (
        "a comment ended by a carriage return before a second statement",
        "migration_concurrent_index_syntax",
    ),
    (
        "a carriage return between two words",
        "migration_concurrent_index_syntax",
    ),
    (
        "carriage returns in the body",
        "migration_concurrent_index_syntax",
    ),
    ("a quoted column", "migration_concurrent_index_syntax"),
    (
        "a quoted name that holds a quote, a second statement and a comment",
        "migration_concurrent_index_syntax",
    ),
    (
        "a double quote in a text",
        "migration_concurrent_index_syntax",
    ),
    (
        "a double quote in a comment",
        "migration_concurrent_index_syntax",
    ),
    // The reference refused this one too, as no index.
    (
        "a concurrent index with a quoted name",
        "migration_concurrent_index_syntax",
    ),
    (
        "a text that never ends",
        "migration_requires_one_concurrent_index",
    ),
    (
        "a text that ends in two quotes",
        "migration_requires_one_concurrent_index",
    ),
    (
        "a text that never ends before a comment",
        "migration_requires_one_concurrent_index",
    ),
    (
        "a lone quote before a text",
        "migration_requires_one_concurrent_index",
    ),
    (
        "three quotes and a statement end",
        "migration_requires_one_concurrent_index",
    ),
    (
        "the marker with a letter outside ASCII after it on a later line",
        "migration_transaction_marker_position",
    ),
    (
        "the marker with a digit outside ASCII after it on a later line",
        "migration_transaction_marker_position",
    ),
];

/// Lints that end differently here, with the code of each. A report of
/// the scanner is JSON in UTF-8 without a byte order mark and without the
/// numbers that are none, both of which the reference read: an empty
/// report after a mark passed there, and a report of such a number was
/// findings there.
const OTHERWISE: [(&str, &str); 2] = [
    ("a report after a byte order mark", "invalid_tool_json"),
    ("a report of a number that is none", "invalid_tool_json"),
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

/// The code here for what the reference raised.
fn named<'a>(code: &'a str, group: &str) -> &'a str {
    match (code, group) {
        ("UnicodeDecodeError", "contexts" | "a source") => {
            "migration_timeout_configuration_missing"
        }
        ("UnicodeDecodeError", _) => "migration_not_text",
        ("FileNotFoundError", _) => "tool_entry_missing",
        ("FileExistsError", _) => "tool_entry_unavailable",
        ("JSONDecodeError", _) => "invalid_tool_json",
        (code, _) => code,
    }
}

/// How one group of cases ended: answered alike, refused alike under a
/// code of the reference's own, refused where the reference raised an
/// exception, and ended differently as a table says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize, usize);

/// Compare what was answered here with the recorded result.
fn settle(
    case: &Value,
    group: &str,
    here: Result<Value, String>,
    tables: &[(&str, &str)],
    tally: &mut Tally,
) {
    let (name, there) = (text(&case["name"]), &case["result"]);
    let code = there.get("code").map(text);
    if let Some((_, expected)) = tables.iter().find(|(named, _)| *named == name) {
        assert_ne!(code, Some(*expected), "{name}");
        assert_eq!(here, Err((*expected).to_owned()), "{name}");
        tally.3 += 1;
    } else if let Some(code) = code {
        assert_eq!(here, Err(named(code, group).to_owned()), "{name}");
        if named(code, group) == code {
            tally.1 += 1;
        } else {
            tally.2 += 1;
        }
    } else {
        assert_eq!(here.as_ref(), Ok(there), "{name}");
        tally.0 += 1;
    }
}

#[test]
fn every_recorded_mode_is_chosen_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["modes"]) {
        let chosen = mode(&content(&case["sql"]));
        let here = chosen.map(|inside| json!({"transactional": inside}));
        let here = here.map_err(|fault| fault.0.to_owned());
        settle(case, "modes", here, &STRICTER, &mut tally);
    }
    assert_eq!(tally, Tally(74, 74, 1, 15));
}

#[test]
fn every_recorded_source_is_read_alike() {
    let recording = recorded();
    let scratch = Scratch::new("mt");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["contexts"]).iter().enumerate() {
        let root = scratch.0.join(index.to_string());
        std::fs::create_dir_all(root.join("src")).expect("a root");
        if !case["source"].is_null() {
            std::fs::write(root.join("src/db.rs"), content(&case["source"])).expect("a source");
        }
        let read = prefix(&root, Limits::REVIEWED.report);
        let here = read.map(|prefix| json!({"prefix": prefix}));
        let here = here.map_err(|fault| fault.0.to_owned());
        settle(case, "contexts", here, &[], &mut tally);
    }
    assert_eq!(tally, Tally(27, 55, 2, 0));
}

/// The scripted scanner of a case.
struct Tools(PathBuf);

impl Programs for Tools {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Ok(self.0.join(name))
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite(name))
    }

    fn python(&self) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite("python"))
    }
}

#[test]
fn every_recorded_lint_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("ml");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["checks"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        for part in ["root", "tools"] {
            std::fs::create_dir_all(place.join(part)).expect("a directory");
        }
        let place = place.canonicalize().expect("a place");
        let [root, tools, evidence] = ["root", "tools", "evidence"].map(|part| place.join(part));
        DirBuilder::new()
            .mode(0o700)
            .create(&evidence)
            .expect("a private directory");
        written(&root, case["files"].as_object().expect("files"));
        let log = place.join("log");
        scripted(&tools, &log, "squawk", "squawk", &case["answers"]);
        let ambient = |variable: &str| -> Option<OsString> {
            match variable {
                "PATH" => Some("/usr/bin:/bin".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        let mut context =
            Context::new(&root, &evidence, Limits::REVIEWED, &ambient).expect("a context");
        let migrations: Vec<String> = list(&case["migrations"])
            .iter()
            .map(|name| text(name).to_owned())
            .collect();
        let outcome = lint(&mut context, &Tools(tools), &migrations);
        let checks: Vec<Value> = context
            .checks()
            .iter()
            .map(|note| {
                let field = |(name, value): &(&str, Value)| match *name {
                    "elapsedSeconds" => ((*name).to_owned(), value.is_f64().into()),
                    _ => ((*name).to_owned(), value.clone()),
                };
                Value::Object(note.iter().map(field).collect())
            })
            .collect();
        assert_eq!(json!(checks), case["checks"], "{name}");
        let place = place.to_string_lossy().into_owned();
        assert_eq!(kept(&evidence, &["home"]), case["kept"], "{name}");
        let read = said(&evidence, &place, &["home"], |below| {
            !below.ends_with(".stdout")
        });
        assert_eq!(read, case["texts"], "{name}");
        assert_eq!(asked(&log, &place), case["asked"], "{name}");
        let here = outcome
            .map(|()| json!({}))
            .map_err(|failure| failure.to_string());
        // Of a lint, the text that is none is the migration or the source.
        let group = match name.contains("a source that is no UTF-8") {
            true => "a source",
            false => "checks",
        };
        settle(case, group, here, &OTHERWISE, &mut tally);
    }
    assert_eq!(tally, Tally(14, 19, 8, 2));
}
