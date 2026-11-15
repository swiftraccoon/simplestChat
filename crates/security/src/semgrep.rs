//! The scan of the maintained sources with reviewed local rules.
//!
//! The pinned standalone engine runs the repository's own rule pack: no
//! registry rule, no network and no inline suppression is consulted. Every
//! check first runs the golden fixtures, whose annotations say where each
//! rule must match and where it must not, and only then scans a snapshot of
//! the maintained sources with the same engine and rules. The engine's own
//! streams stay private. What leaves the check is the name of a reviewed
//! rule, a path of the snapshot and a line. An error of the engine, a rule
//! or a file it left out, a match nobody can place and a fixture that does
//! not behave all fail the check. `summary` says what the gate may record
//! of a scan that is over.

pub mod summary;

use crate::{
    Fault, Result,
    context::{Context, Failure, Note, Outcome, Run, Snapshot},
    files, hexadecimal,
    licenses::splitlines,
    policy::space,
    record::{self, pretty, quoted, safe_name},
    require,
    scope::vendor_path,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

/// Where the rule pack is, below the sources.
pub const RULES: &str = "security/semgrep/rules.json";
/// Where the golden fixtures are, below the sources.
const FIXTURES: &str = "security/semgrep/fixtures";
/// The most bytes of the rule pack and of one fixture.
const MAX_CONFIG: u64 = 1024 * 1024;
/// The most files of one run of the engine.
const MAX_FILES: usize = 20000;
/// The most rules of the pack.
const MAX_RULES: usize = 100;
/// The last line a match may be on.
const MAX_LINE: u64 = 1_000_000;
/// The endings of the files that are scanned, and the language of each.
const LANGUAGES: [(&str, &str); 4] = [
    (".py", "python"),
    (".rs", "rust"),
    (".ts", "typescript"),
    (".js", "typescript"),
];
/// Where the maintained sources are.
const PREFIXES: [&str; 5] = [
    "src/",
    "build/",
    "ops/ansible/files/",
    "ops/ansible/callback_plugins/",
    "web/src/",
];
/// What no rule of the pack may do: run code, or change a file.
const FORBIDDEN: [&str; 4] = [
    "pattern-where-python",
    "fix",
    "fix-regex",
    "r2c-internal-project-depends-on",
];
/// A failure that names nothing: a document in another form, or an error
/// of the system. Its details may hold sources or local paths.
const FAILED: Fault = Fault("semgrep_check_failed");

/// The language of the file `name` by its ending, when it is scanned.
fn language(name: &str) -> Option<&'static str> {
    let last = name.rsplit('/').next().unwrap_or(name);
    let point = last.rfind('.').filter(|at| *at > 0)?;
    let known = LANGUAGES
        .iter()
        .find(|(ending, _)| *ending == &last[point..]);
    known.map(|(_, language)| *language)
}

/// Whether `value` holds a forbidden key at any depth.
fn forbidden(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields
            .iter()
            .any(|(key, item)| FORBIDDEN.contains(&key.as_str()) || forbidden(item)),
        Value::Array(items) => items.iter().any(forbidden),
        _ => false,
    }
}

/// The names of the rules of the pack at `path`. A pack holds between one
/// and a hundred rules, each named once under this project's prefix, each
/// an error, each for languages that are scanned, and none with a
/// forbidden feature.
pub fn rules(path: &Path) -> Result<BTreeSet<String>> {
    let content = files::bounded(path, MAX_CONFIG, false)?;
    let document = record::strict(&content).map_err(|_| FAILED)?;
    let fields = document.as_object().ok_or(FAILED)?;
    require(
        fields.len() == 1 && fields.contains_key("rules"),
        "semgrep_rules_schema",
    )?;
    let entries = fields["rules"].as_array().ok_or(FAILED)?;
    require(
        !entries.is_empty() && entries.len() <= MAX_RULES,
        "semgrep_rule_count",
    )?;
    let mut identifiers = BTreeSet::new();
    for entry in entries {
        let rule = entry.as_object().ok_or(FAILED)?;
        let identifier = rule.get("id").and_then(Value::as_str).ok_or(FAILED)?;
        let lettered =
            |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        let named = identifier
            .strip_prefix("simplestchat.")
            .is_some_and(|name| {
                let begun = name.starts_with(|first: char| first.is_ascii_lowercase());
                begun && (2..=81).contains(&name.len()) && name.bytes().all(lettered)
            });
        require(named, "semgrep_rule_id")?;
        require(!identifiers.contains(identifier), "semgrep_duplicate_rule")?;
        let severity = rule.get("severity").and_then(Value::as_str);
        require(severity == Some("ERROR"), "semgrep_rule_severity")?;
        let languages = rule.get("languages").and_then(Value::as_array);
        let languages = languages.ok_or(FAILED)?;
        let scanned = |language: &Value| {
            LANGUAGES
                .iter()
                .any(|(_, known)| language.as_str() == Some(known))
        };
        require(
            !languages.is_empty() && languages.iter().all(scanned),
            "semgrep_rule_language",
        )?;
        require(!forbidden(entry), "semgrep_forbidden_rule_feature")?;
        identifiers.insert(identifier.to_owned());
    }
    Ok(identifiers)
}

