//! The mutation check of the pure policy functions.
//!
//! A reviewed policy names a few functions that decide who may do what,
//! and the tests that must notice when one of them is changed. The pinned
//! mutation tool lists every change it can make to exactly those
//! functions, makes each in a private copy of the sources and runs the
//! tests. The check passes only when the unchanged copy passes enough
//! tests and every listed change makes a test fail. What the tool says is
//! held to what it listed: a result for a change it did not list, a listed
//! change without a result, a count that does not add up and a caught
//! change whose tests did not fail by an assertion are all refused.

use crate::{Fault, Result, files, record, require};
use serde_json::{Map, Value, json};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

/// Where the policy is, below the sources.
pub const POLICY: &str = "security/mutation/policy.json";
/// The version of the mutation tool the policy is written for.
const TOOL: &str = "27.1.0";
/// The most bytes of the policy, of the tool's results and of one log.
const MAX_REPORT: u64 = 16 * 1024 * 1024;
/// The most source files a policy names.
const MAX_POLICY_FILES: usize = 8;
/// The most test filters a policy names.
const MAX_TEST_FILTERS: usize = 16;
/// The fields of a policy.
const FIELDS: [&str; 9] = [
    "schemaVersion",
    "toolVersion",
    "maximumMutants",
    "minimumTests",
    "buildTimeoutSeconds",
    "testTimeoutSeconds",
    "totalTimeoutSeconds",
    "sourceFiles",
    "testFilters",
];
/// The bounds of a policy, each with the most it may be.
const BOUNDS: [(&str, u64); 5] = [
    ("maximumMutants", 128),
    ("minimumTests", 200),
    ("buildTimeoutSeconds", 1800),
    ("testTimeoutSeconds", 120),
    ("totalTimeoutSeconds", 7200),
];
/// The ways a mutant may end, each with the name of its count.
const SUMMARIES: [(&str, &str); 5] = [
    ("caught", "CaughtMutant"),
    ("missed", "MissedMutant"),
    ("unviable", "Unviable"),
    ("timeout", "Timeout"),
    ("failure", "Failure"),
];
/// A policy in another form than a policy has.
const POLICY_FORM: Fault = Fault("mutation_policy_form");
/// A listing of mutants in another form than the tool writes.
const LISTING_FORM: Fault = Fault("mutation_inventory_form");
/// Results in another form than the tool writes.
const RESULT_FORM: Fault = Fault("mutation_result_form");

/// The reviewed selection: the functions to change, the tests that must
/// notice, and the bounds of the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The version of the tool.
    pub version: String,
    /// The most mutants the selection may have.
    pub maximum: u64,
    /// The fewest tests that must run.
    pub minimum_tests: u64,
    /// How long one build may take, in seconds.
    pub build_seconds: u64,
    /// How long the tests of one mutant may take, in seconds.
    pub test_seconds: u64,
    /// How long the whole run may take, in seconds.
    pub total_seconds: u64,
    /// The source files and the functions of each, in the order the policy
    /// writes them.
    pub files: Vec<(String, Vec<String>)>,
    /// The filters of the tests that are run.
    pub filters: Vec<String>,
}

/// Whether `text` is words of ASCII letters, digits and underscores, none
/// beginning with a digit, joined by `between`.
fn words(text: &str, between: &str) -> bool {
    let word = |word: &str| {
        let plain = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        word.bytes().all(plain) && !word.starts_with(|first: char| first.is_ascii_digit())
    };
    text.split(between)
        .all(|part| !part.is_empty() && word(part))
}

/// Whether `name` is a Rust source below `src`, in lowercase letters and
/// underscores.
fn source(name: &str) -> bool {
    let lettered = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    };
    let below = name
        .strip_prefix("src/")
        .and_then(|rest| rest.strip_suffix(".rs"));
    below.is_some_and(|path| path.split('/').all(lettered))
}

/// The strings of the list `value`.
fn strings(value: &Value, form: Fault) -> Result<Vec<String>> {
    let items = value.as_array().ok_or(form)?;
    let text = |item: &Value| item.as_str().map(str::to_owned).ok_or(form);
    items.iter().map(text).collect()
}

