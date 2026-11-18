//! The evidence of a local analysis held to what it claims: a database
//! finished by the pinned CLI in the build mode of its language, one
//! complete original report of the language's own category with real
//! queries and every Rust coverage counter, every language and suite of the
//! run passed by its policy, and a fresh first-party report of an automated
//! language that may be uploaded whatever its findings.

use super::{RUST_EXTRACTION_STATISTICS, categories};
use crate::{
    Fault, Result,
    codeql::{
        alike,
        tools::{self, AUTOMATED_LANGUAGES, FIRST_PARTY_CONFIG, Field},
        triage::{
            MAX_REPORT, NO_FIELDS, NO_ITEMS, among, array, bounded_file, decode, field, get,
            get_or, integer, object, raised_by, same, sarif::first_party_report, string, truthy,
        },
    },
    context::Outcome,
    document::Document,
    require,
};
use std::{collections::BTreeSet, ffi::OsString, path::Path};

/// The most bytes of a database's metadata.
const MAX_METADATA: u64 = 65536;
/// The most bytes of a report's marker.
const MAX_MARKER: u64 = 4096;
/// The levels of a notification that leave an analysis incomplete.
const INCOMPLETE: [&str; 2] = ["error", "warning"];
/// What the reference's interpreter raised for a value that is no record
/// asked for a field.
const UNSUBSCRIPTABLE: Fault = Fault("TypeError");

/// The original Rust coverage counters of the run `run`, by their labels in
/// the order the report gives them, without a source excerpt or a verdict:
/// none for another language, and for Rust every one, once each and none
/// negative.
pub fn extraction_statistics(run: &Document, language: &str) -> Result<Document> {
    let mut result: Vec<(String, Document)> = Vec::new();
    if language != "rust" {
        return Ok(Document::Record(result));
    }
    let properties = object(get_or(run, "properties", &NO_FIELDS))?;
    for value in array(get_or(properties, "metricResults", &NO_ITEMS))? {
        let metric = object(value)?;
        if !same(get(metric, "ruleId"), "rust/summary/summary-statistics") {
            continue;
        }
        let label = string(field(object(field(metric, "message")?)?, "text")?)?;
        if RUST_EXTRACTION_STATISTICS.contains(&label) {
            let value = field(metric, "value")?;
            let first = !result.iter().any(|(held, _)| held == label);
            require(
                integer(value)? >= 0 && first,
                "codeql_rust_extraction_metric",
            )?;
            result.push((label.to_owned(), value.clone()));
        }
    }
    let found: BTreeSet<&str> = result.iter().map(|(label, _)| label.as_str()).collect();
    require(
        found == RUST_EXTRACTION_STATISTICS.into_iter().collect(),
        "codeql_rust_extraction_metrics_missing",
    )?;
    Ok(Document::Record(result))
}

/// Refuse a database of `language` below `database` that is unfinished, an
/// overlay, of another CLI or language than the pin of the checkout `root`
/// names, or of another build mode than its language's: a native one is
/// traced, every other is built without a build.
pub fn database_health(root: &Path, database: &Path, language: &str) -> Outcome<()> {
    let metadata = bounded_file(&database.join("codeql-database.yml"), MAX_METADATA)?;
    let text = String::from_utf8(metadata).map_err(|_| Fault("UnicodeDecodeError"))?;
    let selected = tools::language_pin(root, language)?;
    let extractor = string(field(&selected, "extractor")?)?;
    tools::scalar(&text, Field::PRIMARY_LANGUAGE, extractor)?;
    let pinned = tools::pin(root)?;
    let cli = string(field(&pinned, "cliVersion")?)?;
    tools::scalar(&text, Field::CLI_VERSION, cli)?;
    tools::scalar(&text, Field::FINALISED, "true")?;
    tools::scalar(&text, Field::OVERLAY_BASE_DATABASE, "false")?;
    tools::scalar(&text, Field::OVERLAY_DATABASE, "false")?;
    let mode = if language == "c-cpp" {
        "manual"
    } else {
        "none"
    };
    tools::scalar(&text, Field::BUILD_MODE, mode)
}

