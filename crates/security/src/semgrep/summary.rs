//! What a scan may say in public when it is over.
//!
//! A scan ends with one line and leaves private evidence behind. The gate
//! reads both again as an outsider would: the line must be the scan's report
//! or its failure in exactly their forms, and must agree with the status the
//! scan ended with. Of a failed scan, the engine's own reports are searched
//! for what went wrong, and only fixed words leave them: the kind of an
//! error out of a reviewed list, and a path, a line and a rule when they are
//! ones the scan itself knew. No message of the engine, no name of a
//! function and no matched text is ever copied.

use super::{MAX_FILES, MAX_LINE, PREFIXES, RULES, language, rules};
use crate::{
    Fault, Result,
    context::Note,
    files,
    record::{self, safe_name},
    require,
    secrets::MANIFEST,
};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, path::Path};

/// The most findings, errors and timeouts that are shown.
pub(super) const MAX_ITEMS: usize = 32;
/// The most reports of the engine a scan leaves.
pub(super) const MAX_ENGINE_REPORTS: usize = 2;
/// How deep in what the engine says of a timeout a known word is looked for.
pub(super) const MAX_DEPTH: usize = 4;
/// How many parts of what the engine says of a timeout are looked at.
pub(super) const MAX_NODES: usize = 64;
/// The most bytes of the scan's line and of one report of the engine.
pub(super) const MAX_REPORT: u64 = 16 * 1024 * 1024;
/// What is said of a scan whose line or evidence is in no reviewed form.
const INVALID: Fault = Fault("semgrep_invalid_result");
/// What is said of a failure that is not among the reviewed ones.
const UNLISTED: &str = "semgrep_check_failed";
/// The failures of a scan that are said as they are.
pub(super) const ERRORS: [&str; 14] = [
    "semgrep_check_failed",
    "semgrep_engine_errors",
    "semgrep_engine_version",
    "semgrep_skipped_rules",
    "semgrep_fixpoint_timeout",
    "semgrep_incomplete_rules",
    "semgrep_incomplete_scan",
    "semgrep_unknown_finding",
    "semgrep_finding_line",
    "semgrep_inline_ignore",
    "semgrep_golden_false_positive",
    "semgrep_golden_mismatch",
    "scanner_command_failed_source-engine",
    "scanner_command_failed_golden-engine",
];
/// The kinds of error of the engine that are said as they are.
pub(super) const KINDS: [&str; 13] = [
    "Timeout",
    "ParseError",
    "LexicalError",
    "PartialParsing",
    "MatchingError",
    "OutOfMemory",
    "StackOverflow",
    "FatalError",
    "OtherParseError",
    "AstBuilderError",
    "RuleParseError",
    "PatternParseError",
    "InvalidRuleSchemaError",
];
/// The fields of a scan's report.
const FIELDS: [&str; 10] = [
    "schemaVersion",
    "includeVendor",
    "toolLockSha256",
    "engineVersion",
    "rulesSha256",
    "sourceManifestSha256",
    "goldenAssertions",
    "sourceFiles",
    "findings",
    "passed",
];

/// The maintained sources of the scan's private inventory in `directory`,
/// each of one of `languages`: the only paths that may be said. An inventory
/// that cannot be read names none.
fn inventory(directory: &Path, languages: &[(&str, &str)]) -> BTreeSet<String> {
    let read = || -> Result<BTreeSet<String>> {
        let content = files::bounded(&directory.join("source-manifest.json"), MANIFEST, true)?;
        let manifest = record::strict(&content)?;
        let manifest = manifest.as_object().ok_or(INVALID)?;
        require(manifest.len() <= MAX_FILES, INVALID.0)?;
        let maintained = |name: &&String| {
            let placed = PREFIXES.iter().any(|prefix| name.starts_with(prefix));
            safe_name(name) && language(languages, name).is_some() && placed
        };
        Ok(manifest.keys().filter(maintained).cloned().collect())
    };
    read().unwrap_or_default()
}

/// A line of a file: a whole number from one to the last line there may be.
fn line(value: Option<&Value>) -> Option<u64> {
    let line = value.and_then(Value::as_u64);
    line.filter(|line| (1..=MAX_LINE).contains(line))
}

/// The kind of an error of the engine, which names it alone or first of
/// two: one of the reviewed kinds, or `other`.
fn kind(value: Option<&Value>) -> &'static str {
    let tag = match value {
        Some(Value::Array(parts)) if parts.len() == 2 => parts.first(),
        other => other,
    };
    let named = tag.and_then(Value::as_str);
    let known = named.and_then(|name| KINDS.iter().find(|kind| **kind == name));
    known.copied().unwrap_or("other")
}