/// The policy at `path`. A selection that is malformed or open-ended is
/// refused before anything is run: exactly the reviewed fields, the one
/// version of the tool, every bound within its range, source files below
/// `src`, functions by their plain paths, each once, and filters that are
/// paths of tests.
pub fn policy_at(path: &Path) -> Result<Policy> {
    let content = files::bounded(path, MAX_REPORT, false)?;
    let document = record::strict(&content).map_err(|_| POLICY_FORM)?;
    let value = document.as_object().ok_or(POLICY_FORM)?;
    require(
        value.len() == FIELDS.len() && FIELDS.iter().all(|name| value.contains_key(*name)),
        "mutation_policy_fields",
    )?;
    require(
        value["schemaVersion"].as_u64() == Some(1) && value["toolVersion"] == TOOL,
        "mutation_policy_version",
    )?;
    let mut numbers = [0; 5];
    for (number, (name, most)) in numbers.iter_mut().zip(BOUNDS) {
        let whole = value[name].as_i64().is_some() || value[name].as_u64().is_some();
        require(whole, POLICY_FORM.0)?;
        *number = value[name]
            .as_u64()
            .filter(|number| (1..=most).contains(number))
            .ok_or(Fault("mutation_policy_bound"))?;
    }
    let selected = value["sourceFiles"].as_object().ok_or(POLICY_FORM)?;
    let mut files = Vec::new();
    for name in record::written(&content, "sourceFiles").map_err(|_| POLICY_FORM)? {
        require(source(&name), "mutation_source_path")?;
        let functions = strings(&selected[&name], POLICY_FORM)?;
        let once = |(index, function): (usize, &String)| !functions[..index].contains(function);
        require(
            !functions.is_empty()
                && functions.iter().enumerate().all(once)
                && functions.iter().all(|function| words(function, "::")),
            "mutation_function_selection",
        )?;
        files.push((name, functions));
    }
    let filters = strings(&value["testFilters"], POLICY_FORM)?;
    let filter = |filter: &String| {
        let plain = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':';
        let begun = filter.starts_with(|first: char| first.is_ascii_alphabetic() || first == '_');
        begun && filter.bytes().all(plain)
    };
    require(
        !files.is_empty()
            && files.len() <= MAX_POLICY_FILES
            && !filters.is_empty()
            && filters.len() <= MAX_TEST_FILTERS
            && filters.iter().all(filter),
        "mutation_test_selection",
    )?;
    let [
        maximum,
        minimum_tests,
        build_seconds,
        test_seconds,
        total_seconds,
    ] = numbers;
    Ok(Policy {
        version: TOOL.to_owned(),
        maximum,
        minimum_tests,
        build_seconds,
        test_seconds,
        total_seconds,
        files,
        filters,
    })
}

/// The arguments that select every mutation of exactly the reviewed
/// functions, never a sample: the files, and a pattern that the tool's name
/// for a mutant matches only when it changes one of the functions.
pub fn selection(policy: &Policy) -> Vec<String> {
    let names = policy
        .files
        .iter()
        .flat_map(|(_, functions)| functions.iter().map(String::as_str));
    let names: Vec<&str> = names.collect();
    let pattern = format!("(?:replace |in )(?:{})(?: ->|$)", names.join("|"));
    let files = policy
        .files
        .iter()
        .flat_map(|(file, _)| ["--file".to_owned(), file.clone()]);
    files.chain(["--re".to_owned(), pattern]).collect()
}

/// The command that lists the selected mutants, or that makes each in the
/// private copy `source` and runs the tests with its results in `output`.
/// Mutation in place implies one mutation at a time.
pub fn command(
    tool: &Path,
    source: &Path,
    output: &Path,
    policy: &Policy,
    listing: bool,
) -> Vec<OsString> {
    let fixed = [
        "--no-config",
        "--no-shuffle",
        "--colors=never",
        "--annotations=none",
        "-C=--locked",
        "-C=--offline",
        "-C=--lib",
    ];
    let mut argv: Vec<OsString> =
        vec![tool.into(), "mutants".into(), "--dir".into(), source.into()];
    argv.extend(fixed.map(OsString::from));
    argv.extend(selection(policy).into_iter().map(OsString::from));
    if listing {
        argv.extend(["--list", "--json"].map(OsString::from));
        return argv;
    }
    let bounded = [
        "--in-place".to_owned(),
        "--baseline=run".to_owned(),
        "--cap-lints=true".to_owned(),
        "--jobserver-tasks=2".to_owned(),
        format!("--build-timeout={}", policy.build_seconds),
        format!("--timeout={}", policy.test_seconds),
        "--output".to_owned(),
    ];
    argv.extend(bounded.map(OsString::from));
    argv.push(output.into());
    argv.extend(["--", "--"].map(OsString::from));
    argv.extend(policy.filters.iter().map(OsString::from));
    argv.push(OsStr::new("--test-threads=1").into());
    argv
}

/// The field `name` of a record the tool wrote.
fn field<'a>(record: &'a Map<String, Value>, name: &str, form: Fault) -> Result<&'a Value> {
    record.get(name).ok_or(form)
}

