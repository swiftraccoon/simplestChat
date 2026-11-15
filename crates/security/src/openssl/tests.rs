//! Replays of the recorded freshness check of the pinned OpenSSL.
//!
//! `recorded_openssl.json` holds what the check's reference in Python did:
//! what its reader of HTML saw in two hundred pages and which release it
//! chose, and whole checks of a snapshot's native manifest with a `curl`
//! that answers from a file and a clock that stands still. Where the reader
//! here takes a page, it sees exactly what the reference saw. The tables
//! name every page it does not take, by the reason; the reference read each
//! of them, and what it would have chosen from a page built to exploit a
//! difference is not this check's to guess.

use super::{
    FETCH, MAX_LINKS, MAX_PAGE, SOURCE, check, closing, comment, declaration, latest, listed, tag,
};
use crate::{
    context::{Context, Failure, Limits, Outcome},
    fast::Programs,
    scratch::{Scratch, content},
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

const RECORDED: &str = include_str!("recorded_openssl.json");

/// An anchor's address with a reference to a character: a number, or a name
/// that a semicolon ends. The reference's reader put the character in its
/// place, and a point, a colon or two letters can be written so.
const REFERENCES: [&str; 15] = [
    "a point written as a number",
    "a point written as a hexadecimal number",
    "a point written as a hexadecimal number in capitals",
    "a point written as a number without its semicolon",
    "a point written by its name",
    "a colon and slashes written by their names",
    "letters written as numbers",
    "a digit written as a number",
    "a reference that is no character",
    "a reference to nothing known",
    "a hyphen written by a name of another character",
    "a bare ampersand and a semicolon in another address",
    "a bare ampersand before a name as long as a reference may be",
    "two letters written as one",
    "a reference in an address without quotes",
];

/// Elements, sections and instructions with rules of their own for what
/// follows them: frames, examples, plain text, marked sections,
/// declarations other than the document's type, and instructions.
const SECTIONS: [&str; 14] = [
    "an anchor in a frame",
    "an anchor in an example",
    "an anchor where embedding is off",
    "an anchor where frames are off",
    "an anchor after plain text begins",
    "an anchor in a marked section",
    "an anchor in a marked section inside a drawing",
    "an anchor in a declaration",
    "an anchor in a declaration of another kind",
    "an anchor in a processing instruction",
    "a marked section with a greater-than sign",
    "a marked section with a greater-than sign inside a drawing",
    "a processing instruction with a greater-than sign",
    "a comment that begins with one dash",
];

/// A comment, a text element, a tag or a quote that the page ends before:
/// the reference passed over the rest of the page, which may be a page cut
/// short.
const UNENDED: [&str; 14] = [
    "a comment with space in its end",
    "a comment that never ends",
    "a comment that never ends, with an end written otherwise",
    "a script that never ends",
    "a style that never ends",
    "a title that never ends",
    "a tag that never ends",
    "a tag that ends with the page, without quotes",
    "a quote that never ends",
    "an end tag that never ends",
    "a script's end without its greater-than sign",
    "a value without quotes that begins with a quote and never ends it",
    "an anchor cut off after its name",
    "an anchor cut off in its address",
];

/// Tags written as only a lenient reader takes them: quotes and signs where
/// a name belongs, a value without quotes that holds a quote, a backtick or
/// an equals sign, two equals signs, and an end tag that is more than its
/// name.
const LOOSE: [&str; 22] = [
    "two equals signs",
    "an anchor in the value of an attribute without quotes",
    "an anchor in an end tag",
    "a script's end with attributes",
    "quotes with nothing between attributes",
    "text in quotes after the address",
    "an equals sign alone after the address",
    "a less-than sign inside a tag",
    "an empty end tag",
    "an end tag with space before its name",
    "a title's end with space and attributes",
    "a script's end before a slash",
    "a value without quotes that holds a quote",
    "an attribute named by an equals sign",
    "an attribute named after an equals sign",
    "a value without quotes and nothing after the equals sign",
    "an attribute whose name holds a single quote",
    "an attribute whose name holds a backtick",
    "a value without quotes that holds a single quote",
    "a value without quotes that holds a backtick",
    "a value without quotes that holds an equals sign",
    "an element whose name holds a quote",
];

/// Bytes that part nothing: a null byte anywhere, and in a tag outside
/// quotes anything but printable ASCII and the five spaces.
const BYTES: [&str; 10] = [
    "a null byte in the tag",
    "a null byte in the address",
    "a null byte in the page",
    "a space outside ASCII between attributes",
    "a space outside ASCII after a value without quotes",
    "a space outside ASCII after the tag's name",
    "a vertical tab between attributes",
    "a unit separator between attributes",
    "a line separator between attributes",
    "an attribute named with a Kelvin sign",
];

/// Checks the reference passed and that are refused here: a native manifest
/// is JSON in UTF-8 without a byte order mark.
const UNREAD: [(&str, &str); 1] = [(
    "a native manifest after a byte order mark",
    "openssl_native_manifest_format",
)];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

/// The code here for what the reference raised: a page that is no UTF-8 is
/// a page in another form, a manifest its readers of JSON refused is a
/// manifest in another form, and a manifest that is not there is a file
/// that is missing.
fn named(code: &str) -> &str {
    match code {
        "UnicodeDecodeError" => "openssl_release_page_form",
        "ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError" => {
            "openssl_native_manifest_format"
        }
        "FileNotFoundError" => "tool_entry_missing",
        code => code,
    }
}

#[test]
fn every_recorded_page_is_read_alike_or_not_taken() {
    let recording = recorded();
    let refused = [&REFERENCES[..], &SECTIONS, &UNENDED, &LOOSE, &BYTES].concat();
    let (mut alike, mut stricter, mut undecoded) = (0, 0, 0);
    for case in list(&recording["pages"]) {
        let name = text(&case["name"]);
        let page = content(&case["content"]);
        let chosen = match latest(&page, text(&case["pinned"])) {
            Ok(latest) => json!({"latest": latest}),
            Err(fault) => json!({"code": fault.0}),
        };
        if refused.contains(&name) {
            // The reference read the page, whatever it then made of it.
            assert!(case["seen"].get("code").is_none(), "{name}");
            let seen = std::str::from_utf8(&page).map(listed);
            let form = super::FORM;
            assert_eq!(seen, Ok(Err(form)), "{name}");
            assert_eq!(chosen, json!({"code": form.0}), "{name}");
            stricter += 1;
            continue;
        }
        let Ok(page) = std::str::from_utf8(&page) else {
            assert_eq!(
                case["seen"],
                json!({"code": "UnicodeDecodeError"}),
                "{name}"
            );
            assert_eq!(
                chosen,
                json!({"code": named("UnicodeDecodeError")}),
                "{name}"
            );
            undecoded += 1;
            continue;
        };
        let seen = match listed(page) {
            Ok(listed) => json!({"links": listed.links, "versions": listed.versions}),
            Err(fault) => json!({"code": fault.0}),
        };
        assert_eq!(seen, case["seen"], "{name}");
        assert_eq!(chosen, case["result"], "{name}");
        alike += 1;
    }
    assert_eq!((alike, stricter, undecoded), (149, 75, 2));
}

#[test]
fn each_part_of_a_page_is_as_long_as_it_is_written() {
    // Nothing a page lists depends on the byte after a part, so the lengths
    // are shown on the parts themselves: each sample is one part and an `x`.
    let whole = |written: &str| written.len() - 1;
    for written in [
        "<!---->x",
        "<!-->x",
        "<!--->x",
        "<!-- a -->x",
        "<!-- a --!>x",
    ] {
        assert_eq!(comment(written), Ok(whole(written)), "{written}");
    }
    // A comment ends at its first end, whichever kind comes first.
    assert_eq!(comment("<!-- a --!> b -->x"), Ok("<!-- a --!>".len()));
    assert_eq!(comment("<!-- a --> b --!>x"), Ok("<!-- a -->".len()));
    for written in ["<!DOCTYPE html>x", "<!doctype html PUBLIC \"-//x\">x"] {
        assert_eq!(declaration(written), Ok(whole(written)), "{written}");
    }
    for written in ["</a>x", "</my-element \n>x"] {
        assert_eq!(closing(written), Ok(whole(written)), "{written}");
    }
    for (written, closes) in [
        ("<a>x", false),
        ("<br/>x", true),
        ("<a href=\"u\" />x", true),
        ("<a href=u>x", false),
        ("<a\nhref = 'u'\n>x", false),
    ] {
        let tag = tag(written).expect("a tag");
        assert_eq!(
            (tag.length, tag.closes),
            (whole(written), closes),
            "{written}"
        );
    }
}

/// The scripted `curl` of one check.
struct Tools(PathBuf);

impl Programs for Tools {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite(name))
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        Ok(self.0.join(name))
    }

    fn python(&self) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite("python3"))
    }
}

