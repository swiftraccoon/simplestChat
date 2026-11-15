//! Replays of the recorded diagnosis of a scan and of the gate's step.
//!
//! `recorded_semgrep_summary.json` holds what the reference in Python made
//! of a scan's one line, its status and its private evidence, and whole
//! steps of its gate in a real repository, with its scanner run as the
//! command it was and an engine that answers from files. `OTHERWISE` names
//! the cases that end differently here and says how.

use super::{
    Engine, MAX_FILES, gate,
    summary::{
        ERRORS, KINDS, MAX_DEPTH, MAX_ENGINE_REPORTS, MAX_ITEMS, MAX_NODES, MAX_REPORT, diagnosis,
    },
    tests::{compared, list, strings, text, tree},
};
use crate::{
    context::{Context, Limits, Note},
    scratch::{Scratch, asked, content, host_git, kept, repository, said, scripted},
};
use serde_json::{Map, Value, json};
use std::{cell::Cell, ffi::OsString, os::unix::fs::DirBuilderExt};

const RECORDED: &str = include_str!("recorded_semgrep_summary.json");

/// Cases that end differently here, with what is said of each. What was
/// said, an inventory and a report of the engine are JSON in UTF-8 without
/// a byte order mark, which the reference let pass: a line after one is no
/// result, an inventory after one names no path, and a report after one is
/// not read. And of what the engine says of a timeout, the values of a
/// record are looked at in the order of their keys, where the reference
/// took them as they were written: the two differ only when there are more
/// parts than are looked at.
const OTHERWISE: [(&str, &str); 4] = [
    (
        "what was said after a byte order mark",
        r#"{"passed": false, "error": "semgrep_invalid_result"}"#,
    ),
    (
        "a finding with an inventory after a byte order mark",
        r#"{"passed": false, "error": "semgrep_invalid_result"}"#,
    ),
    (
        "a report after a byte order mark",
        r#"{"passed": false, "error": "semgrep_engine_errors", "engineErrors": [],
            "engineErrorsTruncated": false, "fixpointTimeouts": [],
            "fixpointTimeoutsTruncated": false}"#,
    ),
    (
        "a timeout whose last written of 64 fields is first by name",
        r#"{"passed": false, "error": "semgrep_fixpoint_timeout", "engineErrors": [],
            "engineErrorsTruncated": false, "fixpointTimeouts": [{}],
            "fixpointTimeoutsTruncated": false}"#,
    ),
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

/// A line of the summary as the record the reference kept, and the order
/// of its fields.
fn kept_as(note: &Note) -> (Value, Vec<&'static str>) {
    let fields: Map<String, Value> = note
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    (fields.into(), note.iter().map(|(name, _)| *name).collect())
}

#[test]
fn every_recorded_diagnosis_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("sd");
    // Clean scans, scans with findings, failures with all that is said of
    // them, failures of which no timeout is said, invalid results, and the
    // cases that end otherwise here.
    let mut tally = [0; 6];
    for (index, case) in list(&recording["diagnoses"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        let [root, directory] = ["root", "evidence/semgrep"].map(|part| place.join(part));
        std::fs::create_dir_all(root.join("security/semgrep")).expect("a root");
        if !case["rules"].is_null() {
            let pack = root.join(super::RULES);
            std::fs::write(pack, content(&case["rules"])).expect("a pack");
        }
        if case["made"] == true {
            std::fs::create_dir_all(&directory).expect("a directory");
            tree(&directory, &case["files"]);
        }
        let status = i32::try_from(case["status"].as_i64().expect("a status")).expect("small");
        let note = diagnosis(&root, status, &content(&case["said"]), &directory);
        let (said, order) = kept_as(&note);
        if let Some((_, expected)) = OTHERWISE.iter().find(|(named, _)| *named == name) {
            let expected: Value = serde_json::from_str(expected).expect("an expectation");
            assert_ne!(case["result"], expected, "{name}");
            assert_eq!(said, expected, "{name}");
            tally[5] += 1;
            continue;
        }
        assert_eq!(said, case["result"], "{name}");
        assert_eq!(json!(order), case["order"], "{name}");
        let kind = match (said["passed"] == true, order.len()) {
            (true, _) => 0,
            (false, 4) if said.get("findings").is_some() => 1,
            (false, 6) => 2,
            (false, 4) => 3,
            _ => 4,
        };
        assert!(
            kind != 4 || said["error"] == "semgrep_invalid_result",
            "{name}"
        );
        tally[kind] += 1;
    }
    assert_eq!(tally, [7, 10, 114, 3, 104, 4]);
}