/// The identity of the one complete original report at `path` of
/// `language`'s suite `category`, before its policy is enforced: SARIF 2.1.0
/// of one run whose every invocation succeeded without an error or a
/// warning, of the suite's own category, of no incremental mode, of the
/// pinned CodeQL and of at least one query.
pub fn report_health(
    root: &Path,
    path: &Path,
    language: &str,
    category: &str,
) -> Outcome<Document> {
    let raw = decode(&bounded_file(path, MAX_REPORT)?)?;
    let raw = object(&raw)?;
    require(same(get(raw, "version"), "2.1.0"), "codeql_sarif_version")?;
    let runs = array(field(raw, "runs")?)?;
    require(runs.len() == 1, "codeql_local_report_runs")?;
    let run = object(&runs[0])?;
    let invocations = array(field(run, "invocations")?)?;
    require(!invocations.is_empty(), "codeql_missing_execution")?;
    for invocation in invocations {
        let details = object(invocation)?;
        let succeeded = get(details, "executionSuccessful") == &Document::Bool(true);
        require(succeeded, "codeql_execution_failed")?;
        for key in [
            "toolExecutionNotifications",
            "toolConfigurationNotifications",
        ] {
            for notice in array(get_or(details, key, &NO_ITEMS))? {
                let level = get(object(notice)?, "level");
                require(!among(level, &INCOMPLETE)?, "codeql_incomplete_execution")?;
            }
        }
    }
    let automation = object(field(run, "automationDetails")?)?;
    let identity = string(field(automation, "id")?)?;
    require(
        identity.trim_end_matches('/') == format!("/language:{language}/{category}"),
        "codeql_local_report_category",
    )?;
    let properties = object(get_or(run, "properties", &NO_FIELDS))?;
    require(
        !truthy(get(properties, "incrementalMode")),
        "codeql_local_incremental_report",
    )?;
    let tool = object(field(run, "tool")?)?;
    let driver = object(field(tool, "driver")?)?;
    require(same(get(driver, "name"), "CodeQL"), "codeql_sarif_tool")?;
    let version = get_or(driver, "semanticVersion", get(driver, "version"));
    let pinned = tools::pin(root)?;
    require(
        alike(version, field(&pinned, "cliVersion")?),
        "codeql_local_report_version",
    )?;
    let mut components = vec![driver];
    for extension in array(get_or(tool, "extensions", &NO_ITEMS))? {
        components.push(object(extension)?);
    }
    let mut count = 0;
    for component in components {
        count += array(get_or(component, "rules", &NO_ITEMS))?.len();
    }
    require(count > 0, "codeql_local_empty_queries")?;
    let cli = string(field(&pinned, "cliVersion")?)?;
    let selected = tools::language_pin(root, language)?;
    let queries = string(field(&selected, "queriesVersion")?)?;
    let results = array(field(run, "results")?)?.len();
    let statistics = extraction_statistics(run, language)?;
    let content = std::fs::read(path).map_err(|error| raised_by(&error))?;
    Ok(Document::record([
        ("language", language.into()),
        ("category", category.into()),
        ("cliVersion", cli.into()),
        ("queriesVersion", queries.into()),
        ("rules", count.into()),
        ("results", results.into()),
        ("extractionStatistics", statistics),
        ("sarifSha256", crate::digest(&content).into()),
    ]))
}

/// The field `name` of `report`, as the reference asked a value for one: a
/// value that is no record cannot be asked.
fn subscript<'a>(report: &'a Document, name: &str) -> Result<&'a Document> {
    match report {
        Document::Record(_) => field(report, name),
        _ => Err(UNSUBSCRIPTABLE),
    }
}

/// Whether `value` is a positive whole number and no truth value.
fn counted(value: &Document) -> bool {
    let whole = match value {
        Document::Number(_) => value.whole(),
        _ => None,
    };
    whole.is_some_and(|count| count > 0)
}