/// The mutants the tool listed, each by its name and without its
/// difference from the source, in the order of the listing. Every mutant
/// changes a selected function of its file, none is listed twice, there
/// are no more than the policy allows, and every selected function has one.
pub fn inventory(listing: &[u8], policy: &Policy) -> Result<Vec<(String, Value)>> {
    let values = record::strict(listing).map_err(|_| LISTING_FORM)?;
    let mut result: Vec<(String, Value)> = Vec::new();
    let mut observed = Vec::new();
    for raw in values.as_array().ok_or(LISTING_FORM)? {
        let item = raw.as_object().ok_or(LISTING_FORM)?;
        let name = field(item, "name", LISTING_FORM)?
            .as_str()
            .ok_or(LISTING_FORM)?;
        let file = field(item, "file", LISTING_FORM)?
            .as_str()
            .ok_or(LISTING_FORM)?;
        let function = field(item, "function", LISTING_FORM)?
            .as_object()
            .ok_or(LISTING_FORM)?;
        let function = field(function, "function_name", LISTING_FORM)?
            .as_str()
            .ok_or(LISTING_FORM)?;
        let chosen = policy.files.iter().find(|(chosen, _)| chosen == file);
        require(
            chosen.is_some_and(|(_, functions)| functions.iter().any(|chosen| chosen == function)),
            "mutation_unselected_function",
        )?;
        require(
            result.iter().all(|(listed, _)| listed != name),
            "mutation_duplicate",
        )?;
        let mut kept = item.clone();
        kept.remove("diff");
        result.push((name.to_owned(), kept.into()));
        observed.push((file.to_owned(), function.to_owned()));
    }
    let covered = |(file, functions): &(String, Vec<String>)| {
        let seen = |function: &String| observed.iter().any(|(at, of)| at == file && of == function);
        functions.iter().all(seen)
    };
    require(
        result.len() as u64 <= policy.maximum && policy.files.iter().all(covered),
        "mutation_inventory_coverage",
    )?;
    Ok(result)
}

/// The digits at the start of `text` as a number, and what follows them. A
/// number too long to hold counts as the most there is.
fn counted(text: &[u8]) -> Option<(u64, &[u8])> {
    let digits = text.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let number = std::str::from_utf8(&text[..digits]).ok()?;
    let rest = &text[digits..];
    (digits > 0).then(|| (number.parse().unwrap_or(u64::MAX), rest))
}

/// The one line of a test run's result at the start of `text`: how many
/// tests passed and how many failed. A result with ignored or measured
/// tests is none.
fn result(text: &[u8]) -> Option<(u64, u64)> {
    let rest = text.strip_prefix(b"test result: ")?;
    let rest = rest
        .strip_prefix(b"ok")
        .or_else(|| rest.strip_prefix(b"FAILED"))?;
    let (passed, rest) = counted(rest.strip_prefix(b". ")?)?;
    let (failed, rest) = counted(rest.strip_prefix(b" passed; ")?)?;
    let rest = rest.strip_prefix(b" failed; 0 ignored; 0 measured; ")?;
    let (_, rest) = counted(rest)?;
    rest.strip_prefix(b" filtered out")?;
    Some((passed, failed))
}

/// How many tests passed and failed in the log that `row` names, below
/// `directory`. The log is one of the tool's own, by its plain name, and
/// holds exactly one result: a run without tests, a compiler error, a crash
/// and a second run are none.
fn test_counts(row: &Map<String, Value>, directory: &Path) -> Result<(u64, u64)> {
    let name = field(row, "log_path", RESULT_FORM)?
        .as_str()
        .ok_or(RESULT_FORM)?;
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-');
    let stem = name
        .strip_prefix("log/")
        .and_then(|log| log.strip_suffix(".log"));
    require(
        stem.is_some_and(|stem| !stem.is_empty() && stem.bytes().all(plain)),
        "mutation_log_path",
    )?;
    let text = files::bounded(&directory.join(name), MAX_REPORT, false)?;
    // No result holds the beginning of another, so each is found where it
    // begins.
    let found: Vec<(u64, u64)> = (0..text.len())
        .filter_map(|at| result(&text[at..]))
        .collect();
    match found.as_slice() {
        [counts] => Ok(*counts),
        _ => Err(Fault("mutation_test_result_missing")),
    }
}