#[test]
fn every_recorded_step_of_the_gate_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("sg");
    let git = host_git();
    let lock = &recording["lock"];
    let (mut passed, mut failed) = (0, 0);
    for (index, case) in list(&recording["gates"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        for part in ["root", "tools", "evidence"] {
            std::fs::create_dir_all(place.join(part)).expect("a directory");
        }
        let place = place.canonicalize().expect("a place");
        let [root, tools, evidence] = ["root", "tools", "evidence"].map(|part| place.join(part));
        repository(
            &root,
            &git,
            &recording,
            &json!({"first": case["files"], "working": {}}),
        );
        for made in list(&case["made"]) {
            let mut private = std::fs::DirBuilder::new();
            let made = private.mode(0o700).create(place.join(text(made)));
            made.expect("a private directory");
        }
        let log = place.join("log");
        let program = scripted(&tools, &log, "semgrep-core", "engine", &case["answers"]);
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
        let ambient = |variable: &str| -> Option<OsString> {
            match variable {
                "PATH" => Some("/usr/bin:/bin".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        let mut context =
            Context::new(&root, &evidence, Limits::REVIEWED, &ambient).expect("a context");
        let outcome = gate(&mut context, &engine, &git, case["includeVendor"] == true);
        let (mut checks, mut orders) = (Vec::new(), Vec::new());
        for note in context.checks() {
            let (mut check, order) = kept_as(note);
            if let Some(elapsed) = check.get_mut("elapsedSeconds") {
                *elapsed = elapsed.is_f64().into();
            }
            checks.push(check);
            orders.push(order);
        }
        assert_eq!(json!(checks), case["checks"], "{name}");
        assert_eq!(json!(orders), case["orders"], "{name}");
        let place = place.to_string_lossy().into_owned();
        let closed = ["home", "semgrep/home", "semgrep/source"];
        assert_eq!(kept(&evidence, &closed), case["kept"], "{name}");
        let read = |below: &str| compared(below) || below == "01-semgrep.stdout";
        assert_eq!(
            said(&evidence, &place, &closed, read),
            case["texts"],
            "{name}"
        );
        assert_eq!(asked(&log, &place), case["asked"], "{name}");
        match case["result"].get("code").map(text) {
            Some(code) => {
                let failure = outcome.expect_err("a failure").to_string();
                assert_eq!(failure, code, "{name}");
                failed += 1;
            }
            None => {
                outcome.expect("a pass");
                passed += 1;
            }
        }
    }
    assert_eq!((passed, failed), (2, 11));
}

#[test]
fn the_bounds_and_the_lists_are_the_ones_the_reference_keeps() {
    let recording = recorded();
    let limits = &recording["limits"];
    assert_eq!(json!(MAX_ITEMS), limits["MAX_ITEMS"]);
    assert_eq!(json!(MAX_ENGINE_REPORTS), limits["MAX_ENGINE_REPORTS"]);
    assert_eq!(json!(MAX_DEPTH), limits["MAX_METADATA_DEPTH"]);
    assert_eq!(json!(MAX_NODES), limits["MAX_METADATA_NODES"]);
    assert_eq!(json!(MAX_REPORT), limits["MAX_REPORT"]);
    let sorted = |names: &[&str]| {
        let mut names = names.to_vec();
        names.sort_unstable();
        json!(names)
    };
    assert_eq!(sorted(&ERRORS), recording["errors"]);
    assert_eq!(sorted(&KINDS), recording["kinds"]);
    assert_eq!(MAX_FILES, 20000);
}
