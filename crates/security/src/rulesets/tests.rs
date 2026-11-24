//! Replays of the recorded reconciliation of rulesets.
//!
//! `recorded_rulesets.json` holds what the command's reference, the Python
//! module this one replaced, did: how it read policies, and whole runs of
//! its command in a real checkout whose commit has a fixed name, with a
//! `gh` that answers its calls in order and a `git` that runs. Each run is
//! compared by its status, what it printed, what it complained of and every
//! call with its arguments. Where the reference printed the class of an
//! exception, `named` gives the code here. The tables name every run that
//! ends differently.
//!
//! One difference has no table: the ruleset sent to GitHub is the same
//! value in another spelling, with its keys in the order of their names,
//! so what `gh` read is compared as JSON and not byte for byte.

use super::{
    Answer, Asked, COMMAND_TIME, Mode, POLICY, ROOM, asked, healthy_analyses, loaded, reconcile,
};
use crate::{
    context::{Failure, Outcome},
    scratch::{Scratch, content, host_git, repository, written},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

const RECORDED: &str = include_str!("recorded_rulesets.json");
/// The languages whose analysis an application required when the runs were
/// recorded: every recorded run is replayed with them.
const RECORDED_LANGUAGES: [&str; 4] = ["actions", "javascript-typescript", "python", "rust"];

/// Policies the reference read and that are refused here: a policy is JSON
/// in UTF-8 without a byte order mark, and the repository's number fits in
/// sixty-four bits.
const UNREAD: [(&str, &str); 3] = [
    ("a policy after a byte order mark", "ruleset_policy_format"),
    ("a policy in UTF-16", "ruleset_policy_format"),
    (
        "an identifier past sixty-four bits",
        "ruleset_repository_identity",
    ),
];

/// Runs the reference completed and that are refused here. An answer is
/// JSON in UTF-8 without a byte order mark. A number is written as a whole
/// number: the reference held `1.0` and `true` to be the number one, so a
/// repository's or an application's number written as a fraction, and a
/// count of checks that is a truth, passed there, and it read the number
/// of a run out of text.
const REFUSED: [(&str, &str); 6] = [
    (
        "a check of a repository whose identifier is written as a fraction",
        "ruleset_repository_changed",
    ),
    (
        "a check when an answer comes after a byte order mark",
        "ruleset_answer_format",
    ),
    (
        "a check when an answer is in UTF-16",
        "ruleset_answer_format",
    ),
    (
        "applying when the application's identifier is written as a fraction",
        "ruleset_gate_not_successful",
    ),
    (
        "applying when the count of checks is a truth",
        "ruleset_check_pagination",
    ),
    (
        "applying when the runs are numbered in text and all passed",
        "ruleset_answer_format",
    ),
];

/// Runs both refuse, under different codes: the reference read a run's
/// number out of text as well, and so went on to judge the run.
const OTHERWISE: [(&str, &str); 1] = [(
    "applying when the runs are numbered in text",
    "ruleset_answer_format",
)];

/// Command lines the reference took and that are no command here: an
/// option is written in full, and nothing marks the end of the options.
const UNTAKEN: [&str; 2] = [
    "the option shortened",
    "the mode after the end of the options",
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

/// The code here for what the reference complained of. A reader of JSON
/// that failed there is a policy or an answer in another form here, and an
/// answer too long to be printed is refused by the bound of the command.
fn named<'a>(code: &'a str, name: &str) -> &'a str {
    let form = matches!(
        code,
        "ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError"
    );
    match code {
        _ if form && name.contains("with a policy") => "ruleset_policy_format",
        _ if form => "ruleset_answer_format",
        "ProcessError" => "command_stdout_limit_exceeded",
        code => code,
    }
}