/// One match: the rule, the path below the scanned root, and the line.
/// Nothing of what matched, and nothing else the engine says, is kept.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    rule: String,
    path: String,
    line: u64,
}

impl Finding {
    fn json(&self) -> Value {
        json!({"rule": self.rule, "path": self.path, "line": self.line})
    }
}

/// The findings as the evidence lists them, in their order.
fn shown(findings: &BTreeSet<Finding>) -> Value {
    findings.iter().map(Finding::json).collect()
}

/// The files `paths` as the engine is given them: each named once, by a
/// safe path below the root, with the language of its ending.
fn targets(paths: &[String]) -> Result<Value> {
    let distinct: BTreeSet<&String> = paths.iter().collect();
    require(
        !paths.is_empty() && paths.len() <= MAX_FILES && distinct.len() == paths.len(),
        "semgrep_target_count",
    )?;
    let mut listed = Vec::new();
    for path in paths {
        let language = language(path).filter(|_| safe_name(path));
        let language = language.ok_or(Fault("semgrep_target_path"))?;
        listed.push(json!(["CodeTarget", {
            "path": {"fpath": path, "ppath": format!("/{path}")},
            "analyzer": language,
            "products": ["sast"],
        }]));
    }
    Ok(json!(["Targets", listed]))
}

/// Whether a value counts as true where the reference asked for its truth:
/// anything but nothing, false, nought and what is empty.
fn truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(held) => *held,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// The field `name` of a part of the engine's report.
fn field<'a>(record: &'a Value, name: &str) -> Result<&'a Value> {
    record
        .as_object()
        .and_then(|record| record.get(name))
        .ok_or(FAILED)
}

/// A part of the engine's report as a list.
fn list(value: &Value) -> Result<&Vec<Value>> {
    value.as_array().ok_or(FAILED)
}

/// What the engine found, when its report is complete: its version is the
/// locked one, it had no error, skipped no rule, timed out nowhere, ran
/// every rule of `identifiers` itself and scanned exactly `paths`. A match
/// names a known rule, a given path and a line, and none is ignored inline.
fn findings(
    data: &[u8],
    paths: &[String],
    identifiers: &BTreeSet<String>,
    version: &str,
) -> Result<BTreeSet<Finding>> {
    let report = record::strict(data).map_err(|_| FAILED)?;
    require(
        field(&report, "version")?.as_str() == Some(version),
        "semgrep_engine_version",
    )?;
    require(
        list(field(&report, "errors")?)?.is_empty(),
        "semgrep_engine_errors",
    )?;
    require(
        list(field(&report, "skipped_rules")?)?.is_empty(),
        "semgrep_skipped_rules",
    )?;
    let timeouts = field(field(&report, "time")?, "fixpoint_timeouts")?;
    require(list(timeouts)?.is_empty(), "semgrep_fixpoint_timeout")?;
    let engines = list(field(&report, "rules_by_engine")?)?;
    let mut ran = BTreeSet::new();
    if engines.len() == identifiers.len() {
        for engine in engines {
            let pair = list(engine)?;
            // A list or a record cannot be told apart from another by its hash.
            let plain = pair
                .iter()
                .all(|part| !part.is_array() && !part.is_object());
            require(plain, FAILED.0)?;
            if let [Value::String(identifier), Value::String(by)] = pair.as_slice() {
                ran.extend((by == "OSS").then_some(identifier));
            }
        }
    }
    require(
        ran.len() == identifiers.len() && ran.iter().all(|rule| identifiers.contains(*rule)),
        "semgrep_incomplete_rules",
    )?;
    let scanned = list(field(field(&report, "paths")?, "scanned")?)?;
    let mut seen = BTreeSet::new();
    if scanned.len() == paths.len() {
        for path in scanned {
            seen.insert(path.as_str().ok_or(FAILED)?);
        }
    }
    let given: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    require(
        scanned.len() == paths.len() && seen == given,
        "semgrep_incomplete_scan",
    )?;
    let mut found = BTreeSet::new();
    for result in list(field(&report, "results")?)? {
        let rule = field(result, "check_id")?.as_str().ok_or(FAILED)?;
        let path = field(result, "path")?.as_str().ok_or(FAILED)?;
        require(
            identifiers.contains(rule) && given.contains(path),
            "semgrep_unknown_finding",
        )?;
        let line = field(field(result, "start")?, "line")?.as_u64();
        let line = line.filter(|line| (1..=MAX_LINE).contains(line));
        let line = line.ok_or(Fault("semgrep_finding_line"))?;
        let extra = field(result, "extra")?.as_object().ok_or(FAILED)?;
        require(
            !extra.get("is_ignored").is_some_and(truth),
            "semgrep_inline_ignore",
        )?;
        found.insert(Finding {
            rule: rule.to_owned(),
            path: path.to_owned(),
            line,
        });
    }
    Ok(found)
}

