//! Replays of the `recorded_local.json` sections of the reports and their
//! health: whole numbers, Rust coverage counters, database metadata,
//! reports, inventories, upload readiness, extraction options and scopes,
//! each case rebuilt in its own place against the recorded pin, and the
//! reference's own tests of them.

use super::{
    complete_health, database_health, extraction_options, extraction_statistics, report_health,
    upload_ready, validate_report_scope,
};
use crate::{
    codeql::{
        local::tests::{Tally, came, number, placed, words},
        tests::{items, recording, text},
        tools::{AUTOMATED_LANGUAGES, LANGUAGES},
        triage::integer,
    },
    context::Failure,
    document::Document,
};
use std::path::Path;

const RECORDED: &[u8] = include_bytes!("recorded_local.json");

/// The recorded checkout's pin and workflow.
fn checkout(recorded: &Document) -> (&str, &str) {
    (text(recorded, "pin"), text(recorded, "workflow"))
}

/// What came of a step that refuses with a fault here, as the tally takes it.
fn faulted<T>(
    result: crate::Result<T>,
    held: impl FnOnce(T) -> Document,
) -> std::result::Result<Document, String> {
    came(result.map_err(Failure::from), held)
}

#[test]
fn every_recorded_value_is_a_whole_number_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "integers") {
        let value = Document::read(text(case, "json").as_bytes()).expect("a value");
        tally.judge(
            case,
            faulted(integer(&value), number),
            &[
                ("18446744073709551616", "ValueError"),
                ("-9223372036854775809", "ValueError"),
                ("-0", "ValueError"),
            ],
            &[],
        );
    }
    let expected = Tally {
        held: 8,
        refused: 0,
        raised: 10,
        stricter: 3,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_run_keeps_its_rust_counters_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "statistics") {
        let run = Document::read(text(case, "run").as_bytes()).expect("a run");
        let kept = extraction_statistics(&run, text(case, "language"));
        tally.judge(
            case,
            faulted(kept, |counters| counters),
            &[("every counter past the largest machine word", "ValueError")],
            &[],
        );
    }
    let expected = Tally {
        held: 19,
        refused: 20,
        raised: 19,
        stricter: 1,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_database_is_judged_alike() {
    let recorded = recording(RECORDED);
    let (pin, workflow) = checkout(&recorded);
    let mut tally = Tally::default();
    for case in items(&recorded, "databases") {
        let scratch = placed(case.get("tree").expect("a tree"), Some(pin), Some(workflow));
        let place = &scratch.0;
        let judged = database_health(
            &place.join("root"),
            &place.join("database"),
            text(case, "language"),
        );
        tally.judge(case, came(judged, |()| Document::Null), &[], &[]);
    }
    let expected = Tally {
        held: 7,
        refused: 28,
        raised: 4,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_report_is_identified_alike() {
    let recorded = recording(RECORDED);
    let (pin, workflow) = checkout(&recorded);
    let mut tally = Tally::default();
    for case in items(&recorded, "reports") {
        let pin = case.get("pin").and_then(Document::text).unwrap_or(pin);
        let scratch = placed(case.get("tree").expect("a tree"), Some(pin), Some(workflow));
        let place = &scratch.0;
        let (language, category) = (text(case, "language"), text(case, "category"));
        let identity = report_health(
            &place.join("root"),
            &place.join("report.sarif"),
            language,
            category,
        );
        tally.judge(
            case,
            came(identity, |identity| identity),
            &[],
            // The reference's reader took the number and its checker
            // refused it; here the reader refuses it.
            &[(
                "a report with a number that is not finite",
                "JSONDecodeError",
            )],
        );
    }
    let expected = Tally {
        held: 34,
        refused: 58,
        raised: 42,
        stricter: 0,
        reworded: 1,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_inventory_is_judged_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "inventories") {
        let reports = Document::read(text(case, "reports").as_bytes()).expect("reports");
        let reports = reports.items().expect("a list");
        let languages = words(case, "languages");
        let judged = complete_health(reports, &languages, text(case, "suite"));
        tally.judge(case, faulted(judged, |()| Document::Null), &[], &[]);
    }
    let expected = Tally {
        held: 10,
        refused: 26,
        raised: 11,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_report_is_ready_for_upload_alike() {
    let recorded = recording(RECORDED);
    let (pin, workflow) = checkout(&recorded);
    let mut tally = Tally::default();
    for case in items(&recorded, "uploads") {
        let scratch = placed(case.get("tree").expect("a tree"), Some(pin), Some(workflow));
        let place = &scratch.0;
        let ready = upload_ready(
            &place.join("root"),
            &place.join("out"),
            text(case, "revision"),
            text(case, "category"),
            text(case, "language"),
        );
        tally.judge(case, came(ready, Document::Bool), &[], &[]);
    }
    let expected = Tally {
        held: 13,
        refused: 16,
        raised: 7,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_extraction_is_scoped_alike() {
    let recorded = recording(RECORDED);
    let mut held = 0;
    for case in items(&recorded, "options") {
        let options = extraction_options(
            Path::new("/x/out/source"),
            text(case, "language"),
            case.get("includeVendor") == Some(&Document::Bool(true)),
        );
        let options: Vec<Document> = options
            .iter()
            .map(|option| option.to_str().expect("text").into())
            .collect();
        let result = case.get("result").and_then(|result| result.get("held"));
        assert_eq!(
            Some(&Document::List(options)),
            result,
            "{}",
            text(case, "name")
        );
        held += 1;
    }
    assert_eq!(held, 12);
    let mut tally = Tally::default();
    for case in items(&recorded, "scopes") {
        let scratch = placed(case.get("tree").expect("a tree"), None, None);
        let judged = validate_report_scope(
            &scratch.0.join("report.sarif"),
            text(case, "language"),
            case.get("includeVendor") == Some(&Document::Bool(true)),
        );
        tally.judge(case, faulted(judged, |()| Document::Null), &[], &[]);
    }
    let expected = Tally {
        held: 15,
        refused: 7,
        raised: 4,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

/// A report of `language` and `category` as the reference's own tests
/// made one: passed, of a hundred rules, and of two hundred compiled files
/// for the native language.
fn row(language: &str, category: &str) -> Document {
    let compiled = if language == "c-cpp" {
        number(200)
    } else {
        Document::Null
    };
    Document::record([
        ("language", language.into()),
        ("category", category.into()),
        ("policyPassed", true.into()),
        ("rules", number(100)),
        ("nativeCompiledFiles", compiled),
    ])
}

#[test]
fn a_complete_inventory_needs_every_language_and_the_suites_asked_for() {
    let mut reports: Vec<Document> = LANGUAGES
        .iter()
        .map(|language| row(language, "security"))
        .collect();
    assert_eq!(complete_health(&reports, &LANGUAGES, "security"), Ok(()));
    let short = complete_health(&reports[..4], &LANGUAGES, "security");
    assert_eq!(
        short,
        Err(crate::Fault("codeql_local_incomplete_languages"))
    );
    let twice = [reports.clone(), vec![reports[0].clone()]].concat();
    let repeated = complete_health(&twice, &LANGUAGES, "security");
    assert_eq!(repeated, Err(crate::Fault("codeql_local_report_inventory")));
    let both = complete_health(&reports, &LANGUAGES, "all");
    assert_eq!(both, Err(crate::Fault("codeql_local_incomplete_languages")));
    reports.extend(
        LANGUAGES
            .iter()
            .map(|language| row(language, "quality-advisory")),
    );
    assert_eq!(complete_health(&reports, &LANGUAGES, "all"), Ok(()));
    for (field, value, code) in [
        ("rules", number(0), "codeql_local_empty_queries"),
        ("policyPassed", false.into(), "codeql_local_policy_failed"),
        (
            "nativeCompiledFiles",
            Document::Null,
            "codeql_local_native_coverage",
        ),
    ] {
        let mut native = row("c-cpp", "security");
        native.put(field, value);
        let judged = complete_health(&[native], &["c-cpp"], "security");
        assert_eq!(judged, Err(crate::Fault(code)), "{field}");
    }
}

#[test]
fn a_first_party_extraction_excludes_the_vendored_tree_and_a_vendor_scan_does_not() {
    let source = Path::new("/x/out/source");
    let configuration = "--codescanning-config=/x/out/source/security/codeql/first-party.yml";
    for language in AUTOMATED_LANGUAGES {
        let options = extraction_options(source, language, false);
        assert_eq!(options[0], configuration, "{language}");
        let dependencies = "--extractor-option=extract_dependencies_as_source=false";
        let kept = options.iter().any(|option| option == dependencies);
        assert_eq!(kept, language == "rust", "{language}");
        assert!(
            extraction_options(source, language, true).is_empty(),
            "{language}"
        );
    }
}