/// A `curl` that logs its arguments in hexadecimal, each ended by a zero
/// byte, and answers from a file with the status another file holds.
fn program(directory: &Path, log: &Path) -> String {
    let (directory, log) = (directory.display(), log.display());
    [
        "#!/bin/sh".to_owned(),
        "printf 'curl\\t%s\\n' \\".to_owned(),
        format!("  \"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\" >> '{log}'"),
        format!("cat '{directory}/curl.0.out'"),
        format!("exit \"$(cat '{directory}/curl.0.status')\""),
        String::new(),
    ]
    .join("\n")
}

/// The calls the scripted program logged: its name and its arguments.
fn calls(log: &Path) -> Value {
    let logged = std::fs::read_to_string(log).unwrap_or_default();
    let call = |line: &str| {
        let (name, arguments) = line.split_once('\t').expect("a logged call");
        let bytes: Vec<u8> = (0..arguments.len() / 2)
            .map(|at| u8::from_str_radix(&arguments[2 * at..2 * at + 2], 16).expect("hex"))
            .collect();
        let arguments = bytes.strip_suffix(&[0]).expect("an end");
        let mut call = vec![name.to_owned()];
        call.extend(
            arguments
                .split(|byte| *byte == 0)
                .map(|argument| String::from_utf8(argument.to_vec()).expect("text")),
        );
        call
    };
    json!(logged.lines().map(call).collect::<Vec<_>>())
}