#[test]
fn every_recorded_policy_is_read_alike() {
    let recording = recorded();
    let scratch = Scratch::new("rp");
    let directory = scratch.0.join("security");
    std::fs::create_dir_all(&directory).expect("a directory");
    let (mut read, mut refused, mut stricter) = (0, 0, 0);
    for case in list(&recording["policies"]) {
        let name = text(&case["name"]);
        let _ = std::fs::remove_file(scratch.0.join(POLICY));
        let files = [(POLICY.to_owned(), case["content"].clone())];
        written(&scratch.0, &files.into_iter().collect());
        let loaded = loaded(&scratch.0);
        let fault = loaded.as_ref().err().map(|fault| fault.0);
        let reference = &case["result"];
        if let Some((_, code)) = UNREAD.iter().find(|(case, _)| *case == name) {
            assert!(reference.get("code").is_none(), "{name}");
            assert_eq!(fault, Some(*code), "{name}");
            stricter += 1;
        } else if let Some(code) = reference.get("code") {
            let code = match text(code) {
                "ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError" => {
                    "ruleset_policy_format"
                }
                code => code,
            };
            assert_eq!(fault, Some(code), "{name}");
            refused += 1;
        } else {
            let policy = loaded.expect("a policy");
            let named: Vec<&Value> = policy.ruleset.get("name").into_iter().collect();
            let found = json!({
                "repository": policy.repository,
                "identifier": policy.identifier,
                "names": named,
            });
            assert_eq!(found, *reference, "{name}");
            read += 1;
        }
    }
    assert_eq!((read, refused, stricter), (8, 64, 3));
}

/// A `gh` that logs its arguments and what it reads, in hexadecimal, and
/// answers its calls in order from numbered files; or a `git` that logs its
/// arguments and runs the host's git, whatever the caller has configured.
fn program(directory: &Path, log: &Path, name: &str, git: &Path) -> String {
    let (directory, log) = (directory.display(), log.display());
    let logged = [
        format!("printf '{name}\\t%s\\t%s\\n' \\"),
        "  \"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\" \"$read\" \\".to_owned(),
        format!("  >> '{log}'"),
    ]
    .join("\n");
    let lines = match name {
        "git" => vec![
            "#!/bin/sh".to_owned(),
            "read=".to_owned(),
            logged,
            format!(
                "export HOME='{directory}' GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null"
            ),
            format!("exec '{}' \"$@\"", git.display()),
        ],
        _ => vec![
            "#!/bin/sh".to_owned(),
            "read=$(od -An -v -tx1 | tr -d ' \\n')".to_owned(),
            format!("count=$(cat '{directory}/{name}.count')"),
            format!("echo $((count + 1)) > '{directory}/{name}.count'"),
            logged,
            format!("cat '{directory}/{name}'.$count.out"),
            format!("exit \"$(cat '{directory}/{name}'.$count.status)\""),
        ],
    };
    lines.join("\n") + "\n"
}

fn unhexed(digits: &str) -> Vec<u8> {
    (0..digits.len() / 2)
        .map(|at| u8::from_str_radix(&digits[2 * at..2 * at + 2], 16).expect("hex"))
        .collect()
}

/// The calls the scripted programs logged: each program's name with its
/// arguments, and what it read.
fn calls(log: &Path) -> Vec<(Vec<String>, String)> {
    let logged = std::fs::read_to_string(log).unwrap_or_default();
    let call = |line: &str| {
        let mut parts = line.split('\t');
        let name = parts.next().expect("a name").to_owned();
        let arguments = unhexed(parts.next().expect("arguments"));
        let read = unhexed(parts.next().expect("what was read"));
        let arguments = arguments.strip_suffix(&[0]).expect("an end");
        let mut argv = vec![name];
        argv.extend(
            arguments
                .split(|byte| *byte == 0)
                .map(|argument| String::from_utf8(argument.to_vec()).expect("text")),
        );
        (argv, String::from_utf8(read).expect("text"))
    };
    logged.lines().map(call).collect()
}

