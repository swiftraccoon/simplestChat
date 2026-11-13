//! Replays of the recorded registry sessions and npm checks.
//!
//! `recorded_registry.json` holds what the registry session of
//! `build/security_dependency_licenses.py` did with a `curl` that answers
//! its calls in order from numbered files: what each fetch answered, how
//! `curl` was run, and what the npm check made of a registry's document.
//! Where the reference failed with an exception of its interpreter, `named`
//! gives the code here. `STRICTER` names the addresses and documents that
//! its lenient readers admitted and that are refused here.

use super::{
    Dependency,
    registry::{Bounds, Evidence, Registry, npm_license},
    tests::{content, list, text},
};
use crate::{
    context::{Context, Failure, Limits, Outcome},
    fast::Programs,
    scratch::Scratch,
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::BTreeSet,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const RECORDED: &str = include_str!("recorded_registry.json");

/// An address is written as it is asked for: the scheme in lower case, a
/// path, no space or control anywhere, nothing outside ASCII, and no mark
/// of a query or a fragment even when none follows it.
const STRICTER_ADDRESSES: [&str; 9] = [
    "an address with a scheme in capitals",
    "an address after a space",
    "an address with a tab in its host",
    "an address with a line end in its path",
    "an address with a space in its path",
    "an address with an empty query",
    "an address with an empty fragment",
    "an address without a path",
    "an address outside ASCII",
];

/// A document is UTF-8 from its first byte.
const STRICTER_DOCUMENTS: [&str; 2] =
    ["a document after a byte order mark", "a document in UTF-16"];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

/// The host's programs when only `curl` is asked for, or when it is missing.
pub(super) struct Curl(Option<PathBuf>);

impl Programs for Curl {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite(name))
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        let curl = self.0.clone().filter(|_| name == "curl");
        curl.ok_or(Failure::Prerequisite(name))
    }

    fn python(&self) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite("python3"))
    }
}

fn spelled(path: &Path) -> &str {
    path.to_str().expect("a path in text")
}

/// A `curl` that logs how it was run and answers its calls in order from
/// numbered files. Its arguments are logged in hexadecimal, each ended by a
/// zero byte, because one of them holds a line end.
fn curl(directory: &Path, log: &Path) -> String {
    let (directory, log) = (spelled(directory), spelled(log));
    [
        "#!/bin/sh".to_owned(),
        format!("count=$(cat '{directory}/count')"),
        format!("echo $((count + 1)) > '{directory}/count'"),
        concat!(
            "{ printf 'argv\\t%s\\n' ",
            "\"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\"; ",
            "printf 'cwd\\t%s\\n' \"$(pwd -P)\"; } >> '"
        )
        .to_owned()
            + log
            + "'",
        format!("if [ -f '{directory}'/$count.sleep ]; then"),
        format!("  sleep \"$(cat '{directory}'/$count.sleep)\""),
        "fi".to_owned(),
        format!("if [ -f '{directory}'/$count.err ]; then cat '{directory}'/$count.err >&2; fi"),
        format!("cat '{directory}'/$count.out"),
        format!("exit \"$(cat '{directory}'/$count.status)\""),
        String::new(),
    ]
    .join("\n")
}

