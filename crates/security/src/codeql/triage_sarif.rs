//! The findings of a local SARIF report: a successful, complete, nonempty
//! analysis of CodeQL's, each result unsuppressed and unfiltered with one
//! primary location, its severity from its query's security score and
//! never from SARIF's presentation level.

use super::{
    CRITICAL_SCORE, HIGH_SCORE, LANGUAGES, MAX_FINDINGS, MAX_SCORE, MAX_SECONDS, MEDIUM_SCORE,
    NO_FIELDS, NO_ITEMS, VALUE, among, array, field, findings::finding, get, get_or, github::Clock,
    has, holds, object, same, string, truthy,
};
use crate::{
    Fault, Result,
    codeql::sources::{NativeSources, Shown},
    document::Document,
    require,
};
use std::path::Path;

/// The severity of the rule `rule`, from its query's security score: a
/// rule of no score is advisory unless it is tagged as a security query.
pub fn sarif_score(rule: &Document) -> Result<&'static str> {
    let properties = object(get_or(rule, "properties", &NO_FIELDS))?;
    let score = get(properties, "security-severity");
    if *score == Document::Null {
        let tags = array(get_or(properties, "tags", &NO_ITEMS))?;
        require(!holds(tags, "security"), "codeql_missing_score")?;
        return Ok("advisory");
    }
    let text = string(score)?;
    require(scored(text), "codeql_invalid_score")?;
    let value: f64 = text.parse().map_err(|_| VALUE)?;
    require((0.0..=MAX_SCORE).contains(&value), "codeql_invalid_score")?;
    Ok(if value >= CRITICAL_SCORE {
        "critical"
    } else if value >= HIGH_SCORE {
        "high"
    } else if value >= MEDIUM_SCORE {
        "medium"
    } else {
        "low"
    })
}

/// Whether `text` is a score as the reference wrote one: one digit with an
/// optional fraction, or ten with an optional fraction of zeros.
fn scored(text: &str) -> bool {
    let fraction = |rest: &[u8], digit: fn(&u8) -> bool| match rest {
        [] => true,
        [b'.', digits @ ..] => !digits.is_empty() && digits.iter().all(digit),
        _ => false,
    };
    match text.as_bytes() {
        [b'1', b'0', rest @ ..] => fraction(rest, |byte| *byte == b'0'),
        [first, rest @ ..] if first.is_ascii_digit() => fraction(rest, u8::is_ascii_digit),
        _ => false,
    }
}

/// `run`, every `%` and two hexadecimal digits of it the byte they name.
fn unquoted_bytes(run: &[u8]) -> Vec<u8> {
    let mut bits = run.split(|byte| *byte == b'%');
    let mut bytes = bits.next().unwrap_or_default().to_vec();
    let value = |digit: u8| match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        _ => digit - b'A' + 10,
    };
    for item in bits {
        match item {
            [high, low, rest @ ..] if high.is_ascii_hexdigit() && low.is_ascii_hexdigit() => {
                bytes.push(value(*high) << 4 | value(*low));
                bytes.extend_from_slice(rest);
            }
            _ => {
                bytes.push(b'%');
                bytes.extend_from_slice(item);
            }
        }
    }
    bytes
}

/// `text` with its percent escapes decoded as the reference decoded a
/// URI's: each run of ASCII decoded on its own as UTF-8, a byte sequence
/// that is none replaced by the replacement character, and every other
/// character kept.
pub(crate) fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let ascii = rest.bytes().take_while(u8::is_ascii).count();
        if ascii > 0 {
            let bytes = unquoted_bytes(&rest.as_bytes()[..ascii]);
            decoded.push_str(&String::from_utf8_lossy(&bytes));
            rest = &rest[ascii..];
        } else {
            let other = rest.find(|character: char| character.is_ascii());
            let other = other.unwrap_or(rest.len());
            decoded.push_str(&rest[..other]);
            rest = &rest[other..];
        }
    }
    decoded
}

/// The primary location of the physical location `physical`, with CodeQL's
/// omitted defaults written out and the range otherwise unchanged: an end
/// line of the start line, and a start column of one.
pub fn sarif_location(physical: &Document) -> Result<Document> {
    require(
        has(physical, "artifactLocation") && has(physical, "region"),
        "codeql_missing_location",
    )?;
    let artifact = object(field(physical, "artifactLocation")?)?;
    let region = object(field(physical, "region")?)?;
    require(has(artifact, "uri"), "codeql_missing_source_uri")?;
    require(has(region, "startLine"), "codeql_missing_start_line")?;
    // CodeQL writes an end column for a range of lines and columns; one
    // written without it, or by offsets, needs its own exact binding.
    require(has(region, "endColumn"), "codeql_missing_end_column")?;
    let path = unquote(string(field(artifact, "uri")?)?);
    let start = field(region, "startLine")?;
    let first_column = Document::from(1_u64);
    Ok(Document::record([
        ("path", path.into()),
        ("start_line", start.clone()),
        ("end_line", get_or(region, "endLine", start).clone()),
        (
            "start_column",
            get_or(region, "startColumn", &first_column).clone(),
        ),
        ("end_column", field(region, "endColumn")?.clone()),
    ]))
}

