//! Replays of the recorded mutation policy check.
//!
//! `recorded_mutation.json` holds what the reference in Python did with
//! policies, with the tool's listing of mutants, with the logs of test runs
//! and with the tool's results. Where the reference failed with an
//! exception of its interpreter, `named` gives the code here. `OTHERWISE`
//! names the cases that end differently here and says how.

use super::{assess, command, inventory, policy_at, selection, test_counts};
use crate::scratch::{Scratch, content, written};
use serde_json::{Value, json};
use std::{fs::Permissions, os::unix::fs::PermissionsExt, path::Path};

const RECORDED: &str = include_str!("recorded_mutation.json");

/// Cases that end differently here, with the code of each or what is
/// answered. The reference compared numbers as its interpreter does, which
/// takes a truth for one or nought and a whole fraction for its whole
/// number: a schema, a total, a count and a status are whole numbers here.
/// A document is JSON in UTF-8 without a byte order mark. A number too long
/// for a machine word is a fraction to the reader here: as a bound it is in
/// no form rather than out of range, and as a count of tests it is the most
/// there is.
const OTHERWISE: [(&str, &str); 11] = [
    ("a schema that is a truth", "mutation_policy_version"),
    (
        "a schema that is a whole fraction",
        "mutation_policy_version",
    ),
    ("a policy after a byte order mark", "mutation_policy_form"),
    (
        "a bound too long for a machine word",
        "mutation_policy_form",
    ),
    (
        "a total that is a whole fraction",
        "mutation_result_coverage",
    ),
    ("a count that is a truth", "mutation_result_totals"),
    ("a count that is a whole fraction", "mutation_result_totals"),
    ("a success that is false", "mutation_result_totals"),
    (
        "a caught mutant with a status that is a whole fraction",
        "mutation_caught_process",
    ),
    ("outcomes after a byte order mark", "mutation_result_form"),
    (
        "a result with a count too long for a machine word",
        r#"{"counts": [18446744073709551615, 0]}"#,
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

/// The code here for what the reference raised.
fn named<'a>(code: &'a str, group: &str) -> &'a str {
    match (code, group) {
        ("FileNotFoundError", _) => "tool_entry_missing",
        ("IsADirectoryError", _) => "nonregular_tool_file",
        ("OSError", _) => "tool_entry_unavailable",
        ("ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError", "policies") => {
            "mutation_policy_form"
        }
        ("ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError", "inventories") => {
            "mutation_inventory_form"
        }
        ("ValueError" | "KeyError" | "JSONDecodeError" | "DuplicateJsonError", _) => {
            "mutation_result_form"
        }
        (code, _) => code,
    }
}

/// How one group of cases ended: answered alike, refused alike under a
/// code of the reference's own, refused where the reference raised an
/// exception, and ended differently as the table says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize, usize);