/// Whether the call that was made is the recorded one: the same arguments,
/// and the same value read where anything was read.
fn same(made: &(Vec<String>, String), reference: &Value) -> bool {
    let read = text(&reference["input"]);
    let value = |text: &str| serde_json::from_str::<Value>(text).ok();
    let alike = match read {
        "" => made.1.is_empty(),
        read => !made.1.is_empty() && value(&made.1) == value(read),
    };
    json!(made.0) == reference["argv"] && alike
}

/// One recorded run set up in a directory of its own: the checkout, and
/// the two scripted programs.
struct Staged {
    root: PathBuf,
    tools: PathBuf,
    log: PathBuf,
}

fn staged(base: &Path, git: &Path, recording: &Value, case: &Value) -> Staged {
    let name = text(&case["name"]);
    let [root, tools] = ["root", "tools"].map(|part| base.join(part));
    for directory in [&root, &tools] {
        std::fs::create_dir_all(directory).expect("a directory");
    }
    if case["repository"] == true {
        let built = repository(&root, git, recording, case);
        let head = built.commits.get("first").map(String::as_str);
        assert_eq!(head, recording["head"].as_str(), "{name}");
    } else {
        let tree = recording["tree"].as_object().expect("a tree");
        written(&root, tree);
    }
    if !case["policy"].is_null() {
        std::fs::write(root.join(POLICY), content(&case["policy"])).expect("a policy");
    }
    written(&root, case["dirty"].as_object().expect("files"));
    if case["stagedChange"] == true {
        let added = Command::new(git)
            .args(["add", "-A"])
            .current_dir(&root)
            .output();
        assert!(added.is_ok_and(|done| done.status.success()), "{name}");
    }
    let log = base.join("log");
    for (number, answer) in list(&case["answers"]).iter().enumerate() {
        std::fs::write(tools.join(format!("gh.{number}.out")), content(&answer[1]))
            .expect("an answer");
        std::fs::write(
            tools.join(format!("gh.{number}.status")),
            format!("{}\n", answer[0]),
        )
        .expect("a status");
    }
    std::fs::write(tools.join("gh.count"), "0\n").expect("a count");
    for name in ["gh", "git"] {
        let path = tools.join(name);
        std::fs::write(&path, program(&tools, &log, name, git)).expect("a program");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    }
    Staged { root, tools, log }
}

#[test]
fn every_recorded_run_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("rr");
    let git = host_git();
    let (mut alike, mut stricter, mut otherwise, mut usage) = (0, 0, 0, 0);
    for (index, case) in list(&recording["runs"]).iter().enumerate() {
        let name = text(&case["name"]);
        let argv: Vec<String> = list(&case["argv"])
            .iter()
            .map(|word| text(word).to_owned())
            .collect();
        let asked = asked(&argv);
        if let Some(status) = case["status"].get("usage") {
            let expected = match status.as_u64() {
                Some(0) => Some(Asked::Help),
                _ => None,
            };
            assert_eq!(asked, expected, "{name}");
            usage += 1;
            continue;
        }
        if UNTAKEN.contains(&name) {
            assert_eq!((asked, &case["status"]), (None, &json!(0)), "{name}");
            usage += 1;
            continue;
        }
        let Some(Asked::Run(mode, revision)) = asked else {
            panic!("{name}: no run");
        };
        let staged = staged(&scratch.0.join(index.to_string()), &git, &recording, case);
        let missing = list(&case["missing"]);
        let programs = |program: &'static str| -> Outcome<PathBuf> {
            if missing.iter().any(|name| name == program) {
                Err(Failure::Prerequisite(program))
            } else {
                Ok(staged.tools.join(program))
            }
        };
        let answer = reconcile(&staged.root, &programs, &RECORDED_LANGUAGES, mode, revision);
        let made = calls(&staged.log);
        let reference = list(&case["calls"]);
        let complained = text(&case["errors"]);
        let code = complained
            .strip_prefix("Ruleset reconciliation failed: ")
            .map(str::trim_end);
        let failed = |code: &str| Answer {
            output: String::new(),
            errors: format!("Ruleset reconciliation failed: {code}\n"),
            status: 1,
        };
        let prefix = made.len() <= reference.len()
            && made
                .iter()
                .zip(reference)
                .all(|(made, call)| same(made, call));
        if let Some((_, code)) = REFUSED.iter().find(|(case, _)| *case == name) {
            // What was asked before the refusal is what the reference asked first.
            assert_eq!((&case["status"], complained), (&json!(0), ""), "{name}");
            assert_eq!(answer, failed(code), "{name}");
            assert!(prefix && made.len() < reference.len(), "{name}");
            stricter += 1;
            continue;
        }
        if let Some((_, here)) = OTHERWISE.iter().find(|(case, _)| *case == name) {
            assert!(code.is_some_and(|code| code != *here), "{name}");
            assert_eq!(answer, failed(here), "{name}");
            assert!(prefix, "{name}");
            otherwise += 1;
            continue;
        }
        let expected = match code {
            Some(code) => failed(named(code, name)),
            None => Answer {
                output: text(&case["output"]).to_owned(),
                errors: String::new(),
                status: u8::try_from(case["status"].as_u64().expect("a status")).expect("small"),
            },
        };
        assert_eq!(code.is_none(), complained.is_empty(), "{name}");
        assert_eq!(answer, expected, "{name}");
        assert!(prefix && made.len() == reference.len(), "{name}: {made:?}");
        alike += 1;
    }
    assert_eq!((alike, stricter, otherwise, usage), (147, 6, 1, 11));
}