/// Refuse `reports` unless they are exactly one passed report of every
/// language of `languages` and every suite `suite` asks for, each of at
/// least one query, and each of the native language of compiled files.
pub fn complete_health(reports: &[Document], languages: &[&str], suite: &str) -> Result<()> {
    let wanted = categories(suite);
    let expected: BTreeSet<(&str, &str)> = languages
        .iter()
        .flat_map(|language| wanted.iter().map(move |category| (*language, *category)))
        .collect();
    let mut observed = BTreeSet::new();
    for report in reports {
        let language = string(subscript(report, "language")?)?;
        let category = string(subscript(report, "category")?)?;
        let identity = (language, category);
        require(
            expected.contains(&identity) && !observed.contains(&identity),
            "codeql_local_report_inventory",
        )?;
        require(
            subscript(report, "policyPassed")? == &Document::Bool(true),
            "codeql_local_policy_failed",
        )?;
        require(
            counted(subscript(report, "rules")?),
            "codeql_local_empty_queries",
        )?;
        if language == "c-cpp" {
            let compiled = subscript(report, "nativeCompiledFiles")?;
            require(counted(compiled), "codeql_local_native_coverage")?;
        }
        observed.insert(identity);
    }
    require(observed == expected, "codeql_local_incomplete_languages")
}

/// Whether the report of `language`'s suite `category` below `output` is a
/// fresh, complete, first-party report of `revision` that may be uploaded
/// whatever its findings: one of an automated language, validated by this
/// run's analysis and unchanged since. A report that was never validated is
/// not; one whose marker is of another report, revision or scope is refused.
pub fn upload_ready(
    root: &Path,
    output: &Path,
    revision: &str,
    category: &str,
    language: &str,
) -> Outcome<bool> {
    require(
        AUTOMATED_LANGUAGES.contains(&language),
        "codeql_vendor_upload_forbidden",
    )?;
    let report = output.join(format!("{language}-{category}.sarif"));
    let marker = output.join(format!("{language}-{category}.validated.json"));
    if !marker.exists() {
        return Ok(false);
    }
    let expected = decode(&bounded_file(&marker, MAX_MARKER)?)?;
    let expected = object(&expected)?;
    let identity = report_health(root, &report, language, category)?;
    let validated = Document::record([
        ("revision", revision.into()),
        ("sarifSha256", field(&identity, "sarifSha256")?.clone()),
        ("includeVendor", false.into()),
    ]);
    require(alike(expected, &validated), "codeql_upload_identity")?;
    let raw = decode(&bounded_file(&report, MAX_REPORT)?)?;
    first_party_report(object(&raw)?)?;
    Ok(true)
}

/// The options that keep an extraction of `language` from the sources at
/// `source` to the first-party sources while Cargo may still build the
/// vendored dependencies: none for a vendor scan.
pub fn extraction_options(source: &Path, language: &str, include_vendor: bool) -> Vec<OsString> {
    if include_vendor {
        return Vec::new();
    }
    let mut configuration = OsString::from("--codescanning-config=");
    configuration.push(source.join(FIRST_PARTY_CONFIG));
    let mut options = vec![configuration];
    if language == "rust" {
        options.push("--extractor-option=extract_dependencies_as_source=false".into());
    }
    options
}

/// Refuse a report at `report` of `language` that is not of the scope it
/// was asked for: the native language only in a vendor scan, and no
/// vendored result in a first-party one.
pub fn validate_report_scope(report: &Path, language: &str, include_vendor: bool) -> Result<()> {
    require(
        include_vendor || AUTOMATED_LANGUAGES.contains(&language),
        "codeql_vendor_scope",
    )?;
    if !include_vendor {
        let raw = decode(&bounded_file(report, MAX_REPORT)?)?;
        first_party_report(object(&raw)?)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_reports_tests.rs"]
mod tests;