/// The names of the files and of the links to files below `directory`,
/// each from `root`, in the order of their parts. A directory that is a
/// link is not entered.
fn listed(root: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, directory: &Path, names: &mut Vec<String>) -> Result<()> {
        let entries = std::fs::read_dir(directory).map_err(|error| files::unusable(&error))?;
        for entry in entries {
            let path = entry.map_err(|error| files::unusable(&error))?.path();
            let kind = path.symlink_metadata();
            if kind.map_err(|error| files::unusable(&error))?.is_dir() {
                walk(root, &path, names)?;
            } else if path.is_file() {
                let below = path.strip_prefix(root).unwrap_or(&path);
                names.push(below.to_string_lossy().into_owned());
            }
        }
        Ok(())
    }
    let mut names = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut names)?;
    }
    names.sort_by(|left, right| left.split('/').cmp(right.split('/')));
    Ok(names)
}

/// What an annotation says: whether the rule must match the next line, and
/// the rule. An annotation is a line of its own: a comment sign, `ruleid:`
/// or `ok:`, and one word.
fn annotation(line: &str) -> Option<(bool, &str)> {
    let rest = line.trim_start_matches(space);
    let rest = rest.strip_prefix('#').or_else(|| rest.strip_prefix("//"))?;
    let rest = rest.trim_start_matches(space);
    let positive = rest.strip_prefix("ruleid:").map(|rest| (true, rest));
    let negative = rest.strip_prefix("ok:").map(|rest| (false, rest));
    let (kind, rest) = positive.or(negative)?;
    let word = rest.trim_matches(space);
    (!word.is_empty() && !word.contains(space)).then_some((kind, word))
}

/// Where the fixtures below `root` say each rule must match, and where it
/// must not: the line after each annotation. Every rule of `identifiers`
/// has both, and nothing that looks like an annotation is left unread.
fn expectations(
    root: &Path,
    identifiers: &BTreeSet<String>,
) -> Result<(BTreeSet<Finding>, BTreeSet<Finding>)> {
    let (mut positive, mut negative) = (BTreeSet::new(), BTreeSet::new());
    for name in listed(root)? {
        require(
            safe_name(&name) && language(&name).is_some(),
            "semgrep_fixture_path",
        )?;
        let content = files::bounded(&root.join(&name), MAX_CONFIG, false)?;
        let content = String::from_utf8(content).map_err(|_| FAILED)?;
        let lines = splitlines(&content);
        for (index, line) in lines.iter().enumerate() {
            let Some((expected, rule)) = annotation(line) else {
                require(
                    !line.contains("ruleid:") && !line.contains("ok:"),
                    "semgrep_fixture_annotation",
                )?;
                continue;
            };
            require(identifiers.contains(rule), "semgrep_fixture_unknown_rule")?;
            let target = lines.get(index + 1);
            let target = target
                .filter(|next| !next.trim_matches(space).is_empty() && annotation(next).is_none());
            require(target.is_some(), "semgrep_fixture_missing_target")?;
            let finding = Finding {
                rule: rule.to_owned(),
                path: name.clone(),
                line: index as u64 + 2,
            };
            if expected {
                positive.insert(finding);
            } else {
                negative.insert(finding);
            }
        }
    }
    let covered = |found: &BTreeSet<Finding>| {
        let rules: BTreeSet<&String> = found.iter().map(|finding| &finding.rule).collect();
        rules.len() == identifiers.len()
    };
    require(covered(&positive), "semgrep_positive_coverage")?;
    require(covered(&negative), "semgrep_negative_coverage")?;
    Ok((positive, negative))
}

