//! Scanner findings, the migration review and the dependency budget.
//!
//! A scanner that exits with zero has only written a report, and a report can
//! hold findings. Each supported report is read by its own exact rules and
//! every finding needs a review of exactly its identity. A report of another
//! shape, or one that leaves out what is read here, fails the gate: a missing
//! field is never zero findings.

use crate::{
    Fault, Result, digest, files,
    policy::{Date, Exception, permitted},
    record::{self, MAX_DOCUMENT},
    require, scope,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

/// What a report or a policy does not have, though it is read here.
const MISSING: Fault = Fault("scanner_field_missing");
const CONFIGURATION: Fault = Fault("invalid_gitleaks_configuration");
const BROAD: Fault = Fault("gitleaks_broad_allowlist");
const ROUTE: Fault = Fault("zizmor_route");
const STALE: Fault = Fault("cargo_database_stale");
/// The suppressions a new migration may not carry. `squawk-ignore-file` is
/// the second of them followed by a hyphen, which ends a word.
const DIRECTIVES: [&str; 2] = ["squawk-disable-assume-in-transaction", "squawk-ignore"];
const MICROSECONDS: i128 = 1_000_000;

pub(crate) fn object(value: &Value) -> Result<&Map<String, Value>> {
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(Fault("scanner_object_expected")),
    }
}

pub(crate) fn list(value: &Value) -> Result<&[Value]> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(Fault("scanner_list_expected")),
    }
}

pub(crate) fn field<'a>(fields: &'a Map<String, Value>, name: &str) -> Result<&'a Value> {
    fields.get(name).ok_or(MISSING)
}

/// Whether a value counts as set: anything but null, false, zero and empty.
pub(crate) fn set(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// A whole number, never a fraction or a truth value.
fn whole(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        _ => None,
    }
}

/// JSON of one spelling: sorted keys, no spaces, ASCII only, whole numbers.
pub(crate) fn canonical(value: &Value, text: &mut String) -> Result<()> {
    match value {
        Value::Null => text.push_str("null"),
        Value::Bool(flag) => text.push_str(if *flag { "true" } else { "false" }),
        Value::Number(_) => text.push_str(&whole(value).ok_or(BROAD)?.to_string()),
        Value::String(string) => text.push_str(&record::quoted(string)),
        Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                text.push_str(if index == 0 { "" } else { "," });
                canonical(item, text)?;
            }
            text.push(']');
        }
        Value::Object(fields) => {
            let mut names: Vec<&String> = fields.keys().collect();
            names.sort_unstable();
            text.push('{');
            for (index, name) in names.into_iter().enumerate() {
                text.push_str(if index == 0 { "" } else { "," });
                text.push_str(&record::quoted(name));
                text.push(':');
                canonical(&fields[name], text)?;
            }
            text.push('}');
        }
    }
    Ok(())
}

/// A path as a pattern that matches only itself.
fn literal(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len());
    for character in path.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\u{b}\u{c}".contains(character) {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern
}

/// The gitleaks configuration at `path`, as a document.
pub(crate) fn configuration(path: &Path) -> Result<Value> {
    let content = files::bounded(path, MAX_DOCUMENT, false)?;
    let text = std::str::from_utf8(&content).map_err(|_| CONFIGURATION)?;
    toml::from_str(text).map_err(|_| CONFIGURATION)
}

/// One public fixture allowlist of a gitleaks configuration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fixture<'a> {
    /// The whole allowlist: what a review fingerprints.
    entry: &'a Value,
    /// What the allowlist says it is for.
    pub(crate) description: &'a str,
    path: &'a Value,
    target: &'a Value,
    pattern: &'a Value,
}

/// Where a fixture's value is allowed: in the one file its path names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Place<'a> {
    rooted: bool,
    located: String,
    /// What the pattern is matched against.
    pub(crate) target: &'a str,
    /// The pattern of the value.
    pub(crate) pattern: &'a str,
}

