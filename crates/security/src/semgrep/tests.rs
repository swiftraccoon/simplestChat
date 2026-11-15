//! Replays of the recorded scan with reviewed local rules.
//!
//! `recorded_semgrep.json` holds what the scanner's reference in Python
//! did: how it read rule packs, built the engine's targets, held the
//! engine's report to completeness, read the annotations of fixtures,
//! chose the sources in scope and placed its evidence; and whole checks in
//! a real repository with an engine that answers the golden run and the
//! source run from files. Where the reference failed with an exception of
//! its interpreter, its command printed the one general code, and `named`
//! gives that code here. A document after a byte order mark is refused
//! here and was read there.

use super::{
    Engine, MAX_CONFIG, MAX_FILES, MAX_LINE, MAX_RULES, answered, check, expectations, findings,
    new_output, rules, scope, targets,
};
use crate::{
    Fault,
    context::{Context, Limits},
    scope::require_local_vendor,
    scratch::{Scratch, content, host_git, repository, written},
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::BTreeSet,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const RECORDED: &str = include_str!("recorded_semgrep.json");

/// Documents the reference read and that are refused here: a pack and a
/// report are JSON in UTF-8 without a byte order mark.
const MARKED: [&str; 2] = [
    "a pack after a byte order mark",
    "a report after a byte order mark",
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

pub(super) fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

pub(super) fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

pub(super) fn strings(value: &Value) -> Vec<String> {
    list(value)
        .iter()
        .map(|item| text(item).to_owned())
        .collect()
}

/// The code here for what the reference raised. An exception of its
/// interpreter is the general failure, as its command printed it; a file
/// that is a link or is not there keeps the code of the bounded reader.
fn named<'a>(code: &'a str, name: &str) -> &'a str {
    match code {
        "OSError" => "tool_entry_unavailable",
        "FileNotFoundError" if name == "no rules" => "tool_entry_missing",
        "ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError" | "TypeError"
        | "UnicodeDecodeError" | "FileNotFoundError" => "semgrep_check_failed",
        code => code,
    }
}

/// How one group of cases ended: read alike, refused alike under a code of
/// the reference's own, refused where the reference raised an exception,
/// and refused only here.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize, usize);

/// Compare what was read here with the recorded result.
fn settle(name: &str, here: crate::Result<Value>, there: &Value, tally: &mut Tally) {
    let fault = here.as_ref().err().map(|fault| fault.0);
    if MARKED.contains(&name) {
        assert!(there.get("code").is_none(), "{name}");
        assert_eq!(fault, Some("semgrep_check_failed"), "{name}");
        tally.3 += 1;
    } else if let Some(code) = there.get("code").map(text) {
        assert_eq!(fault, Some(named(code, name)), "{name}");
        if named(code, name) == code {
            tally.1 += 1;
        } else {
            tally.2 += 1;
        }
    } else {
        assert_eq!(here.as_ref().ok(), Some(there), "{name}");
        tally.0 += 1;
    }
}

/// Write the files of a recorded tree: text, a link, or a directory.
pub(super) fn tree(root: &Path, files: &Value) {
    let files = files.as_object().expect("files");
    for (path, spec) in files {
        if spec.get("directory").is_some() {
            std::fs::create_dir_all(root.join(path)).expect("a directory");
        }
    }
    let plain = files
        .iter()
        .filter(|(_, spec)| spec.get("directory").is_none());
    written(
        root,
        &plain
            .map(|(path, spec)| (path.clone(), spec.clone()))
            .collect(),
    );
}

fn shown(found: &BTreeSet<super::Finding>) -> Vec<Value> {
    let coordinates = |item: &super::Finding| json!([item.rule, item.path, item.line]);
    found.iter().map(coordinates).collect()
}

#[test]
fn every_recorded_pack_is_read_alike() {
    let recording = recorded();
    let scratch = Scratch::new("sp");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["packs"]).iter().enumerate() {
        let path = scratch.0.join(format!("{index}.json"));
        std::fs::write(&path, content(&case["content"])).expect("a pack");
        if let Some(mode) = case["mode"].as_u64() {
            let mode = std::fs::Permissions::from_mode(u32::try_from(mode).expect("a mode"));
            std::fs::set_permissions(&path, mode).expect("a mode that is set");
        }
        let read = rules(&path).map(|found| json!({"identifiers": found}));
        settle(text(&case["name"]), read, &case["result"], &mut tally);
    }
    assert_eq!(tally, Tally(13, 40, 10, 1));
}