/// What the results in `directory` say of the `expected` mutants: the
/// unchanged sources built and passed enough tests, every listed mutant has
/// one result that is its own, a caught mutant built and failed its tests
/// by an assertion with enough tests run, and the tool's counts are the
/// counts of the results. The check passes when every mutant was caught.
pub fn assess(directory: &Path, expected: &[(String, Value)], policy: &Policy) -> Result<Value> {
    let content = files::bounded(&directory.join("outcomes.json"), MAX_REPORT, false)?;
    let document = record::strict(&content).map_err(|_| RESULT_FORM)?;
    let value = document.as_object().ok_or(RESULT_FORM)?;
    require(
        value.get("cargo_mutants_version") == Some(&Value::from(policy.version.as_str()))
            && value.get("end_time").is_some_and(Value::is_string),
        "mutation_result_incomplete",
    )?;
    let rows = field(value, "outcomes", RESULT_FORM)?
        .as_array()
        .ok_or(RESULT_FORM)?;
    let rows: Vec<&Map<String, Value>> = rows
        .iter()
        .map(|row| row.as_object().ok_or(RESULT_FORM))
        .collect::<Result<_>>()?;
    let mut baselines = Vec::new();
    for row in &rows {
        if field(row, "scenario", RESULT_FORM)? == "Baseline" {
            baselines.push(*row);
        }
    }
    let [baseline] = baselines.as_slice() else {
        return Err(Fault("mutation_baseline_failed"));
    };
    require(
        field(baseline, "summary", RESULT_FORM)? == "Success",
        "mutation_baseline_failed",
    )?;
    let (passed, failed) = test_counts(baseline, directory)?;
    require(
        passed >= policy.minimum_tests && failed == 0,
        "mutation_baseline_test_count",
    )?;
    let mut observed: Vec<(&str, &str)> = Vec::new();
    for row in &rows {
        let scenario = &row["scenario"];
        if scenario == "Baseline" {
            continue;
        }
        let mutant = field(
            scenario.as_object().ok_or(RESULT_FORM)?,
            "Mutant",
            RESULT_FORM,
        )?;
        let name = field(mutant.as_object().ok_or(RESULT_FORM)?, "name", RESULT_FORM)?;
        let name = name.as_str().ok_or(RESULT_FORM)?;
        let listed = expected.iter().find(|(listed, _)| listed == name);
        require(
            listed.is_some_and(|(_, listed)| listed == mutant)
                && observed.iter().all(|(seen, _)| *seen != name),
            "mutation_result_identity",
        )?;
        let summary = field(row, "summary", RESULT_FORM)?
            .as_str()
            .ok_or(RESULT_FORM)?;
        require(
            SUMMARIES.iter().any(|(_, kind)| *kind == summary),
            "mutation_result_summary",
        )?;
        observed.push((name, summary));
        if summary == "CaughtMutant" {
            let phases = field(row, "phase_results", RESULT_FORM)?
                .as_array()
                .ok_or(RESULT_FORM)?;
            let mut names = Vec::new();
            for phase in phases {
                names.push(field(
                    phase.as_object().ok_or(RESULT_FORM)?,
                    "phase",
                    RESULT_FORM,
                )?);
            }
            let status = |index: usize| {
                field(
                    phases[index].as_object().ok_or(RESULT_FORM)?,
                    "process_status",
                    RESULT_FORM,
                )
            };
            let ordered = names.len() == 2 && *names[0] == "Build" && *names[1] == "Test";
            let failed =
                ordered && *status(0)? == "Success" && *status(1)? == json!({"Failure": 101});
            require(failed, "mutation_caught_process")?;
            let (passed, failed) = test_counts(row, directory)?;
            require(
                failed > 0 && passed.saturating_add(failed) >= policy.minimum_tests,
                "mutation_caught_assertion",
            )?;
        }
    }
    let whole = observed.len() == expected.len();
    let total = Some(expected.len() as u64);
    require(
        whole && field(value, "total_mutants", RESULT_FORM)?.as_u64() == total,
        "mutation_result_coverage",
    )?;
    let count = |kind: &str| {
        observed
            .iter()
            .filter(|(_, summary)| *summary == kind)
            .count() as u64
    };
    // The tool's counts are read one after another, and no further than
    // the first that disagrees.
    let mut agreed = true;
    for (name, kind) in SUMMARIES.iter().take(4).chain([&("success", "")]) {
        agreed = agreed && field(value, name, RESULT_FORM)?.as_u64() == Some(count(kind));
    }
    require(agreed, "mutation_result_totals")?;
    let counts: Map<String, Value> = SUMMARIES
        .iter()
        .map(|(name, kind)| ((*name).to_owned(), count(kind).into()))
        .collect();
    let mutants: Map<String, Value> = observed
        .iter()
        .map(|(name, summary)| ((*name).to_owned(), (*summary).into()))
        .collect();
    Ok(json!({
        "passed": count("CaughtMutant") == expected.len() as u64,
        "baselineTests": passed,
        "total": expected.len(),
        "counts": counts,
        "mutants": mutants,
    }))
}

#[cfg(test)]
mod tests;