impl Place<'_> {
    /// Whether the fixture's path finds the file `name`: the whole name, or
    /// its end after a separator unless the path is anchored at the root.
    pub(crate) fn finds(&self, name: &str) -> bool {
        let above = name.strip_suffix(self.located.as_str());
        above.is_some_and(|above| above.is_empty() || (!self.rooted && above.ends_with('/')))
    }
}

impl<'a> Fixture<'a> {
    /// The file the description names.
    fn name(&self) -> &'a str {
        let described = self.description.strip_prefix("Public fixture: ");
        described.unwrap_or(self.description)
    }

    /// The fixture's place. Its path is a literal one between `(^|/)` or `^`
    /// and `$`, so it is matched without a pattern library and can find
    /// nothing its reviewer did not read; its target and pattern are text.
    pub(crate) fn place(&self) -> Result<Place<'a>> {
        let texts = (
            self.path.as_str(),
            self.target.as_str(),
            self.pattern.as_str(),
        );
        let (Some(path), Some(target), Some(pattern)) = texts else {
            return Err(BROAD);
        };
        let (rooted, located) = located(path).ok_or(Fault("gitleaks_fixture_path"))?;
        Ok(Place {
            rooted,
            located,
            target,
            pattern,
        })
    }
}

/// The path a pattern names and whether the pattern is anchored at the root,
/// when the pattern is that path alone: every character stands for itself,
/// or is punctuation after a backslash.
fn located(pattern: &str) -> Option<(bool, String)> {
    let body = pattern.strip_suffix('$')?;
    let (rooted, body) = match body.strip_prefix("(^|/)") {
        Some(body) => (false, body),
        None => (true, body.strip_prefix('^')?),
    };
    let mut path = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        let escaped = |next: &char| next.is_ascii_punctuation() || *next == ' ';
        path.push(match character {
            '\\' => characters.next().filter(escaped)?,
            '.' | '^' | '$' | '*' | '+' | '?' | '{' | '}' | '[' | ']' | '|' | '(' | ')' => {
                return None;
            }
            plain => plain,
        });
    }
    (!path.is_empty()).then_some((rooted, path))
}

fn fixture(entry: &Value) -> Result<Fixture<'_>> {
    let fields = record::record(
        entry,
        &[
            "description",
            "condition",
            "paths",
            "regexTarget",
            "regexes",
        ],
    )?;
    let (paths, patterns) = (list(&fields["paths"])?, list(&fields["regexes"])?);
    require(
        fields["condition"] == "AND" && paths.len() == 1 && patterns.len() == 1,
        BROAD.0,
    )?;
    Ok(Fixture {
        entry,
        description: record::string(&fields["description"])?,
        path: &paths[0],
        target: &fields["regexTarget"],
        pattern: &patterns[0],
    })
}

/// The public fixture allowlists of a configuration, each read when it is
/// reached: the default rules stay on, and an allowlist is one path and one
/// pattern joined by AND.
pub(crate) fn fixtures(document: &Value) -> Result<impl Iterator<Item = Result<Fixture<'_>>>> {
    let raw = record::record(document, &["title", "extend", "allowlists"])?;
    require(
        raw["extend"] == json!({"useDefault": true}),
        "gitleaks_default_rules_required",
    )?;
    Ok(list(&raw["allowlists"])?.iter().map(fixture))
}

/// Every public fixture allowlist of the gitleaks configuration at `path` is
/// exact, conjunctive and backed by a current review of its whole text.
pub fn gitleaks_configuration(
    path: &Path,
    reviews: &[Exception],
    include_vendor: bool,
) -> Result<()> {
    let document = configuration(path)?;
    for fixture in fixtures(&document)? {
        let fixture = fixture?;
        let name = fixture.name();
        if scope::selected(name, include_vendor) {
            let mut spelled = String::new();
            canonical(fixture.entry, &mut spelled)?;
            let fingerprint = format!("public-fixture:{}", digest(spelled.as_bytes()));
            require(
                permitted(reviews, "gitleaks", &fingerprint, name),
                "unreviewed_gitleaks_allowlist",
            )?;
        } else {
            // A vendor-only fixture cannot authorize a first-party path: its
            // pattern is the fixture itself, anchored at the repository root.
            require(
                *fixture.path == json!(format!("^{}$", literal(name))),
                "gitleaks_vendor_fixture_path",
            )?;
        }
        fixture.place()?;
    }
    Ok(())
}

