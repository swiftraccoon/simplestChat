//! Replays of the recorded review of pinned actions.
//!
//! `recorded_actions.json` holds what the reference in Python read from
//! workflow YAML through its YAML library, and what its command did in real
//! repositories whose commits have fixed names, with a `curl` and a `git
//! ls-remote` that answer their calls in order: how it ended, what it
//! complained of, its receipt byte for byte and what it asked. Where the
//! reference failed with an exception of its interpreter or its library,
//! `named` gives the code here. The tables name every reading that differs.

use super::{
    Asking, COMMAND_TIME, COMPLAINTS, MAX_BYTES, MAX_CHANGED, MAX_NODES, MAX_VERSIONS,
    MAX_WORKFLOWS, PAGE, REVIEW_TIME, Review, check, patience, pins, reviewed,
};
use crate::{
    context::{Context, Failure, Limits, Outcome},
    fast::Programs,
    scratch::{Scratch, content, host_git, repository, written},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const RECORDED: &str = include_str!("recorded_actions.json");

/// What the reference read and is refused here. Every use a workflow
/// writes is judged, also one that a later key of the same name replaces;
/// aliases are followed 1,024 times at most, never into themselves, and
/// what they repeat is 100,000 values and openings and closings of lists
/// and mappings at most; a workflow is UTF-8; and lists and mappings nest
/// at most 64 deep.
const STRICTER: [(&str, &str); 10] = [
    (
        "a use given twice, the first unpinned",
        "actions_unpinned_or_unsupported_use",
    ),
    ("an alias of itself", "actions_workflow_yaml"),
    ("many aliases of one step", "actions_workflow_yaml"),
    ("one alias more than are followed", "actions_workflow_yaml"),
    ("aliases that multiply", "actions_workflow_yaml"),
    ("UTF-16 with a byte order mark", "actions_workflow_yaml"),
    ("deep nesting", "actions_workflow_yaml"),
    (
        "lists nested one deeper than is read",
        "actions_workflow_yaml",
    ),
    ("a use one deeper than is read", "actions_workflow_yaml"),
    (
        "aliases that repeat one value more than is followed",
        "actions_workflow_yaml",
    ),
];

/// Where more pins are found here than the reference found: it kept the
/// last of two keys of one name, and held the keys `1` and `true` to be
/// one key.
const WIDER: [(&str, usize); 2] = [
    ("a use given twice", 2),
    ("keys that are numbers and truths", 3),
];

/// Uses that both readers refuse, under different codes: the two read
/// plain scalars as different kinds of value, and of two faults the one
/// that the workflow writes first is answered here, where the reference
/// met the last step first and kept the last of two keys of one name.
const OTHERWISE: [(&str, &str); 7] = [
    (
        "a use that is a date",
        "actions_unpinned_or_unsupported_use",
    ),
    ("a use that is an octal number", "actions_use_not_literal"),
    (
        "a use that is sixty-based",
        "actions_unpinned_or_unsupported_use",
    ),
    (
        "two uses refused differently, the first no text",
        "actions_use_not_literal",
    ),
    (
        "two uses refused differently, the first unpinned",
        "actions_unpinned_or_unsupported_use",
    ),
    (
        "a use given twice, refused differently, the first no text",
        "actions_use_not_literal",
    ),
    (
        "a use given twice, refused differently, the first unpinned",
        "actions_unpinned_or_unsupported_use",
    ),
];

/// Reviews the reference passed and that are refused here.
///
/// A version is written in the digits 0 to 9: a tag in other digits is no
/// release, where the reference asked about it as one. An answer is JSON in
/// UTF-8 without a byte order mark, and its text holds no half of a pair
/// of surrogates. And a local action is one the review read: below the
/// vendored tree, behind a link, without a description or spelled otherwise
/// than the tree spells it, it is refused, where the reference never looked
/// at what a local path names.
const REFUSED: [(&str, &str); 11] = [
    (
        "an advisory and a release in other digits",
        "actions_advisory_version_unresolved",
    ),
    (
        "an answer after a byte order mark",
        "actions_metadata_format",
    ),
    ("an answer in UTF-16", "actions_metadata_format"),
    (
        "an answer with half of a pair of surrogates",
        "actions_metadata_format",
    ),
    (
        "a local action below the vendored tree",
        "actions_local_action_unreviewed",
    ),
    (
        "a local action that is not there",
        "actions_local_action_unreviewed",
    ),
    (
        "a local action that is read beside one that is not",
        "actions_local_action_unreviewed",
    ),
    (
        "a local action without a description",
        "actions_local_action_unreviewed",
    ),
    (
        "a local action behind a link",
        "actions_local_action_unreviewed",
    ),
    (
        "a local action named with a step back",
        "actions_local_action_unreviewed",
    ),
    (
        "a local action in other letters",
        "actions_local_action_unreviewed",
    ),
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

#[test]
fn a_workflow_nested_to_its_bound_is_read_whatever_stack_the_caller_has() {
    // 64 levels need most of a default stack in a build that is not
    // optimised, and a thread has less of it the more the program links.
    let nested = |levels: usize| {
        let mut text = String::new();
        for level in 0..levels {
            text.push_str(&"  ".repeat(level));
            text.push_str("level:\n");
        }
        text.push_str(&"  ".repeat(levels));
        text.push_str("uses: ./local\n");
        text
    };
    let read = |levels: usize| {
        let text = nested(levels);
        let small = std::thread::Builder::new().stack_size(256 * 1024);
        let reading = small.spawn(move || pins(text.as_bytes()).map_err(|fault| fault.0));
        reading.expect("a thread").join().expect("no overflow")
    };
    assert_eq!(read(63), Ok(BTreeSet::new()));
    assert_eq!(read(64), Err("actions_workflow_yaml"));
}

#[test]
fn every_recorded_workflow_is_read_alike() {
    let recording = recorded();
    let (mut read, mut refused, mut stricter, mut wider, mut otherwise) = (0, 0, 0, 0, 0);
    for case in list(&recording["reads"]) {
        let name = text(&case["name"]);
        let found = pins(&content(&case["content"]));
        let fault = found.as_ref().err().map(|fault| fault.0);
        let reference = &case["result"];
        if let Some((_, code)) = STRICTER.iter().find(|(case, _)| *case == name) {
            assert!(reference.get("code").is_none(), "{name}");
            assert_eq!(fault, Some(*code), "{name}");
            stricter += 1;
        } else if let Some((_, count)) = WIDER.iter().find(|(case, _)| *case == name) {
            let found = found.expect("pins");
            let fewer = list(&reference["pins"]);
            assert!(fewer.len() < found.len() && found.len() == *count, "{name}");
            assert!(fewer.iter().all(|pin| found.contains(text(pin))), "{name}");
            wider += 1;
        } else if let Some((_, code)) = OTHERWISE.iter().find(|(case, _)| *case == name) {
            assert!(reference["code"] != *code, "{name}");
            assert_eq!(fault, Some(*code), "{name}");
            otherwise += 1;
        } else if let Some(code) = reference.get("code") {
            let code = match text(code) {
                "YAMLError" | "RecursionError" => "actions_workflow_yaml",
                code => code,
            };
            assert_eq!(fault, Some(code), "{name}");
            refused += 1;
        } else {
            assert_eq!(json!(found.ok()), reference["pins"], "{name}");
            read += 1;
        }
    }
    assert_eq!(
        (read, refused, stricter, wider, otherwise),
        (56, 59, 10, 2, 7)
    );
}

/// The scripted programs of one review: `curl`, and a `git` that passes
/// everything but `ls-remote` on to the host's git.
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

/// A program that logs its arguments in hexadecimal, each ended by a zero
/// byte, and answers its calls in order from numbered files.
fn program(directory: &Path, log: &Path, name: &str, git: &Path) -> String {
    let (directory, log) = (
        directory.to_str().expect("a path in text"),
        log.to_str().expect("a path in text"),
    );
    let passes = match name {
        "git" => format!(
            "case \"$1\" in ls-remote) ;; *) exec '{}' \"$@\";; esac",
            git.display()
        ),
        _ => ":".to_owned(),
    };
    [
        "#!/bin/sh".to_owned(),
        passes,
        format!("count=$(cat '{directory}/{name}.count')"),
        format!("echo $((count + 1)) > '{directory}/{name}.count'"),
        format!("printf '{name}\\t%s\\n' \\"),
        format!("  \"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\" >> '{log}'"),
        format!("cat '{directory}/{name}'.$count.out"),
        format!("exit \"$(cat '{directory}/{name}'.$count.status)\""),
        String::new(),
    ]
    .join("\n")
}

/// The calls the scripted programs logged: each program's name and its
/// arguments.
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

/// The code here for what the reference complained of: an exception of its
/// YAML library is a workflow that is no YAML, one of its readers of
/// answers is an answer of another form, and a tag line it could not
/// divide names no commit.
fn named<'a>(code: &'a str, name: &str) -> &'a str {
    match code {
        "ParserError" => "actions_workflow_yaml",
        "FileNotFoundError" => "tool_entry_missing",
        "OSError" => "tool_entry_unavailable",
        "ProcessError" => "command_stdout_limit_exceeded",
        "ValueError" if name.ends_with("a tag line without a tab") => "actions_tag_identity",
        "ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError"
        | "UnicodeDecodeError" => "actions_metadata_format",
        code => code,
    }
}