#[test]
fn every_recorded_list_of_targets_is_built_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["targets"]) {
        let read = match case["many"].as_u64() {
            Some(count) => {
                let paths: Vec<String> =
                    (0..count).map(|index| format!("src/f{index}.py")).collect();
                targets(&paths).map(|value| json!({"count": value[1].as_array().map(Vec::len)}))
            }
            None => targets(&strings(&case["paths"])).map(|value| json!({"targets": value})),
        };
        settle(text(&case["name"]), read, &case["result"], &mut tally);
    }
    assert_eq!(tally, Tally(5, 19, 0, 0));
}

#[test]
fn every_recorded_report_is_held_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["reports"]) {
        let identifiers: BTreeSet<String> = strings(&case["identifiers"]).into_iter().collect();
        let read = findings(
            &content(&case["content"]),
            &strings(&case["paths"]),
            &identifiers,
            text(&case["version"]),
        );
        let read = read.map(|found| json!({"findings": shown(&found)}));
        settle(text(&case["name"]), read, &case["result"], &mut tally);
    }
    assert_eq!(tally, Tally(14, 47, 32, 1));
}

#[test]
fn every_recorded_fixture_is_read_alike() {
    let recording = recorded();
    let scratch = Scratch::new("sf");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["fixtures"]).iter().enumerate() {
        let root = scratch.0.join(index.to_string());
        std::fs::create_dir_all(&root).expect("a root");
        tree(&root, &case["files"]);
        let identifiers: BTreeSet<String> = strings(&case["identifiers"]).into_iter().collect();
        let read = expectations(&root, &identifiers).map(|(positive, negative)| {
            json!({"positive": shown(&positive), "negative": shown(&negative)})
        });
        settle(text(&case["name"]), read, &case["result"], &mut tally);
    }
    assert_eq!(tally, Tally(22, 32, 2, 0));
}

#[test]
fn every_recorded_scope_is_chosen_alike() {
    let recording = recorded();
    let scratch = Scratch::new("ss");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["scopes"]).iter().enumerate() {
        let root = scratch.0.join(index.to_string());
        std::fs::create_dir_all(&root).expect("a root");
        tree(&root, &case["files"]);
        let read = scope(&root, case["includeVendor"] == true).map(|paths| json!({"paths": paths}));
        settle(text(&case["name"]), read, &case["result"], &mut tally);
    }
    assert_eq!(tally, Tally(8, 2, 0, 0));
}

#[test]
fn every_recorded_output_is_placed_alike() {
    let recording = recorded();
    let scratch = Scratch::new("so");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["outputs"]).iter().enumerate() {
        let place = scratch.0.join(index.to_string());
        std::fs::create_dir_all(place.join("root")).expect("a root");
        let place = place.canonicalize().expect("a place");
        for made in list(&case["made"]) {
            std::fs::create_dir_all(place.join(text(made))).expect("a directory");
        }
        for link in list(&case["links"]) {
            std::os::unix::fs::symlink(text(&link[1]), place.join(text(&link[0]))).expect("a link");
        }
        let made = new_output(&place.join("root"), &place.join(text(&case["output"])));
        let read = made.map(|created| {
            let mode = created.metadata().expect("made").permissions().mode() & 0o777;
            let below = created.strip_prefix(&place).expect("below the place");
            json!({"created": below.to_string_lossy(), "mode": mode})
        });
        settle(text(&case["name"]), read, &case["result"], &mut tally);
    }
    assert_eq!(tally, Tally(7, 10, 1, 0));
}