/// The maintained sources of the snapshot that are scanned, in the order
/// of their names: the scanned languages below the maintained directories,
/// and below the vendored tree only when that is asked for.
fn scope(snapshot: &Path, include_vendor: bool) -> Result<Vec<String>> {
    let maintained = |name: &String| {
        let placed = PREFIXES.iter().any(|prefix| name.starts_with(prefix));
        language(name).is_some() && (placed || (include_vendor && vendor_path(name)))
    };
    let mut paths: Vec<String> = listed(snapshot)?.into_iter().filter(maintained).collect();
    require(!paths.is_empty(), "semgrep_empty_source_scope")?;
    paths.sort_unstable();
    Ok(paths)
}

/// Make the new, private directory `output` for the evidence. It is not
/// there yet, and it is not among the sources at `root`, unless below their
/// build directory or their results, which are ignored.
pub fn new_output(root: &Path, output: &Path) -> Result<PathBuf> {
    require(output.symlink_metadata().is_err(), "semgrep_output_exists")?;
    let parent = output.parent().map(Path::canonicalize);
    let parent = parent.and_then(std::result::Result::ok).ok_or(FAILED)?;
    let output = parent.join(output.file_name().ok_or(FAILED)?);
    let ignored = ["target", "results"]
        .iter()
        .any(|name| output.starts_with(root.join(name)));
    require(
        !output.starts_with(root) || ignored,
        "semgrep_output_in_source",
    )?;
    let made = std::fs::DirBuilder::new().mode(0o700).create(&output);
    made.map_err(|_| FAILED)?;
    Ok(output)
}

/// The engine and what the lock says of it.
pub struct Engine<'a> {
    /// The verified executable.
    pub path: &'a Path,
    /// The locked version, which the engine's report must name.
    pub version: &'a str,
    /// The lock's digest, read anew whenever it is asked for.
    pub lock: &'a dyn Fn() -> Result<String>,
}

/// Run the engine as the check `name` over `paths` below `root`, with the
/// rules at `pack`, and answer what it found. It has the time every command
/// has, its memory and its workers are bounded, and the files are given to
/// it one by one.
fn scanned(
    context: &mut Context,
    engine: &Engine<'_>,
    pack: &Path,
    root: &Path,
    paths: &[String],
    name: &'static str,
) -> Outcome<BTreeSet<Finding>> {
    let given = context.output().join(format!("{name}-targets.json"));
    files::write_private(&given, pretty(&targets(paths)?).as_bytes(), 0o600)?;
    let arguments = [
        "-json_nodots",
        "-no_filter_irrelevant_rules",
        "-no_filter_irrelevant_patterns",
        "-j",
        "2",
        "-timeout",
        "5",
        "-timeout_threshold",
        "1",
        "-max_memory",
        "2048",
        "-max_match_per_file",
        "1000",
        "-strict",
    ];
    let mut argv = vec![
        engine.path.as_os_str(),
        OsStr::new("-rules"),
        pack.as_os_str(),
        OsStr::new("-targets"),
        given.as_os_str(),
    ];
    argv.extend(arguments.map(OsStr::new));
    let how = Run {
        directory: Some(root),
        ..Run::default()
    };
    let (_, report) = context.run(name, &argv, &how)?;
    (engine.lock)()?;
    Ok(findings(&report, paths, &rules(pack)?, engine.version)?)
}

/// Run the fixtures of the snapshot as sources, without their `.fixture`
/// ending, and hold the engine to their annotations: it matches every line
/// they say it must, no line they say it must not, and nothing else. The
/// answer is how many lines were annotated.
fn golden(context: &mut Context, engine: &Engine<'_>, snapshot: &Path) -> Outcome<usize> {
    let destination = context.output().join("golden");
    let private = |path: &Path| {
        let mut made = std::fs::DirBuilder::new();
        made.mode(0o700)
            .recursive(true)
            .create(path)
            .map_err(|_| FAILED)
    };
    private(&destination)?;
    let fixtures = snapshot.join(FIXTURES);
    for name in listed(&fixtures)? {
        let plain = name.strip_suffix(".fixture");
        let target = destination.join(plain.ok_or(Fault("semgrep_fixture_extension"))?);
        private(target.parent().ok_or(FAILED)?)?;
        let content = files::bounded(&fixtures.join(&name), MAX_CONFIG, false)?;
        files::write_private(&target, &content, 0o600)?;
    }
    let pack = snapshot.join(RULES);
    let (positive, negative) = expectations(&destination, &rules(&pack)?)?;
    let mut paths = listed(&destination)?;
    paths.sort_unstable();
    let actual = scanned(
        context,
        engine,
        &pack,
        &destination,
        &paths,
        "golden-engine",
    )?;
    let kept = json!({
        "expected": shown(&positive),
        "actual": shown(&actual),
        "negative": shown(&negative),
    });
    let place = context.output().join("golden.json");
    files::write_private(&place, pretty(&kept).as_bytes(), 0o600)?;
    require(
        actual.is_disjoint(&negative),
        "semgrep_golden_false_positive",
    )?;
    require(actual == positive, "semgrep_golden_mismatch")?;
    Ok(positive.len() + negative.len())
}