/// One recorded case set up in directories of its own: the repository, the
/// copy of its sources, the scripted programs and the base it is given.
struct Staged {
    root: PathBuf,
    output: PathBuf,
    snapshot: PathBuf,
    log: PathBuf,
    programs: Tools,
    base: Option<String>,
}

fn staged(base: &Path, git: &Path, recording: &Value, case: &Value) -> Staged {
    let name = text(&case["name"]);
    let [root, output, tools, snapshot] =
        ["root", "output", "tools", "snapshot"].map(|part| base.join(part));
    for directory in [&root, &output, &tools, &snapshot] {
        std::fs::create_dir_all(directory).expect("a directory");
    }
    let repository = repository(&root, git, recording, case);
    for (commit, named) in case["commits"].as_object().expect("commits") {
        let built = repository.commits.get(commit.as_str());
        assert_eq!(
            built.map(String::as_str),
            Some(text(named)),
            "{name}: {commit}"
        );
    }
    written(&snapshot, &repository.files.clone().into_iter().collect());
    let log = base.join("log");
    for (program_name, answers) in [("curl", &case["said"]), ("git", &case["remotes"])] {
        for (number, answer) in list(answers).iter().enumerate() {
            let said = tools.join(format!("{program_name}.{number}.out"));
            std::fs::write(said, content(&answer[1])).expect("an answer");
            let status = tools.join(format!("{program_name}.{number}.status"));
            std::fs::write(status, format!("{}\n", answer[0])).expect("a status");
        }
        std::fs::write(tools.join(format!("{program_name}.count")), "0\n").expect("a count");
        let path = tools.join(program_name);
        std::fs::write(&path, program(&tools, &log, program_name, git)).expect("a program");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    }
    let given = match text(&case["base"]) {
        "head" => None,
        "unknown" => Some("0".repeat(40)),
        base => Some(
            repository
                .commits
                .get(base)
                .cloned()
                .unwrap_or(base.to_owned()),
        ),
    };
    Staged {
        root,
        output,
        snapshot,
        log,
        programs: Tools(tools),
        base: given,
    }
}