/// One recorded session's directories, its context and its scripted `curl`.
pub(super) struct Session {
    pub(super) context: Context,
    pub(super) curl: Curl,
    log: PathBuf,
    replacements: [(String, &'static str); 2],
}

pub(super) fn session(base: &Path, answers: &[Value], missing: bool) -> Session {
    let [root, output, tools] = ["root", "output", "tools"].map(|part| base.join(part));
    for directory in [&root, &output, &tools] {
        std::fs::create_dir_all(directory).expect("a directory");
    }
    let log = base.join("log");
    for (index, answer) in answers.iter().enumerate() {
        let said = match answer.get("raw") {
            Some(raw) => content(raw),
            None => {
                let mut said = content(&answer["body"]);
                said.push(b'\n');
                said.extend(text(&answer["code"]).bytes());
                said
            }
        };
        std::fs::write(tools.join(format!("{index}.out")), said).expect("an answer");
        let status = answer["status"].as_i64().expect("a status");
        std::fs::write(tools.join(format!("{index}.status")), format!("{status}\n"))
            .expect("a status file");
        if let Some(errors) = answer.get("errors") {
            std::fs::write(tools.join(format!("{index}.err")), content(errors)).expect("errors");
        }
        if let Some(seconds) = answer.get("sleep") {
            std::fs::write(tools.join(format!("{index}.sleep")), format!("{seconds}\n"))
                .expect("a pause");
        }
    }
    std::fs::write(tools.join("count"), "0\n").expect("a count");
    let program = tools.join("curl");
    std::fs::write(&program, curl(&tools, &log)).expect("a program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    let ambient = |name: &str| -> Option<OsString> {
        match name {
            "PATH" => Some("/usr/bin:/bin".into()),
            "HOME" => Some("/h".into()),
            _ => None,
        }
    };
    let context = Context::new(&root, &output, Limits::REVIEWED, &ambient).expect("a context");
    let replacements = [
        (spelled(&program).to_owned(), "<curl>"),
        (spelled(&root).to_owned(), "<root>"),
    ];
    Session {
        context,
        curl: Curl((!missing).then_some(program)),
        log,
        replacements,
    }
}

impl Session {
    /// How `curl` was run, call by call, in the recording's own spelling.
    pub(super) fn calls(&self) -> Value {
        let written = std::fs::read_to_string(&self.log).unwrap_or_default();
        let token = |word: &str| {
            let mut word = word.to_owned();
            for (path, token) in &self.replacements {
                word = word.replace(path, token);
            }
            word
        };
        let mut calls: Vec<Value> = Vec::new();
        for line in written.lines() {
            let mut words = line.split('\t');
            if words.next() == Some("argv") {
                let logged = words.next().expect("the arguments");
                let bytes = (0..logged.len() / 2)
                    .map(|at| u8::from_str_radix(&logged[2 * at..2 * at + 2], 16))
                    .collect::<Result<Vec<u8>, _>>();
                let bytes = bytes.expect("hexadecimal");
                let argv: Vec<String> = bytes
                    .strip_suffix(&[0])
                    .expect("an end after the last argument")
                    .split(|byte| *byte == 0)
                    .map(|word| token(std::str::from_utf8(word).expect("text")))
                    .collect();
                calls.push(json!({"argv": argv}));
            } else {
                let last = calls.last_mut().expect("a call before its directory");
                last["cwd"] = token(words.next().expect("a directory")).into();
            }
        }
        calls.into()
    }

    /// Nothing a session asks is kept as evidence or recorded as a check.
    fn left_no_trace(&self, name: &str) {
        assert!(self.context.checks().is_empty(), "{name}");
        let kept = std::fs::read_dir(self.context.output()).expect("the output");
        let kept: Vec<OsString> = kept
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(kept, ["home"], "{name}");
    }
}

fn bound(case: &Value, name: &str, reviewed: u64) -> u64 {
    case["bounds"]
        .get(name)
        .map_or(reviewed, |bound| bound.as_u64().expect("a bound"))
}

#[test]
fn every_recorded_fetch_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("lf");
    let (mut answered, mut refused, mut stricter) = (0, 0, 0);
    for (index, case) in list(&recording["fetches"]).iter().enumerate() {
        let name = text(&case["name"]);
        let reviewed = Bounds::REVIEWED;
        let fetches = bound(case, "MAX_FETCHES", reviewed.fetches as u64);
        let bounds = Bounds {
            fetches: usize::try_from(fetches).expect("a count"),
            seconds: bound(case, "MAX_SECONDS", reviewed.seconds),
            kept: bound(case, "MAX_CACHE", reviewed.kept),
            ..reviewed
        };
        let answers = list(&case["answers"]);
        let session = session(
            &scratch.0.join(index.to_string()),
            answers,
            case["missing"] == true,
        );
        let clock = Cell::new(Duration::ZERO);
        let since = || clock.get();
        let mut registry = Registry::new(&session.curl, &since, bounds);
        let strict = STRICTER_ADDRESSES.contains(&name);
        for step in list(&case["steps"]) {
            clock.set(Duration::from_secs_f64(
                step["at"].as_f64().expect("a time"),
            ));
            let limit = step["limit"].as_u64().unwrap_or(bounds.index);
            let fetched = registry.fetch(&session.context, text(&step["url"]), limit);
            let judged = fetched
                .map(|answer| json!({"sha256": crate::digest(&answer), "length": answer.len()}));
            let failure = judged.as_ref().err().map(ToString::to_string);
            let reference = &step["result"];
            if strict {
                assert!(reference.get("code").is_none(), "{name}");
                assert_eq!(
                    failure.as_deref(),
                    Some("dependency_registry_origin"),
                    "{name}"
                );
                stricter += 1;
            } else if let Some(code) = reference.get("code") {
                let code = match text(code) {
                    "ValueError" => "dependency_registry_origin",
                    code => code,
                };
                assert_eq!(failure.as_deref(), Some(code), "{name}");
                refused += 1;
            } else {
                assert_eq!(judged.as_ref().ok(), Some(reference), "{name}");
                answered += 1;
            }
        }
        if strict {
            assert_eq!(registry.requests(), 0, "{name}");
            assert_eq!(session.calls(), json!([]), "{name}");
        } else {
            assert_eq!(json!(registry.requests()), case["requests"], "{name}");
            assert_eq!(session.calls(), case["calls"], "{name}");
        }
        session.left_no_trace(name);
    }
    assert_eq!((answered, refused, stricter), (38, 51, 9));
}

/// The code here for an exception of the reference's interpreter: a
/// document of another shape is no document of the locked package, and a
/// licence that is no text is not the lock's licence.
fn named<'a>(code: &'a str, name: &str) -> &'a str {
    let exception = matches!(
        code,
        "KeyError" | "ValueError" | "JSONDecodeError" | "DuplicateJsonError" | "UnicodeDecodeError"
    );
    match (exception, name.contains("licence is a")) {
        (true, true) => "dependency_npm_license_identity",
        (true, false) => "dependency_npm_registry_identity",
        (false, _) => code,
    }
}