/// One step of a symbolic route: a key or an index, as text.
fn step(item: &Value) -> Result<String> {
    let component = object(item)?;
    let value = component.get("Key").or_else(|| component.get("Index"));
    match (component.len(), value) {
        (1, Some(Value::String(text))) => Ok(text.clone()),
        (1, Some(number)) => whole(number).map(|number| number.to_string()).ok_or(ROUTE),
        _ => Err(ROUTE),
    }
}

/// The number of zizmor findings in the report `data`, each reviewed by its
/// audit, the exact text it flags and its symbolic place in the workflow.
pub fn zizmor(data: &[u8], reviews: &[Exception]) -> Result<usize> {
    let report = record::document(data)?;
    let findings = list(&report)?;
    for value in findings {
        let finding = object(value)?;
        require(
            !finding.get("ignored").is_some_and(set),
            "zizmor_inline_ignore_forbidden",
        )?;
        let mut primary = Vec::new();
        for item in list(field(finding, "locations")?)? {
            let location = object(item)?;
            if field(object(field(location, "symbolic")?)?, "kind")? == "Primary" {
                primary.push(location);
            }
        }
        let [location] = primary[..] else {
            return Err(Fault("zizmor_primary_location"));
        };
        let symbolic = object(field(location, "symbolic")?)?;
        let local = object(field(object(field(symbolic, "key")?)?, "Local")?)?;
        let route = list(field(object(field(symbolic, "route")?)?, "route")?)?;
        let steps = route.iter().map(step).collect::<Result<Vec<_>>>()?;
        let place = format!(
            "{}#{}",
            record::string(field(local, "verbatim_path")?)?,
            steps.join("/")
        );
        let feature = record::string(field(object(field(location, "concrete")?)?, "feature")?)?;
        let fingerprint = format!(
            "{}:{}",
            record::string(field(finding, "ident")?)?,
            digest(feature.as_bytes())
        );
        require(
            permitted(reviews, "zizmor", &fingerprint, &place),
            "unreviewed_zizmor_finding",
        )?;
    }
    Ok(findings.len())
}

/// Digits only, as a number.
fn digits(text: &str) -> Option<i128> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The microseconds since the epoch of a date and time with a zone, written
/// as `YYYY-MM-DD`, any one character, `HH` with optional `:MM` and `:SS`, an
/// optional fraction after a point or a comma, and `Z` or an offset of hours
/// with optional minutes. A time without a zone is `Ok(None)`; anything else
/// is no time.
fn instant(text: &str) -> Result<Option<i128>> {
    let day = text.get(..10).and_then(Date::parse).ok_or(STALE)?;
    let mut rest = text[10..].chars();
    if rest.next().is_none() {
        return Ok(None);
    }
    let clock = rest.as_str();
    let zone = clock.find(['Z', '+', '-']).unwrap_or(clock.len());
    let (time, offset) = clock.split_at(zone);
    let (whole, fraction) = time.split_once(['.', ',']).unwrap_or((time, "0"));
    let mut parts = whole.split(':');
    let mut part = |most: i128| {
        let text = parts.next().unwrap_or("00");
        digits(text).filter(|value| text.len() == 2 && *value <= most)
    };
    let (hour, minute, second) = (part(23), part(59), part(59));
    let fraction = digits(&format!("{fraction:0<6.6}")).filter(|_| digits(fraction).is_some());
    let (Some(hour), Some(minute), Some(second), Some(fraction), None) =
        (hour, minute, second, fraction, parts.next())
    else {
        return Err(STALE);
    };
    let east = match offset {
        "" => return Ok(None),
        "Z" => 0,
        _ => {
            let (sign, amount) = offset.split_at(1);
            let amount = amount.replacen(':', "", 1);
            let amount = if amount.len() == 2 {
                amount + "00"
            } else {
                amount
            };
            let minutes = digits(&amount)
                .filter(|value| amount.len() == 4 && value / 100 <= 23 && value % 100 <= 59)
                .map(|value| value / 100 * 60 + value % 100)
                .ok_or(STALE)?;
            if sign == "-" { -minutes } else { minutes }
        }
    };
    let seconds =
        i128::from(day.epoch_day()) * 86_400 + hour * 3600 + minute * 60 + second - east * 60;
    Ok(Some(seconds * MICROSECONDS + fraction))
}