impl Staged {
    fn context(&self) -> Context {
        let ambient = |variable: &str| -> Option<OsString> {
            match variable {
                "PATH" => Some("/usr/bin:/bin".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        Context::new(&self.root, &self.output, Limits::REVIEWED, &ambient).expect("a context")
    }

    fn receipt(&self) -> PathBuf {
        self.output.join("actions-dependencies.json")
    }

    /// The receipt and its mode, as a recording holds them.
    fn kept(&self) -> (Value, Value) {
        let receipt = self.receipt();
        let mode = receipt.metadata().ok();
        let mode = mode.map(|metadata| metadata.permissions().mode() & 0o777);
        (json!(std::fs::read_to_string(&receipt).ok()), json!(mode))
    }
}

#[test]
fn every_recorded_review_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("ar");
    let git = host_git();
    let (mut passed, mut refused, mut stricter) = (0, 0, 0);
    for (index, case) in list(&recording["reviews"]).iter().enumerate() {
        let name = text(&case["name"]);
        let staged = staged(&scratch.0.join(index.to_string()), &git, &recording, case);
        let context = staged.context();
        let asking = Asking {
            context: &context,
            programs: &staged.programs,
            ends: Instant::now() + REVIEW_TIME,
        };
        let review = Review {
            snapshot: &staged.snapshot,
            base: staged.base.as_deref(),
            include_vendor: case["includeVendor"] == true,
        };
        let judged = reviewed(&asking, &review, &staged.receipt());
        let failure = judged.err().map(|failure| failure.to_string());
        let complained = text(&case["errors"]);
        assert_eq!(complained.is_empty(), case["status"] == 0, "{name}");
        if let Some((_, code)) = REFUSED.iter().find(|(case, _)| *case == name) {
            // What was asked before the refusal is what the reference asked first.
            let asked = calls(&staged.log);
            let (asked, reference) = (list(&asked), list(&case["calls"]));
            assert!(complained.is_empty(), "{name}");
            assert_eq!(failure.as_deref(), Some(*code), "{name}");
            assert_eq!(staged.kept(), (Value::Null, Value::Null), "{name}");
            assert!(reference.starts_with(asked), "{name}");
            stricter += 1;
            continue;
        }
        let expected = complained
            .strip_prefix("Action dependency review failed: ")
            .map(|code| named(code.trim_end(), name));
        assert_eq!(failure.as_deref(), expected, "{name}");
        if expected.is_none() {
            passed += 1;
        } else {
            refused += 1;
        }
        let (receipt, mode) = staged.kept();
        assert_eq!(receipt, case["receipt"], "{name}");
        assert_eq!(mode, case["receiptMode"], "{name}");
        assert_eq!(calls(&staged.log), case["calls"], "{name}");
        assert!(context.checks().is_empty(), "{name}");
    }
    assert_eq!((passed, refused, stricter), (41, 55, 11));
}

#[test]
fn the_bounds_are_the_ones_the_reference_keeps() {
    let recording = recorded();
    let limits = &recording["limits"];
    assert_eq!(json!(MAX_BYTES), limits["MAX_BYTES"]);
    assert_eq!(json!(MAX_NODES), limits["MAX_NODES"]);
    assert_eq!(json!(MAX_WORKFLOWS), limits["MAX_WORKFLOWS"]);
    assert_eq!(json!(PAGE), limits["PAGE_SIZE"]);
    assert_eq!(json!(MAX_VERSIONS), limits["MAX_VERSIONS"]);
    assert_eq!(json!(MAX_CHANGED), limits["MAX_CHANGED"]);
    // What one of its commands may take, print and complain, and what the
    // whole review may take: read from the calls the reference made.
    let command = json!({
        "timeout": COMMAND_TIME.as_secs(),
        "stdout": MAX_BYTES,
        "stderr": COMPLAINTS,
    });
    assert_eq!(command, limits["command"]);
    assert_eq!(json!({"timeout": REVIEW_TIME.as_secs()}), limits["review"]);
}

#[test]
fn a_command_has_what_is_left_of_the_review_and_its_own_time_at_most() {
    let seconds = Duration::from_secs;
    assert_eq!(REVIEW_TIME, seconds(600));
    assert_eq!(patience(REVIEW_TIME), Some(seconds(45)));
    assert_eq!(patience(seconds(46)), Some(seconds(45)));
    assert_eq!(patience(seconds(45)), Some(seconds(45)));
    assert_eq!(patience(seconds(44)), Some(seconds(44)));
    assert_eq!(
        patience(Duration::from_millis(1)),
        Some(Duration::from_millis(1))
    );
    assert_eq!(patience(Duration::ZERO), None);
}

#[test]
fn a_review_whose_time_is_over_asks_nothing_more() {
    let recording = recorded();
    let scratch = Scratch::new("at");
    let git = host_git();
    let cases = list(&recording["reviews"]);
    let case = cases
        .iter()
        .find(|case| case["name"] == "an action added below a path")
        .expect("the recorded case");
    assert!(case["status"] == 0 && !list(&case["calls"]).is_empty());
    let staged = staged(&scratch.0, &git, &recording, case);
    let context = staged.context();
    let asking = Asking {
        context: &context,
        programs: &staged.programs,
        ends: Instant::now(),
    };
    let review = Review {
        snapshot: &staged.snapshot,
        base: staged.base.as_deref(),
        include_vendor: false,
    };
    let judged = reviewed(&asking, &review, &staged.receipt());
    assert_eq!(judged, Err(Failure::Fixed("command_timed_out")));
    assert_eq!(staged.kept(), (Value::Null, Value::Null));
    assert_eq!(calls(&staged.log), json!([]));
}

#[test]
fn every_recorded_check_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("ac");
    let git = host_git();
    let (mut passed, mut refused) = (0, 0);
    for (index, case) in list(&recording["checks"]).iter().enumerate() {
        let name = text(&case["name"]);
        let staged = staged(&scratch.0.join(index.to_string()), &git, &recording, case);
        let mut context = staged.context();
        let review = Review {
            snapshot: &staged.snapshot,
            base: staged.base.as_deref(),
            include_vendor: false,
        };
        let judged = check(&mut context, &staged.programs, &review);
        let failure = judged.err().map(|failure| failure.to_string());
        let expected = case["result"].get("code").map(text);
        assert_eq!(failure.as_deref(), expected, "{name}");
        if expected.is_none() {
            passed += 1;
        } else {
            refused += 1;
        }
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
                line.entry("timed").or_insert(false.into());
                line.into()
            })
            .collect();
        assert_eq!(json!(lines), case["checks"], "{name}");
        let mut evidence: Vec<String> = std::fs::read_dir(&staged.output)
            .expect("the output")
            .map(|entry| {
                let entry = entry.expect("an entry").file_name();
                entry.into_string().expect("text")
            })
            .filter(|entry| entry != "home")
            .collect();
        evidence.sort();
        assert_eq!(json!(evidence), case["evidence"], "{name}");
        for kept in &evidence {
            let said = std::fs::read(staged.output.join(kept)).expect("evidence");
            if let Some(complained) = case["complained"].get(kept) {
                let complained = text(complained);
                let expected = match complained.strip_prefix("Action dependency review failed: ") {
                    Some(code) => {
                        let code = named(code.trim_end(), name);
                        format!("Action dependency review failed: {code}\n")
                    }
                    None => complained.to_owned(),
                };
                assert_eq!(
                    String::from_utf8(said).ok(),
                    Some(expected),
                    "{name}: {kept}"
                );
            } else if let Some(size) = case["printed"].get(kept) {
                assert_eq!(json!(said.len()), *size, "{name}: {kept}");
            }
        }
        let (receipt, mode) = staged.kept();
        assert_eq!(receipt, case["receipt"], "{name}");
        assert_eq!(mode, case["receiptMode"], "{name}");
        assert_eq!(calls(&staged.log), case["calls"], "{name}");
    }
    assert_eq!((passed, refused), (9, 7));
}