/// What the engine reported in the runs of the scan in `directory`, in the
/// order of the runs. A directory with more reports than a scan leaves is
/// not read, and neither is a report that is not private, no JSON or not
/// named as a scan names its runs.
fn reports(directory: &Path) -> Vec<Value> {
    let entries = std::fs::read_dir(directory).into_iter().flatten();
    let names = entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok());
    let mut names: Vec<String> = names
        .filter(|name| name.ends_with("-engine.stdout"))
        .collect();
    if names.len() > MAX_ENGINE_REPORTS {
        return Vec::new();
    }
    names.sort_unstable();
    let numbered = |name: &&String| {
        let run = name.strip_suffix("-engine.stdout").unwrap_or(name);
        let run = run.strip_prefix(|digit: char| digit.is_ascii_digit());
        let run = run.and_then(|run| run.strip_prefix(|digit: char| digit.is_ascii_digit()));
        matches!(run, Some("-golden" | "-source"))
    };
    let read = |name: &String| {
        let content = files::bounded(&directory.join(name), MAX_REPORT, true).ok()?;
        record::strict(&content).ok()
    };
    names.iter().filter(numbered).filter_map(read).collect()
}

/// The errors in `reports`: the kind of each, and its path and its line
/// when the path is one of `paths`.
fn engine_errors(reports: &[Value], paths: &BTreeSet<String>) -> Vec<Value> {
    let mut result = Vec::new();
    for raw in reports {
        let Some(Value::Array(errors)) = raw.get("errors") else {
            continue;
        };
        for error in errors {
            let entry = error.as_object();
            let mut public = Map::new();
            let named = kind(entry.and_then(|entry| entry.get("error_type")));
            public.insert("kind".to_owned(), named.into());
            let location = entry.and_then(|entry| entry.get("location")?.as_object());
            let name = location.and_then(|location| location.get("path")?.as_str());
            if let Some(name) = name.filter(|name| paths.contains(*name)) {
                public.insert("path".to_owned(), name.into());
                let start = location.and_then(|location| location.get("start")?.as_object());
                if let Some(line) = line(start.and_then(|start| start.get("line"))) {
                    public.insert("line".to_owned(), line.into());
                }
            }
            result.push(Value::Object(public));
        }
    }
    result
}

/// The one path of `paths` and the one rule of `identifiers` that `value`
/// names. A bounded count of its parts is looked at, the last first, down
/// to a bounded depth; the values of a record are taken in the order of
/// their keys.
fn coordinates(
    value: &Value,
    paths: &BTreeSet<String>,
    identifiers: &BTreeSet<String>,
) -> Map<String, Value> {
    let mut pending = vec![(value, 0)];
    let (mut names, mut named) = (BTreeSet::new(), BTreeSet::new());
    for _ in 0..MAX_NODES {
        let Some((entry, depth)) = pending.pop() else {
            break;
        };
        let below = |child| (child, depth + 1);
        match entry {
            Value::String(text) => {
                names.extend(paths.get(text));
                named.extend(identifiers.get(text));
            }
            Value::Object(fields) if depth < MAX_DEPTH => {
                pending.extend(fields.values().take(MAX_NODES).map(below));
            }
            Value::Array(items) if depth < MAX_DEPTH => {
                pending.extend(items.iter().take(MAX_NODES).map(below));
            }
            _ => {}
        }
    }
    let mut result = Map::new();
    for (name, found) in [("path", names), ("rule", named)] {
        if let (1, Some(only)) = (found.len(), found.first()) {
            result.insert(name.to_owned(), only.as_str().into());
        }
    }
    result
}

/// Where the fixpoint timeouts in `reports` were, as far as they name a
/// path of `paths` or a rule of the pack below `root`, read for `languages`.
fn timeouts(
    root: &Path,
    reports: &[Value],
    paths: &BTreeSet<String>,
    languages: &[(&str, &str)],
) -> Result<Vec<Value>> {
    let identifiers = rules(&root.join(RULES), languages)?;
    let mut result = Vec::new();
    for raw in reports {
        let entries = match raw.get("time") {
            Some(Value::Object(timing)) => timing.get("fixpoint_timeouts"),
            _ => None,
        };
        let Some(Value::Array(entries)) = entries else {
            continue;
        };
        for entry in entries {
            result.push(Value::Object(coordinates(entry, paths, &identifiers)));
        }
    }
    Ok(result)
}