/// The number of advisories in the cargo-audit report `data`, vulnerabilities
/// and warnings alike, each reviewed for its package version. The report must
/// be unfiltered, of the whole lock file of `dependencies` packages, and of an
/// advisory database that is at most thirty days old at `now`, the time since
/// the epoch.
pub fn cargo(
    data: &[u8],
    reviews: &[Exception],
    dependencies: u64,
    now: Duration,
) -> Result<usize> {
    let report = record::document(data)?;
    let raw = object(&report)?;
    let unfiltered = json!({
        "target_arch": [],
        "target_os": [],
        "severity": null,
        "ignore": [],
        "informational_warnings": ["unmaintained", "unsound", "notice"],
    });
    require(
        *field(raw, "settings")? == unfiltered,
        "cargo_filtered_report",
    )?;
    let lock = object(field(raw, "lockfile")?)?;
    require(
        whole(field(lock, "dependency-count")?) == Some(i128::from(dependencies)),
        "cargo_incomplete_lockfile",
    )?;
    let database = object(field(raw, "database")?)?;
    let commit = record::string(field(database, "last-commit")?)?;
    let identified = commit.len() == 40
        && commit
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && whole(field(database, "advisory-count")?).is_some_and(|count| count > 0);
    require(identified, "cargo_database_identity")?;
    let updated = instant(record::string(field(database, "last-updated")?)?)?.ok_or(STALE)?;
    let age = i128::try_from(now.as_micros()).map_err(|_| STALE)? - updated;
    require(
        (-2 * 3600 * MICROSECONDS..=30 * 86_400 * MICROSECONDS).contains(&age),
        STALE.0,
    )?;
    let vulnerabilities = object(field(raw, "vulnerabilities")?)?;
    let mut entries: Vec<&Value> = list(field(vulnerabilities, "list")?)?.iter().collect();
    let counted = whole(field(vulnerabilities, "count")?) == i128::try_from(entries.len()).ok()
        && *field(vulnerabilities, "found")? == Value::Bool(!entries.is_empty());
    require(counted, "cargo_inconsistent_report")?;
    for values in object(field(raw, "warnings")?)?.values() {
        entries.extend(list(values)?);
    }
    for value in &entries {
        let item = object(value)?;
        let package = object(field(item, "package")?)?;
        let advisory = object(field(item, "advisory")?)?;
        let place = format!(
            "{}@{}",
            record::string(field(package, "name")?)?,
            record::string(field(package, "version")?)?
        );
        require(
            permitted(
                reviews,
                "cargo-audit",
                record::string(field(advisory, "id")?)?,
                &place,
            ),
            "unreviewed_cargo_advisory",
        )?;
    }
    Ok(entries.len())
}

/// Every package that the Cargo metadata `data` holds in more than one version
/// is in the reviewed budget at `path`, with no version beyond it.
pub fn dependency_budget(data: &[u8], path: &Path) -> Result<()> {
    let metadata = record::document(data)?;
    let mut versions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for value in list(field(object(&metadata)?, "packages")?)? {
        let package = object(value)?;
        versions
            .entry(record::string(field(package, "name")?)?)
            .or_default()
            .insert(record::string(field(package, "version")?)?);
    }
    let content = files::bounded(path, MAX_DOCUMENT, false)?;
    let budget = record::document(&content)?;
    let raw = record::record(
        &budget,
        &["schemaVersion", "reviewedRevision", "duplicates"],
    )?;
    require(
        raw["schemaVersion"].as_f64() == Some(1.0),
        "dependency_budget_schema",
    )?;
    let allowed = object(&raw["duplicates"])?;
    for (name, current) in versions.iter().filter(|(_, current)| current.len() > 1) {
        let group = allowed
            .get(*name)
            .ok_or(Fault("new_duplicate_dependency_group"))?;
        let reviewed = list(group)?
            .iter()
            .map(record::string)
            .collect::<Result<BTreeSet<_>>>()?;
        require(
            current.is_subset(&reviewed),
            "new_duplicate_dependency_version",
        )?;
    }
    Ok(())
}