#[test]
fn every_recorded_npm_check_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("ln");
    let (mut held, mut refused, mut stricter) = (0, 0, 0);
    for (index, case) in list(&recording["npm"]).iter().enumerate() {
        let name = text(&case["name"]);
        let session = session(
            &scratch.0.join(index.to_string()),
            list(&case["answers"]),
            false,
        );
        let since = || Duration::ZERO;
        let mut registry = Registry::new(&session.curl, &since, Bounds::REVIEWED);
        let dependency = Dependency {
            ecosystem: "npm",
            manifest: "web/package-lock.json".to_owned(),
            name: text(&case["package"]).to_owned(),
            version: text(&case["version"]).to_owned(),
            identity: "identity".to_owned(),
            hashes: BTreeSet::from([text(&case["integrity"]).to_owned()]),
            license: text(&case["license"]).to_owned(),
            source: text(&case["source"]).to_owned(),
        };
        let judged = npm_license(&session.context, &mut registry, &dependency).map(|evidence| {
            let evidence: Vec<Value> = evidence.iter().map(Evidence::record).collect();
            json!({"evidence": evidence})
        });
        let failure = judged.as_ref().err().map(ToString::to_string);
        let reference = &case["result"];
        if STRICTER_DOCUMENTS.contains(&name) {
            assert!(reference.get("code").is_none(), "{name}");
            assert_eq!(
                failure.as_deref(),
                Some("dependency_npm_registry_identity"),
                "{name}"
            );
            stricter += 1;
        } else if let Some(code) = reference.get("code") {
            assert_eq!(failure.as_deref(), Some(named(text(code), name)), "{name}");
            refused += 1;
        } else {
            assert_eq!(judged.as_ref().ok(), Some(reference), "{name}");
            held += 1;
        }
        assert_eq!(session.calls(), case["calls"], "{name}");
        assert_eq!(json!(registry.requests()), case["requests"], "{name}");
        session.left_no_trace(name);
    }
    assert_eq!((held, refused, stricter), (9, 38, 2));
}

#[test]
fn the_reviewed_bounds_are_the_recorded_ones() {
    let recording = recorded();
    let reviewed = &recording["reviewed"];
    let bounds = Bounds::REVIEWED;
    assert_eq!(json!(bounds.fetches), reviewed["MAX_FETCHES"]);
    assert_eq!(json!(bounds.seconds), reviewed["MAX_SECONDS"]);
    assert_eq!(json!(bounds.kept), reviewed["MAX_CACHE"]);
    assert_eq!(json!(bounds.index), reviewed["MAX_INDEX"]);
    assert_eq!(json!(bounds.metadata), reviewed["MAX_METADATA"]);
}