/// Scan the snapshot that `context` takes of its sources: the golden
/// fixtures first, then the maintained sources with the same engine and
/// rules. The answer is the report, which is also kept as `report.json`.
/// The lock must not change while the scan runs.
pub fn check(
    context: &mut Context,
    engine: &Engine<'_>,
    git: &Path,
    include_vendor: bool,
) -> Outcome<Value> {
    let locked = (engine.lock)()?;
    let snapshot = context.snapshot(Snapshot::Source, git)?;
    let assertions = golden(context, engine, &snapshot)?;
    let paths = scope(&snapshot, include_vendor)?;
    let pack = snapshot.join(RULES);
    let actual = scanned(context, engine, &pack, &snapshot, &paths, "source-engine")?;
    require((engine.lock)()? == locked, "semgrep_tool_lock_changed")?;
    let manifest = context.output().join("source-manifest.json");
    let manifest = files::bounded(&manifest, crate::secrets::MANIFEST, false)?;
    let report = json!({
        "schemaVersion": 1,
        "includeVendor": include_vendor,
        "toolLockSha256": locked,
        "engineVersion": engine.version,
        "rulesSha256": hexadecimal(&Sha256::digest(files::bounded(&pack, MAX_CONFIG, false)?)),
        "sourceManifestSha256": hexadecimal(&Sha256::digest(&manifest)),
        "goldenAssertions": assertions,
        "sourceFiles": paths.len(),
        "findings": shown(&actual),
        "passed": actual.is_empty(),
    });
    let place = context.output().join("report.json");
    files::write_private(&place, pretty(&report).as_bytes(), 0o600)?;
    Ok(report)
}

/// A value on one line as the reference prints one: keys in their order,
/// a space after each comma and each colon, ASCII only.
fn flat(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(flat).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(fields) => {
            let field =
                |(name, value): (&String, &Value)| format!("{}: {}", quoted(name), flat(value));
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_unstable_by_key(|(name, _)| *name);
            let fields: Vec<String> = fields.into_iter().map(field).collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::String(text) => quoted(text),
        other => other.to_string(),
    }
}

/// The one line and the status with which a check ends: the report, and 0
/// when it passed; or the failure's code, which is the general one for an
/// error of the system, whose details are never printed.
pub fn answered(outcome: &Outcome<Value>) -> (String, u8) {
    // The reference names the failure after saying that the check failed.
    let failed = |code: &str| {
        let line = format!("{{\"passed\": false, \"error\": {}}}\n", quoted(code));
        (line, 1)
    };
    match outcome {
        Ok(report) => {
            let passed = report.get("passed") == Some(&Value::Bool(true));
            (flat(report) + "\n", u8::from(!passed))
        }
        Err(Failure::Fixed("tool_entry_missing" | "tool_entry_unavailable")) => failed(FAILED.0),
        Err(failure) => failed(&failure.to_string()),
    }
}

/// The gate's step: scan as the check `semgrep`, with the scan's evidence
/// in a directory of its own below the gate's, and record what the scan may
/// say in public as `semgrep-diagnosis`. Only a scan whose own line says
/// that it passed is a pass, and the diagnosis takes no such line from a
/// scan that did not end with 0.
pub fn gate(
    context: &mut Context,
    engine: &Engine<'_>,
    git: &Path,
    include_vendor: bool,
) -> Outcome<()> {
    let (status, line) = context.told("semgrep", |outer| {
        let output = new_output(outer.root(), &outer.output().join("semgrep"));
        let scanned = output.map_err(Failure::from).and_then(|output| {
            let mut nested = outer.nested(&output)?;
            check(&mut nested, engine, git, include_vendor)
        });
        answered(&scanned)
    })?;
    let directory = context.output().join("semgrep");
    let said = summary::diagnosis(context.root(), status, &line, &directory);
    let passed = said.first() == Some(&("passed", Value::Bool(true)));
    let mut note: Note = vec![
        ("name", "semgrep-diagnosis".into()),
        ("exitStatus", status.into()),
    ];
    note.extend(said);
    context.note(note);
    Ok(require(passed, "semgrep_source_check_failed")?)
}

#[cfg(test)]
mod summary_tests;
#[cfg(test)]
mod tests;