#[test]
fn every_recorded_check_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("os");
    let (mut passed, mut refused, mut stricter) = (0, 0, 0);
    for (index, case) in list(&recording["checks"]).iter().enumerate() {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        let [root, output, tools, vendor] =
            ["root", "output", "tools", "snapshot/vendor"].map(|part| base.join(part));
        for directory in [&root, &output, &tools, &vendor] {
            std::fs::create_dir_all(directory).expect("a directory");
        }
        if !case["native"].is_null() {
            let manifest = vendor.join("native-components.json");
            std::fs::write(manifest, content(&case["native"])).expect("a manifest");
        }
        let log = base.join("log");
        std::fs::write(tools.join("curl.0.out"), content(&case["said"][1])).expect("an answer");
        std::fs::write(
            tools.join("curl.0.status"),
            format!("{}\n", case["said"][0]),
        )
        .expect("a status");
        let curl = tools.join("curl");
        std::fs::write(&curl, program(&tools, &log)).expect("a program");
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).expect("a mode");
        let ambient = |variable: &str| -> Option<OsString> {
            match variable {
                "PATH" => Some("/usr/bin:/bin".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        let mut context =
            Context::new(&root, &output, Limits::REVIEWED, &ambient).expect("a context");
        let observed = &case["observed"];
        let seconds = observed["seconds"].as_u64().expect("seconds");
        let millionths = observed["microseconds"].as_u64().expect("millionths");
        let moment = UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_micros(millionths);
        let snapshot = base.join("snapshot");
        let judged = check(&mut context, &Tools(tools), &snapshot, moment);
        let failure = judged.err().map(|failure| failure.to_string());
        let lines: Vec<Value> = context
            .checks()
            .iter()
            .map(|check| {
                let mut line = serde_json::Map::new();
                for (field, value) in check {
                    let (field, value) = match *field {
                        "elapsedSeconds" => ("timed", true.into()),
                        field => (field, value.clone()),
                    };
                    assert!(line.insert(field.to_owned(), value).is_none(), "{name}");
                }
                line.into()
            })
            .collect();
        let receipt = output.join("openssl-freshness.json");
        let kept = std::fs::read_to_string(&receipt).ok();
        let mode = receipt.metadata().ok();
        let mode = mode.map(|metadata| metadata.permissions().mode() & 0o777);
        if let Some((_, code)) = UNREAD.iter().find(|(case, _)| *case == name) {
            assert!(case["result"].get("code").is_none(), "{name}");
            assert_eq!(failure.as_deref(), Some(*code), "{name}");
            assert_eq!((kept, calls(&log)), (None, json!([])), "{name}");
            stricter += 1;
            continue;
        }
        let expected = case["result"].get("code").map(|code| named(text(code)));
        assert_eq!(failure.as_deref(), expected, "{name}");
        assert_eq!(json!(lines), case["checks"], "{name}");
        assert_eq!(json!(kept), case["receipt"], "{name}");
        assert_eq!(json!(mode), case["receiptMode"], "{name}");
        assert_eq!(calls(&log), case["calls"], "{name}");
        let mut evidence: Vec<String> = std::fs::read_dir(&output)
            .expect("the output")
            .map(|entry| {
                let entry = entry.expect("an entry").file_name();
                entry.into_string().expect("text")
            })
            .filter(|entry| entry != "home")
            .collect();
        evidence.sort();
        assert_eq!(json!(evidence), case["evidence"], "{name}");
        if expected.is_none() {
            passed += 1;
        } else {
            refused += 1;
        }
    }
    assert_eq!((passed, refused, stricter), (18, 36, 1));
}

#[test]
fn the_bounds_are_the_ones_the_reference_keeps() {
    let recording = recorded();
    let limits = &recording["limits"];
    assert_eq!(json!(MAX_PAGE), limits["MAX_PAGE"]);
    assert_eq!(json!(MAX_LINKS), limits["MAX_LINKS"]);
    assert_eq!(json!(SOURCE), limits["SOURCE_URL"]);
    // What the fetch may take, print and complain: read from the call the
    // reference made. The page is printed with four bytes of status after it.
    let fetch = json!({"timeout": FETCH.0.as_secs(), "stdout": MAX_PAGE + 4, "stderr": FETCH.1});
    assert_eq!(fetch, limits["fetch"]);
}