/// Compare what was answered here with the recorded result.
fn settle(case: &Value, group: &str, here: crate::Result<Value>, tally: &mut Tally) {
    let (name, there) = (text(&case["name"]), &case["result"]);
    let code = there.get("code").map(text);
    let here = here.map_err(|fault| fault.0);
    if let Some((_, expected)) = OTHERWISE.iter().find(|(named, _)| *named == name) {
        match serde_json::from_str::<Value>(expected) {
            Ok(answer) => {
                assert_ne!(*there, answer, "{name}");
                assert_eq!(here, Ok(answer), "{name}");
            }
            Err(_) => {
                assert_ne!(code, Some(*expected), "{name}");
                assert_eq!(here, Err(*expected), "{name}");
            }
        }
        tally.3 += 1;
    } else if let Some(code) = code {
        assert_eq!(here, Err(named(code, group)), "{name}");
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

/// Give the file at `path` the recorded `mode`, when one is recorded.
fn moded(path: &Path, mode: &Value) {
    if let Some(mode) = mode.as_u64() {
        let mode = Permissions::from_mode(u32::try_from(mode).expect("a mode"));
        std::fs::set_permissions(path, mode).expect("a mode that is set");
    }
}

/// The policy that a recorded text holds.
fn policy_of(scratch: &Path, content: &Value) -> super::Policy {
    let path = scratch.join("policy-of.json");
    std::fs::write(&path, text(content)).expect("a policy");
    policy_at(&path).expect("a recorded policy")
}

#[test]
fn every_recorded_policy_is_read_alike() {
    let recording = recorded();
    let scratch = Scratch::new("mp");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["policies"]).iter().enumerate() {
        let path = scratch.0.join(format!("{index}.json"));
        if !case["content"].is_null() {
            std::fs::write(&path, content(&case["content"])).expect("a policy");
        }
        moded(&path, &case["mode"]);
        let read = policy_at(&path).map(|policy| {
            json!({
                "version": policy.version,
                "maximum": policy.maximum,
                "minimumTests": policy.minimum_tests,
                "buildSeconds": policy.build_seconds,
                "testSeconds": policy.test_seconds,
                "totalSeconds": policy.total_seconds,
                "files": policy.files,
                "filters": policy.filters,
            })
        });
        settle(case, "policies", read, &mut tally);
    }
    assert_eq!(tally, Tally(32, 85, 22, 4));
}

#[test]
fn every_recorded_selection_gives_the_same_commands() {
    let recording = recorded();
    let scratch = Scratch::new("ms");
    let (tool, source, output) = ("/tools/cargo-mutants", "/place/source", "/place/results");
    let spelled = |listing: bool, policy: &super::Policy| -> Vec<String> {
        let argv = command(
            Path::new(tool),
            Path::new(source),
            Path::new(output),
            policy,
            listing,
        );
        argv.iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    };
    for case in list(&recording["selections"]) {
        let name = text(&case["name"]);
        let policy = policy_of(&scratch.0, &case["policy"]);
        assert_eq!(json!(selection(&policy)), case["selection"], "{name}");
        assert_eq!(json!(spelled(true, &policy)), case["listing"], "{name}");
        assert_eq!(json!(spelled(false, &policy)), case["running"], "{name}");
    }
    assert_eq!(list(&recording["selections"]).len(), 5);
}

#[test]
fn every_recorded_listing_is_held_alike() {
    let recording = recorded();
    let scratch = Scratch::new("mi");
    let mut tally = Tally::default();
    for case in list(&recording["inventories"]) {
        let policy = policy_of(&scratch.0, &case["policy"]);
        let listed = inventory(&content(&case["listing"]), &policy);
        let read = listed.map(|mutants| {
            let pairs: Vec<Value> = mutants
                .iter()
                .map(|(name, item)| json!([name, item]))
                .collect();
            json!({"mutants": pairs})
        });
        settle(case, "inventories", read, &mut tally);
    }
    assert_eq!(tally, Tally(6, 12, 17, 0));
}

#[test]
fn every_recorded_log_is_counted_alike() {
    let recording = recorded();
    let scratch = Scratch::new("mc");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["counts"]).iter().enumerate() {
        let directory = scratch.0.join(index.to_string());
        std::fs::create_dir_all(&directory).expect("a directory");
        for (path, spec) in case["files"].as_object().expect("files") {
            if spec.get("directory").is_some() {
                std::fs::create_dir_all(directory.join(path)).expect("a directory");
            }
        }
        let files = case["files"].as_object().expect("files").iter();
        let plain = files.filter(|(_, spec)| spec.get("directory").is_none());
        written(
            &directory,
            &plain
                .map(|(path, spec)| (path.clone(), spec.clone()))
                .collect(),
        );
        let row = case["row"].as_object().expect("a row");
        let counted = test_counts(row, &directory);
        let read = counted.map(|(passed, failed)| json!({"counts": [passed, failed]}));
        settle(case, "counts", read, &mut tally);
    }
    assert_eq!(tally, Tally(9, 28, 6, 1));
}

#[test]
fn every_recorded_result_is_assessed_alike() {
    let recording = recorded();
    let scratch = Scratch::new("ma");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["assessments"]).iter().enumerate() {
        let directory = scratch.0.join(index.to_string());
        std::fs::create_dir_all(&directory).expect("a directory");
        written(&directory, case["logs"].as_object().expect("logs"));
        if !case["outcomes"].is_null() {
            let outcomes = directory.join("outcomes.json");
            std::fs::write(&outcomes, content(&case["outcomes"])).expect("outcomes");
            moded(&outcomes, &case["mode"]);
        }
        let policy = policy_of(&scratch.0, &case["policy"]);
        let named = |item: &Value| (text(&item["name"]).to_owned(), item.clone());
        let expected: Vec<(String, Value)> = list(&case["expected"]).iter().map(named).collect();
        let read =
            assess(&directory, &expected, &policy).map(|summary| json!({"summary": summary}));
        settle(case, "assessments", read, &mut tally);
    }
    assert_eq!(tally, Tally(16, 49, 24, 6));
}

#[test]
fn the_bounds_are_the_ones_the_reference_keeps() {
    let recording = recorded();
    let limits = &recording["limits"];
    assert_eq!(json!(super::MAX_REPORT), limits["MAX_REPORT"]);
    assert_eq!(json!(super::MAX_POLICY_FILES), limits["MAX_POLICY_FILES"]);
    assert_eq!(json!(super::MAX_TEST_FILTERS), limits["MAX_TEST_FILTERS"]);
}