#[test]
fn the_maintained_policy_protects_the_history_of_main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = loaded(&root).expect("the maintained policy");
    assert_eq!(policy.repository, "swiftraccoon/simplestChat");
    assert_eq!(policy.identifier, 1_155_881_649);
    let ruleset = Value::Object(policy.ruleset);
    assert_eq!(ruleset["name"], "Main history protection");
    assert_eq!(ruleset["bypass_actors"], json!([]));
    assert_eq!(
        ruleset["rules"],
        json!([{"type": "deletion"}, {"type": "non_fast_forward"}])
    );
}

#[test]
fn the_bounds_are_the_ones_the_reference_keeps() {
    let recording = recorded();
    let command = json!({
        "timeout": COMMAND_TIME.as_secs(),
        "stdout": ROOM.0,
        "stderr": ROOM.1,
    });
    assert_eq!(command, recording["limits"]["command"]);
}

#[test]
fn a_mode_is_named_as_it_is_asked_for() {
    for (name, mode) in [
        ("plan", Mode::Plan),
        ("check", Mode::Check),
        ("apply", Mode::Apply),
    ] {
        assert_eq!(mode.name(), name);
        assert_eq!(asked(&[name.to_owned()]), Some(Asked::Run(mode, None)));
    }
}

#[test]
fn an_application_requires_exactly_the_stored_analyses_and_passes_over_a_python_one() {
    let revision = "a".repeat(40);
    let analysis = |language: &str, commit: &str| {
        json!({
            "tool": {"name": "CodeQL"},
            "category": format!("/language:{language}/security"),
            "commit_sha": commit,
            "ref": "refs/heads/main",
            "error": "",
            "warning": "",
            "rules_count": 7,
        })
    };
    let older = "b".repeat(40);
    let stored = crate::codeql::tools::STORED_LANGUAGES;
    let current =
        ["rust", "javascript-typescript", "actions"].map(|language| analysis(language, &revision));
    let mut answer: Vec<Value> = vec![analysis("python", &older)];
    answer.extend(current.clone());
    assert_eq!(
        healthy_analyses(&Value::Array(answer), &revision, &stored),
        Ok(())
    );
    // A Python analysis, current or not, never stands in for a stored one.
    let mut answer = vec![analysis("python", &revision)];
    answer.extend(current[1..].iter().cloned());
    assert_eq!(
        healthy_analyses(&Value::Array(answer), &revision, &stored),
        Err(crate::Fault("ruleset_language_coverage"))
    );
}