/// Refuse a report of a vendored result, rather than leave it out.
pub fn first_party_report(raw: &Document) -> Result<()> {
    for value in array(field(raw, "runs")?)? {
        for item in array(field(object(value)?, "results")?)? {
            for location in array(field(object(item)?, "locations")?)? {
                let physical = object(field(object(location)?, "physicalLocation")?)?;
                let primary = sarif_location(physical)?;
                let path = string(field(&primary, "path")?)?;
                require(!path.starts_with("vendor/"), "codeql_vendor_result")?;
            }
        }
    }
    Ok(())
}

/// The rules of the tool `tool` of a run, its driver's and then its
/// extensions', by their identifiers.
fn rules<'a>(tool: &'a Document, driver: &'a Document) -> Result<Vec<(&'a str, &'a Document)>> {
    let mut components = vec![driver];
    for extension in array(get_or(tool, "extensions", &NO_ITEMS))? {
        components.push(object(extension)?);
    }
    let mut rules: Vec<(&str, &Document)> = Vec::new();
    for component in components {
        for item in array(get_or(component, "rules", &NO_ITEMS))? {
            let rule = object(item)?;
            require(has(rule, "id"), "codeql_missing_rule_id")?;
            let identifier = string(field(rule, "id")?)?;
            let named = rules.iter().any(|(held, _)| *held == identifier);
            require(!named, "codeql_duplicate_rule")?;
            rules.push((identifier, rule));
        }
    }
    Ok(rules)
}

/// Refuse a run whose analyzer did not run every invocation to its end
/// without an error or a warning.
fn executed(run: &Document) -> Result<()> {
    require(has(run, "invocations"), "codeql_missing_execution")?;
    let invocations = array(field(run, "invocations")?)?;
    require(!invocations.is_empty(), "codeql_missing_execution")?;
    for invocation in invocations {
        let details = object(invocation)?;
        let succeeded = *get(details, "executionSuccessful") == Document::Bool(true);
        require(succeeded, "codeql_execution_failed")?;
        for key in [
            "toolExecutionNotifications",
            "toolConfigurationNotifications",
        ] {
            let mut quiet = true;
            for notice in array(get_or(details, key, &NO_ITEMS))? {
                if among(get(object(notice)?, "level"), &["error", "warning"])? {
                    quiet = false;
                    break;
                }
            }
            require(quiet, "codeql_incomplete_execution")?;
        }
    }
    Ok(())
}

/// Every finding of the SARIF report `raw` of the checkout `root` at
/// `revision`, native sources bound through `sources`, as the reference
/// read them: at most so many findings, read before `clock` says the
/// operation's time is up.
pub fn sarif_findings(
    raw: &Document,
    root: &Path,
    revision: &str,
    mut sources: Option<&mut NativeSources<'_>>,
    git: &dyn Shown,
    clock: &dyn Clock,
) -> Result<Vec<Document>> {
    require(same(get(raw, "version"), "2.1.0"), "codeql_sarif_version")?;
    require(has(raw, "runs"), "codeql_sarif_runs")?;
    let runs = array(field(raw, "runs")?)?;
    require(
        !runs.is_empty() && runs.len() <= LANGUAGES.len(),
        "codeql_sarif_runs",
    )?;
    let mut result = Vec::new();
    let deadline = clock.now() + MAX_SECONDS;
    for value in runs {
        let run = object(value)?;
        require(has(run, "tool"), "codeql_sarif_tool")?;
        let tool = object(field(run, "tool")?)?;
        require(has(tool, "driver"), "codeql_sarif_tool")?;
        let driver = object(field(tool, "driver")?)?;
        require(same(get(driver, "name"), "CodeQL"), "codeql_sarif_tool")?;
        let version = string(get_or(driver, "semanticVersion", get(driver, "version")))?;
        executed(run)?;
        let rules = rules(tool, driver)?;
        require(!rules.is_empty(), "codeql_missing_rules")?;
        require(has(run, "results"), "codeql_missing_results")?;
        for entry in array(field(run, "results")?)? {
            require(
                result.len() < MAX_FINDINGS && clock.now() < deadline,
                "codeql_report_limit",
            )?;
            let item = object(entry)?;
            let failed = item.get("kind").is_none_or(|kind| same(kind, "fail"));
            require(
                !truthy(get(item, "suppressions")) && failed,
                "codeql_filtered_result",
            )?;
            require(
                !same(get(item, "baselineState"), "absent"),
                "codeql_filtered_result",
            )?;
            require(
                ["ruleId", "locations", "message"]
                    .iter()
                    .all(|name| has(item, name)),
                "codeql_missing_result_fields",
            )?;
            let identifier = string(field(item, "ruleId")?)?;
            let rule = rules.iter().find(|(held, _)| *held == identifier);
            let (_, rule) = rule.ok_or(Fault("codeql_unknown_rule"))?;
            let locations = array(field(item, "locations")?)?;
            require(locations.len() == 1, "codeql_primary_location")?;
            let primary = object(&locations[0])?;
            require(has(primary, "physicalLocation"), "codeql_missing_location")?;
            let location = sarif_location(object(field(primary, "physicalLocation")?)?)?;
            let message = object(field(item, "message")?)?;
            require(has(message, "text"), "codeql_missing_message")?;
            let text = string(field(message, "text")?)?;
            let sources = sources.as_deref_mut();
            let mut found = finding(
                root, revision, identifier, version, &location, text, sources, git,
            )?;
            found.put("severity", sarif_score(rule)?.into());
            result.push(found);
        }
    }
    Ok(result)
}