/// A letter as the reference's caseless search reads it.
fn folded(character: char) -> char {
    let mut upper = character.to_uppercase();
    let single = match (upper.next(), upper.next()) {
        (Some(only), None) => only,
        _ => character,
    };
    let mut lower = single.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(only), None) => only,
        _ => single,
    }
}

/// Whether SQL carries a squawk suppression as a word of its own, in any case.
fn suppressed(sql: &str) -> bool {
    let word = |character: &char| character.is_alphanumeric() || *character == '_';
    let letters: Vec<char> = sql.chars().collect();
    let caseless: Vec<char> = letters.iter().copied().map(folded).collect();
    (0..letters.len()).any(|start| {
        (start == 0 || !word(&letters[start - 1]))
            && DIRECTIVES.iter().any(|directive| {
                let end = start + directive.len();
                caseless
                    .get(start..end)
                    .is_some_and(|found| found.iter().copied().eq(directive.chars()))
                    && !letters.get(end).is_some_and(word)
            })
    })
}

/// The SQL files directly in `root/migrations`, by their path from `root`.
fn migrations(root: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let entries = match std::fs::read_dir(root.join("migrations")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(files::unusable(&error)),
    };
    for entry in entries {
        let name = entry.map_err(|error| files::unusable(&error))?.file_name();
        let name = name.to_str().ok_or(Fault("migration_name"))?;
        if name.ends_with(".sql") {
            names.insert(format!("migrations/{name}"));
        }
    }
    Ok(names)
}

/// The migrations below `root` that the reviewed baseline does not name, in
/// name order. Every reviewed one must still be there with its reviewed
/// bytes, and no new one may suppress the linter it is about to meet.
pub fn new_migrations(root: &Path) -> Result<Vec<String>> {
    let path = root.join("security/migration-baseline.json");
    let content = files::bounded(&path, MAX_DOCUMENT, false)?;
    let document = record::document(&content)?;
    let raw = record::record(&document, &["schemaVersion", "reviewedRevision", "files"])?;
    require(
        raw["schemaVersion"].as_f64() == Some(1.0),
        "migration_baseline_schema",
    )?;
    let mut baseline = BTreeMap::new();
    for item in list(&raw["files"])? {
        let entry = record::record(item, &["path", "sha256"])?;
        let name = record::string(&entry["path"])?;
        require(!baseline.contains_key(name), "duplicate_reviewed_migration")?;
        baseline.insert(name, record::string(&entry["sha256"])?);
    }
    let current = migrations(root)?;
    require(
        baseline.keys().all(|name| current.contains(*name)),
        "reviewed_migration_deleted",
    )?;
    for (name, expected) in &baseline {
        let bytes = files::bounded(&root.join(name), MAX_DOCUMENT, false)?;
        require(digest(&bytes) == *expected, "reviewed_migration_modified")?;
    }
    let added: Vec<String> = current
        .into_iter()
        .filter(|name| !baseline.contains_key(name.as_str()))
        .collect();
    for name in &added {
        let bytes = files::bounded(&root.join(name), MAX_DOCUMENT, false)?;
        let sql = std::str::from_utf8(&bytes).map_err(|_| Fault("migration_not_text"))?;
        require(!suppressed(sql), "migration_inline_suppression_forbidden")?;
    }
    Ok(added)
}

#[cfg(test)]
mod tests;
