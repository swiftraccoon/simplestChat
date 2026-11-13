//! Replays of the recorded findings checks.
//!
//! `recorded_findings.json` holds what `build/security_findings.py` answered
//! for zizmor and cargo-audit reports, dependency budgets, migration trees and
//! gitleaks configurations. Where the reference failed with an exception of
//! its interpreter, `named` gives the code here; `STRICTER` names the inputs
//! it admitted and these checks refuse.

use super::{cargo, dependency_budget, gitleaks_configuration, new_migrations, zizmor};
use crate::{
    Result,
    policy::{self, Date, Exception},
    scratch::Scratch,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const RECORDED: &str = include_str!("recorded_findings.json");

/// A route step is text or a whole number; a time is written the one plain
/// way; a schema is the number one; an allowlist holds no fraction.
const STRICTER: [(&str, &str); 9] = [
    ("a key that is a truth value", "zizmor_route"),
    ("a key that is null", "zizmor_route"),
    ("a key that is a fraction", "zizmor_route"),
    ("a database time in the basic form", "cargo_database_stale"),
    (
        "a database time at hour twenty-four",
        "cargo_database_stale",
    ),
    (
        "a database time with seconds in its zone",
        "cargo_database_stale",
    ),
    (
        "a database time with minute sixty in its zone",
        "cargo_database_stale",
    ),
    (
        "a budget whose schema is a truth value",
        "dependency_budget_schema",
    ),
    (
        "a baseline whose schema is a truth value",
        "migration_baseline_schema",
    ),
];
const FRACTION: (&str, &str) = (
    "a pattern that is a fraction, reviewed",
    "gitleaks_broad_allowlist",
);

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn named<'a>(code: &'a str, group: &str) -> &'a str {
    match (code, group) {
        ("KeyError", _) => "scanner_field_missing",
        ("JSONDecodeError", _) => "invalid_tool_json",
        ("ValueError", _) => "cargo_database_stale",
        ("FileNotFoundError", _) => "tool_entry_missing",
        ("UnicodeDecodeError", "migrations") => "migration_not_text",
        ("UnicodeDecodeError" | "TOMLDecodeError", _) => "invalid_gitleaks_configuration",
        (code, _) => code,
    }
}

fn reviews(recording: &Value, case: &Value) -> Vec<Exception> {
    let today = Date::parse(text(&recording["today"])).expect("the recorded day");
    case["reviews"]
        .as_array()
        .expect("reviews")
        .iter()
        .map(|review| policy::parse(review, today).expect("a recorded review"))
        .collect()
}

/// A report as bytes: the recorded value, or text that is no JSON.
fn bytes(case: &Value, key: &str) -> Vec<u8> {
    match case.get("text") {
        Some(raw) => text(raw).as_bytes().to_vec(),
        None => serde_json::to_vec(&case[key]).expect("a report"),
    }
}

/// Counts of the cases judged alike, refused alike and refused more strictly.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize);

fn settle(group: &str, case: &Value, judged: Result<Value>, tally: &mut Tally) {
    let name = text(&case["name"]);
    let result = &case["result"];
    let refused = judged.as_ref().err().map(|fault| fault.0);
    let stricter = STRICTER
        .iter()
        .chain([&FRACTION])
        .find(|(case, _)| *case == name);
    if let Some((_, code)) = stricter {
        assert!(result.get("code").is_none(), "{group}: {name}");
        assert_eq!(refused, Some(*code), "{group}: {name}");
        tally.2 += 1;
    } else if let Some(expected) = result.get("code") {
        assert_eq!(
            refused,
            Some(named(text(expected), group)),
            "{group}: {name}"
        );
        tally.1 += 1;
    } else {
        assert_eq!(judged.as_ref().ok(), Some(result), "{group}: {name}");
        tally.0 += 1;
    }
}

#[test]
fn every_recorded_zizmor_report_is_judged_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in recording["zizmor"].as_array().expect("zizmor") {
        let judged = zizmor(&bytes(case, "report"), &reviews(&recording, case));
        settle(
            "zizmor",
            case,
            judged.map(|count| json!({"count": count})),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(15, 32, 3));
}