/// An engine that logs its arguments in hexadecimal, each ended by a zero
/// byte, and where it runs, and answers its calls in order from files.
fn engine(directory: &Path, log: &Path) -> String {
    let (directory, log) = (directory.display(), log.display());
    [
        "#!/bin/sh".to_owned(),
        format!("count=$(cat '{directory}/engine.count')"),
        format!("echo $((count + 1)) > '{directory}/engine.count'"),
        "printf '%s\\t%s\\n' \\".to_owned(),
        "  \"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\" \"$(pwd -P)\" \\"
            .to_owned(),
        format!("  >> '{log}'"),
        format!("cat '{directory}/engine'.$count.out"),
        format!("exit \"$(cat '{directory}/engine'.$count.status)\""),
        String::new(),
    ]
    .join("\n")
}

/// Write the engine into `tools` with the recorded `answers` to its calls,
/// and answer where it is.
pub(super) fn scripted(tools: &Path, log: &Path, answers: &Value) -> PathBuf {
    for (number, answer) in list(answers).iter().enumerate() {
        let said = tools.join(format!("engine.{number}.out"));
        std::fs::write(said, content(&answer[1])).expect("an answer");
        let status = tools.join(format!("engine.{number}.status"));
        std::fs::write(status, format!("{}\n", answer[0])).expect("a status");
    }
    std::fs::write(tools.join("engine.count"), "0\n").expect("a count");
    let program = tools.join("semgrep-core");
    std::fs::write(&program, engine(tools, log)).expect("an engine");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    program
}

/// The calls the engine logged, with the place of the case written as the
/// recording writes it.
pub(super) fn asked(log: &Path, place: &str) -> Value {
    let logged = std::fs::read_to_string(log).unwrap_or_default();
    let call = |line: &str| {
        let (arguments, directory) = line.split_once('\t').expect("a logged call");
        let bytes: Vec<u8> = (0..arguments.len() / 2)
            .map(|at| u8::from_str_radix(&arguments[2 * at..2 * at + 2], 16).expect("hex"))
            .collect();
        let arguments = bytes.strip_suffix(&[0]).expect("an end");
        let argv: Vec<String> = arguments
            .split(|byte| *byte == 0)
            .map(|argument| String::from_utf8_lossy(argument).replace(place, "<place>"))
            .collect();
        json!({"argv": argv, "directory": directory.replace(place, "<place>")})
    };
    json!(logged.lines().map(call).collect::<Vec<_>>())
}

/// What a check left in its evidence directory: every entry with its mode,
/// and the text of the JSON evidence, of what the commands complained and
/// of the files `said` names. The directories `closed` are listed and not
/// entered.
pub(super) fn kept(output: &Path, place: &str, closed: &[&str], said: &[&str]) -> (Value, Value) {
    fn walk(root: &Path, directory: &Path, closed: &[&str], entries: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("a directory") {
            let path = entry.expect("an entry").path();
            let below = path.strip_prefix(root).expect("below");
            if path.is_dir() && !closed.iter().any(|closed| below == Path::new(closed)) {
                walk(root, &path, closed, entries);
            }
            entries.push(path);
        }
    }
    let (mut entries, mut listed, mut texts) = (Vec::new(), json!({}), json!({}));
    if output.is_dir() {
        walk(output, output, closed, &mut entries);
    }
    for path in entries {
        let below = path
            .strip_prefix(output)
            .expect("below")
            .to_string_lossy()
            .into_owned();
        let mode = path
            .symlink_metadata()
            .expect("an entry")
            .permissions()
            .mode()
            & 0o777;
        listed[&below] = json!({"mode": mode, "directory": path.is_dir()});
        let evidence = below.ends_with(".json") && !below.ends_with("source-manifest.json");
        let read = evidence || below.ends_with(".stderr") || said.contains(&below.as_str());
        if path.is_file() && read {
            let text = std::fs::read_to_string(&path).expect("text");
            texts[&below] = text.replace(place, "<place>").into();
        }
    }
    (listed, texts)
}