/// The findings of the report `raw`, every one by a rule of the pack below
/// `root`, read for `languages`, in one of `paths` and on a line, and
/// holding nothing else.
fn findings(
    root: &Path,
    raw: &Map<String, Value>,
    paths: &BTreeSet<String>,
    languages: &[(&str, &str)],
) -> Result<Vec<Value>> {
    let identifiers = rules(&root.join(RULES), languages)?;
    let mut result = Vec::new();
    for item in raw["findings"].as_array().ok_or(INVALID)? {
        let entry = item.as_object().ok_or(INVALID)?;
        let rule = entry.get("rule").and_then(Value::as_str);
        let path = entry.get("path").and_then(Value::as_str);
        require(
            entry.len() == 3
                && rule.is_some_and(|rule| identifiers.contains(rule))
                && path.is_some_and(|path| paths.contains(path))
                && line(entry.get("line")).is_some(),
            INVALID.0,
        )?;
        result.push(item.clone());
    }
    Ok(result)
}

/// As many of `items` as are shown, and whether there were more.
fn shown(mut items: Vec<Value>) -> (Value, Value) {
    let more = items.len() > MAX_ITEMS;
    items.truncate(MAX_ITEMS);
    (items.into(), more.into())
}

/// What a scan that failed with `code` may say: the code when it is a
/// reviewed one, and what the engine's reports in `directory` tell, as much
/// as is shown. When the rule pack cannot be read for `languages`, nothing
/// is said of timeouts.
fn failure(
    root: &Path,
    code: &Value,
    directory: &Path,
    paths: &BTreeSet<String>,
    languages: &[(&str, &str)],
) -> Note {
    let listed = code
        .as_str()
        .and_then(|code| ERRORS.iter().find(|listed| **listed == code));
    let reports = reports(directory);
    let (errors, more) = shown(engine_errors(&reports, paths));
    let mut note: Note = vec![
        ("passed", false.into()),
        ("error", (*listed.unwrap_or(&UNLISTED)).into()),
        ("engineErrors", errors),
        ("engineErrorsTruncated", more),
    ];
    if let Ok(timeouts) = timeouts(root, &reports, paths, languages) {
        let (timeouts, more) = shown(timeouts);
        note.push(("fixpointTimeouts", timeouts));
        note.push(("fixpointTimeoutsTruncated", more));
    }
    note
}

/// What the line `data` of a scan that ended with `status` may say, with
/// the scan's evidence in `directory`, the rule pack below `root` and the
/// scan's `languages`.
fn judged(
    root: &Path,
    status: i32,
    data: &[u8],
    directory: &Path,
    languages: &[(&str, &str)],
) -> Result<Note> {
    require(data.len() as u64 <= MAX_REPORT, INVALID.0)?;
    let raw = record::strict(data)?;
    let raw = raw.as_object().ok_or(INVALID)?;
    let paths = inventory(directory, languages);
    if let Some(code) = raw.get("error") {
        let failed = raw.get("passed") == Some(&Value::Bool(false));
        require(raw.len() == 2 && failed && status == 1, INVALID.0)?;
        return Ok(failure(root, code, directory, &paths, languages));
    }
    let counted = |name: &str| {
        let count = raw[name].as_u64();
        count.is_some_and(|count| count > 0 && count <= MAX_FILES as u64)
    };
    require(
        raw.len() == FIELDS.len()
            && FIELDS.iter().all(|name| raw.contains_key(*name))
            && raw["schemaVersion"].as_u64() == Some(1)
            && raw["includeVendor"].is_boolean()
            && counted("sourceFiles")
            && counted("goldenAssertions"),
        INVALID.0,
    )?;
    let entries = findings(root, raw, &paths, languages)?;
    let passed = entries.is_empty();
    require(
        raw["passed"] == passed && status == i32::from(!passed),
        INVALID.0,
    )?;
    let count = entries.len();
    let (entries, more) = shown(entries);
    Ok(vec![
        ("passed", passed.into()),
        ("findingCount", count.into()),
        ("findings", entries),
        ("findingsTruncated", more),
    ])
}

/// What the gate records of a scan of `languages` that ended with `status`
/// and printed `data`: its verdict and its findings, or its failure and what
/// the engine's reports in `directory` tell of it. A line or evidence in no
/// reviewed form, and a line that disagrees with the status, is an invalid
/// result and never a pass.
pub fn diagnosis(
    root: &Path,
    status: i32,
    data: &[u8],
    directory: &Path,
    languages: &[(&str, &str)],
) -> Note {
    let invalid = || vec![("passed", false.into()), ("error", INVALID.0.into())];
    judged(root, status, data, directory, languages).unwrap_or_else(|_| invalid())
}