#[test]
fn every_recorded_cargo_report_is_judged_alike() {
    let recording = recorded();
    let now = Duration::from_secs(recording["now"].as_u64().expect("now"));
    let mut tally = Tally::default();
    for case in recording["cargo"].as_array().expect("cargo") {
        let dependencies = case["dependencies"].as_u64().expect("dependencies");
        let judged = cargo(
            &bytes(case, "report"),
            &reviews(&recording, case),
            dependencies,
            now,
        );
        settle(
            "cargo",
            case,
            judged.map(|count| json!({"count": count})),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(25, 70, 4));
}

#[test]
fn every_recorded_budget_is_judged_alike() {
    let recording = recorded();
    let scratch = Scratch::new("budgets");
    let path = scratch.0.join("budget.json");
    let mut tally = Tally::default();
    for case in recording["budgets"].as_array().expect("budgets") {
        let _ = std::fs::remove_file(&path);
        if let Some(budget) = case["budget"].as_str() {
            std::fs::write(&path, budget).expect("budget");
        }
        let judged = dependency_budget(&bytes(case, "metadata"), &path);
        settle(
            "budgets",
            case,
            judged.map(|()| json!({"held": true})),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(9, 22, 1));
}

#[test]
fn every_recorded_migration_tree_is_judged_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in recording["migrations"].as_array().expect("migrations") {
        let scratch = Scratch::new("migrations");
        let root = &scratch.0;
        if let Some(baseline) = case["baseline"].as_str() {
            std::fs::create_dir(root.join("security")).expect("security");
            std::fs::write(root.join("security/migration-baseline.json"), baseline)
                .expect("baseline");
        }
        if case.get("absent").is_none() {
            std::fs::create_dir(root.join("migrations")).expect("migrations");
        }
        let raw = case["bytes"].as_array().expect("bytes");
        for (name, content) in case["files"].as_object().expect("files") {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("directory");
            // A file recorded as bytes holds one byte for each character.
            let content: Vec<u8> = if raw.contains(&json!(name)) {
                text(content).chars().map(|byte| byte as u8).collect()
            } else {
                text(content).as_bytes().to_vec()
            };
            std::fs::write(path, content).expect("file");
        }
        let judged = new_migrations(root);
        settle(
            "migrations",
            case,
            judged.map(|added| json!({"added": added})),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(23, 29, 1));
}

#[test]
fn every_recorded_gitleaks_configuration_is_judged_alike() {
    let recording = recorded();
    let scratch = Scratch::new("gitleaks");
    let path = scratch.0.join("gitleaks.toml");
    let mut tally = Tally::default();
    for case in recording["gitleaks"].as_array().expect("gitleaks") {
        let _ = std::fs::remove_file(&path);
        if let Some(configuration) = case.get("text").and_then(Value::as_str) {
            std::fs::write(&path, configuration).expect("configuration");
        } else if let Some(configuration) = case.get("latin1") {
            let content: Vec<u8> = text(configuration).chars().map(|byte| byte as u8).collect();
            std::fs::write(&path, content).expect("configuration");
        }
        let vendor = case["includeVendor"] == true;
        let judged = gitleaks_configuration(&path, &reviews(&recording, case), vendor);
        settle(
            "gitleaks",
            case,
            judged.map(|()| json!({"held": true})),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(15, 28, 1));
}

#[test]
fn the_committed_configuration_and_baseline_are_admitted() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Read as of the first day: whether a review has expired is the gate's
    // question on the day it runs.
    let reviews = policy::read(&root.join("security/exceptions.json"), Date::MIN, false)
        .expect("the committed policy");
    assert_eq!(
        gitleaks_configuration(&root.join("security/gitleaks.toml"), &reviews, false),
        Ok(())
    );
    let added = new_migrations(&root).expect("the committed baseline");
    assert!(added.iter().all(|name| name.starts_with("migrations/")));
    assert_eq!(
        dependency_budget(
            br#"{"packages": []}"#,
            &root.join("security/dependency-budget.json")
        ),
        Ok(())
    );
}