#[test]
fn every_recorded_check_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("sc");
    let git = host_git();
    let lock = &recording["lock"];
    let (mut passed, mut found, mut refused, mut printed) = (0, 0, 0, 0);
    for (index, case) in list(&recording["checks"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        let [root, tools] = ["root", "tools"].map(|part| place.join(part));
        for directory in [&root, &tools] {
            std::fs::create_dir_all(directory).expect("a directory");
        }
        let place = place.canonicalize().expect("a place");
        let (root, tools) = (place.join("root"), place.join("tools"));
        let built = json!({"first": case["files"], "working": case["untracked"]});
        repository(&root, &git, &recording, &built);
        for made in list(&case["made"]) {
            std::fs::create_dir_all(place.join(text(made))).expect("a directory");
        }
        let log = place.join("log");
        let program = scripted(&tools, &log, &case["answers"]);
        let digests = strings(&case["digests"]);
        let reads = Cell::new(0);
        let digest = || {
            let read = reads.replace(reads.get() + 1);
            Ok(digests
                .get(read)
                .cloned()
                .unwrap_or(text(&lock["digest"]).to_owned()))
        };
        let engine = Engine {
            path: &program,
            version: text(&lock["version"]),
            lock: &digest,
        };
        let include_vendor = case["includeVendor"] == true;
        let output = place.join(text(&case["output"]));
        let ambient = |variable: &str| -> Option<OsString> {
            match variable {
                "PATH" => Some("/usr/bin:/bin".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        // What the reference's command did before it scanned: the vendor
        // rule, then the place of the evidence, then its own context.
        let outcome = require_local_vendor(include_vendor, !case["marker"].is_null())
            .and_then(|()| new_output(&root, &output))
            .map_err(crate::context::Failure::from)
            .and_then(|output| {
                let mut context = Context::new(&root, &output, Limits::REVIEWED, &ambient)?;
                check(&mut context, &engine, &git, include_vendor)
            });
        let place = place.to_string_lossy().into_owned();
        let (listed, texts) = kept(&output, &place, &["home", "source"], &[]);
        let unmade = matches!(
            case["result"].get("code").map(text),
            Some("semgrep_output_exists" | "semgrep_output_in_source")
        );
        if !unmade {
            assert_eq!(listed, case["kept"], "{name}");
            assert_eq!(texts, case["texts"], "{name}");
        }
        assert_eq!(asked(&log, &place), case["asked"], "{name}");
        if case["throughMain"] == true {
            let (line, status) = answered(&outcome);
            assert_eq!(json!(line), case["printed"], "{name}");
            assert_eq!(json!(status), case["result"]["status"], "{name}");
            printed += 1;
        } else if let Some(code) = case["result"].get("code").map(text) {
            let failure = outcome.err().map(|failure| failure.to_string());
            assert_eq!(failure.as_deref(), Some(named(code, name)), "{name}");
            refused += 1;
        } else {
            let report = outcome.expect("a report");
            assert_eq!(report, case["result"]["report"], "{name}");
            if report["passed"] == true {
                passed += 1;
            } else {
                found += 1;
            }
        }
    }
    assert_eq!((passed, found, refused, printed), (4, 2, 21, 6));
}

/// The reference's command printed the general code for every error of
/// the system, and the bounded reader names two of them.
#[test]
fn an_error_of_the_system_is_printed_as_the_general_failure() {
    for code in ["tool_entry_missing", "tool_entry_unavailable"] {
        let (line, status) = answered(&Err(Fault(code).into()));
        assert_eq!(
            line,
            "{\"passed\": false, \"error\": \"semgrep_check_failed\"}\n"
        );
        assert_eq!(status, 1);
    }
    let (line, status) = answered(&Err(Fault("tool_file_size").into()));
    assert_eq!(line, "{\"passed\": false, \"error\": \"tool_file_size\"}\n");
    assert_eq!(status, 1);
}

#[test]
fn the_bounds_are_the_ones_the_reference_keeps() {
    let recording = recorded();
    let limits = &recording["limits"];
    assert_eq!(json!(MAX_CONFIG), limits["MAX_CONFIG"]);
    assert_eq!(json!(MAX_FILES), limits["MAX_FILES"]);
    assert_eq!(json!(MAX_RULES), limits["MAX_RULES"]);
    assert_eq!(json!(MAX_LINE), limits["MAX_LINE"]);
    let languages: serde_json::Map<String, Value> = super::LANGUAGES
        .iter()
        .map(|(ending, language)| ((*ending).to_owned(), json!(language)))
        .collect();
    assert_eq!(Value::Object(languages), recording["languages"]);
    assert_eq!(json!(super::PREFIXES), recording["prefixes"]);
}
