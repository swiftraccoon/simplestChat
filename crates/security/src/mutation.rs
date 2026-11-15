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
//!
//! The user's sources are never changed. The run works in a private copy
//! with the pinned compiler, the static native libraries and no network,
//! its disk use is watched while the tool runs, and the copy is removed
//! when the run is over, whatever happened.

use crate::{
    Fault, Result,
    context::{Context, Limits, Outcome, Run, Snapshot},
    digest,
    fast::Programs,
    files,
    record::{self, pretty},
    require,
};
use serde_json::{Map, Value, json};
use std::{
    ffi::{OsStr, OsString},
    fs::{DirBuilder, Permissions},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
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

/// The bounds of a run's disk use.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// The most bytes the private copy and what is built in it may hold.
    pub workspace: u64,
    /// The most bytes the tool may leave as results.
    pub evidence: u64,
    /// The most entries either may hold.
    pub entries: u64,
    /// The fewest bytes that must stay free where the copy is.
    pub reserve: u64,
}

impl Bounds {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        workspace: 20 * 1024 * 1024 * 1024,
        evidence: 512 * 1024 * 1024,
        entries: 200_000,
        reserve: 1024 * 1024 * 1024,
    };
}

/// How often the disk is looked at while the tool runs.
const LOOK: Duration = Duration::from_secs(1);

/// The watch over what the tool writes: the private copy with what is
/// built in it, and the tool's own results. Sizes are counted as the file
/// system says them, without reading a byte.
struct Budget<'a> {
    source: &'a Path,
    output: &'a Path,
    bounds: Bounds,
    looked: Option<Instant>,
    failure: Option<String>,
    observed: Map<String, Value>,
}

impl<'a> Budget<'a> {
    fn new(source: &'a Path, output: &'a Path, bounds: Bounds) -> Self {
        Self {
            source,
            output,
            bounds,
            looked: None,
            failure: None,
            observed: Map::new(),
        }
    }

    /// Whether the tree below `root` is within `limit` bytes and the bound
    /// of entries. Directories are entered as they are met, links are not
    /// followed, and what is counted is kept up to the entry that is one
    /// too many.
    fn within(&mut self, root: &Path, label: &str, limit: u64) -> bool {
        let (mut entries, mut bytes) = (0_u64, 0_u64);
        let mut waiting = vec![root.to_path_buf()];
        while let Some(directory) = waiting.pop() {
            let Ok(listed) = std::fs::read_dir(&directory) else {
                continue;
            };
            let listed: Vec<PathBuf> = listed.flatten().map(|entry| entry.path()).collect();
            entries += listed.len() as u64;
            self.observed
                .insert(format!("{label}Entries"), entries.into());
            if entries > self.bounds.entries {
                self.failure = Some(format!("{label}_entry_limit"));
                return false;
            }
            for path in listed {
                // A link to a directory is an entry and is not entered.
                if path.is_dir() {
                    let linked = path
                        .symlink_metadata()
                        .is_ok_and(|entry| entry.is_symlink());
                    waiting.extend((!linked).then_some(path));
                    continue;
                }
                // What the compiler removes meanwhile has no size.
                let Ok(entry) = path.symlink_metadata() else {
                    continue;
                };
                bytes += entry.size();
                self.observed.insert(format!("{label}Bytes"), bytes.into());
                if bytes > limit {
                    self.failure = Some(format!("{label}_byte_limit"));
                    return false;
                }
            }
        }
        true
    }

    /// Whether the run is still within its bounds. While the tool runs the
    /// disk is looked at once a second at most, and between looks the last
    /// verdict stands; `forced`, it is looked at now. A run that was once
    /// out of bounds stays so.
    fn healthy(&mut self, forced: bool) -> Result<bool> {
        if self.failure.is_some() {
            return Ok(false);
        }
        if !forced && self.looked.is_some_and(|looked| looked.elapsed() < LOOK) {
            return Ok(true);
        }
        self.looked = Some(Instant::now());
        let (source, output, bounds) = (self.source, self.output, self.bounds);
        if !self.within(source, "workspace", bounds.workspace)
            || !self.within(output, "evidence", bounds.evidence)
        {
            return Ok(false);
        }
        let disk = rustix::fs::statvfs(source);
        let disk = disk.map_err(|error| files::unusable(&error.into()))?;
        let available = disk.f_bavail.saturating_mul(disk.f_frsize);
        self.observed
            .insert("availableBytes".to_owned(), available.into());
        if available < bounds.reserve {
            self.failure = Some("free_disk_reserve".to_owned());
        }
        Ok(self.failure.is_none())
    }

    /// What the watch found, as the run keeps it.
    fn kept(&self) -> Value {
        let mut kept = self.observed.clone();
        kept.insert("passed".to_owned(), self.failure.is_none().into());
        kept.insert("failure".to_owned(), self.failure.clone().into());
        kept.into()
    }
}

/// The variables that the builds of the private copy `source` run with:
/// the pinned compiler that `rustup` names for the copy's toolchain file
/// before anything else on the search path, a target directory inside the
/// copy, no network, two jobs, and the static OpenSSL at `openssl` and the
/// static AWS-LC, which is where the caller's environment says or at its
/// usual place below the sources.
fn environment(
    context: &mut Context,
    programs: &dyn Programs,
    source: &Path,
    openssl: &Path,
    ambient: &dyn Fn(&str) -> Option<OsString>,
) -> Outcome<Vec<(&'static str, OsString)>> {
    let toolchain = Fault("mutation_rust_toolchain");
    let pinned = files::bounded(&source.join("rust-toolchain.toml"), MAX_REPORT, false)?;
    let pinned = std::str::from_utf8(&pinned).map_err(|_| toolchain)?;
    let pinned: Value = toml::from_str(pinned).map_err(|_| toolchain)?;
    let channel = pinned["toolchain"]["channel"].as_str().ok_or(toolchain)?;
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    require(
        channel.split('.').count() == 3 && channel.split('.').all(number),
        toolchain.0,
    )?;
    let rustup = programs.prerequisite("rustup")?;
    let arguments = ["which", "--toolchain", channel, "cargo"].map(OsStr::new);
    let mut argv = vec![rustup.as_os_str()];
    argv.extend(arguments);
    let (_, found) = context.run("rust-toolchain", &argv, &Run::default())?;
    let found = std::str::from_utf8(&found).map_err(|_| toolchain)?;
    let found = found.trim_matches(crate::policy::space);
    require(!found.is_empty(), toolchain.0)?;
    let real = |path: &Path| path.canonicalize().map_err(|error| files::unusable(&error));
    let cargo = real(Path::new(found))?;
    let prefix = real(openssl)?;
    let held = |directory: &Path, names: [&str; 2]| {
        names.iter().all(|name| directory.join(name).is_file())
    };
    require(
        held(&prefix, ["lib/libssl.a", "lib/libcrypto.a"]),
        "mutation_static_openssl_missing",
    )?;
    let usual = context.root().join("target/aws-lc-5.11.0");
    let aws = ambient("AWS_LC_SYS_SYSTEM_DIR").map_or(usual, PathBuf::from);
    let aws = real(&aws)?;
    require(
        held(
            &aws,
            ["lib/libcrypto-awslc.a", "share/rust/aws_lc_bindings.rs"],
        ),
        "mutation_static_aws_lc_missing",
    )?;
    let mut search = cargo.parent().unwrap_or(&cargo).as_os_str().to_owned();
    search.push(":");
    search.push(context.variable("PATH").unwrap_or_default());
    Ok(vec![
        ("PATH", search),
        ("RUSTC", cargo.with_file_name("rustc").into()),
        ("RUSTDOC", cargo.with_file_name("rustdoc").into()),
        ("CARGO_NET_OFFLINE", "true".into()),
        ("CARGO_BUILD_JOBS", "2".into()),
        ("CARGO_PROFILE_TEST_DEBUG", "0".into()),
        ("MEDIASOUP_BUILD_JOBS", "2".into()),
        ("CARGO_TARGET_DIR", source.join("target").into()),
        ("CARGO_TERM_COLOR", "never".into()),
        ("OPENSSL_DIR", prefix.clone().into()),
        ("OPENSSL_STATIC", "1".into()),
        ("AWS_LC_SYS_SYSTEM_DIR", aws.into()),
        ("AWS_LC_SYS_STATIC", "1".into()),
        ("AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK", "0".into()),
        ("PKG_CONFIG_PATH", prefix.join("lib/pkgconfig").into()),
        (
            "PIP_CONSTRAINT",
            source.join("build/pip-constraints.txt").into(),
        ),
    ])
}

/// What one run is given.
pub struct Mutation<'a> {
    /// Where the sources are.
    pub root: &'a Path,
    /// The new directory of the run's evidence.
    pub output: &'a Path,
    /// The platform the tool is for.
    pub platform: &'a str,
    /// Whether this host is a Mac; otherwise it is Linux.
    pub darwin: bool,
    /// The static OpenSSL.
    pub openssl: &'a Path,
    /// The verified mutation tool, asked for when the run needs it.
    pub tool: &'a dyn Fn() -> Outcome<PathBuf>,
    /// Where the host's own programs are.
    pub programs: &'a dyn Programs,
    /// The host's git.
    pub git: &'a Path,
    /// The caller's environment.
    pub ambient: &'a dyn Fn(&str) -> Option<OsString>,
    /// The bounds of the run's disk use.
    pub bounds: Bounds,
}

/// The lowercase SHA-256 of the bounded file at `path`.
fn identity(path: &Path) -> Result<String> {
    Ok(digest(&files::bounded(path, MAX_REPORT, false)?))
}

/// Keep `value` as the private evidence file `name`.
fn keep(context: &Context, name: &str, value: &Value) -> Result<()> {
    files::write_private(
        &context.output().join(name),
        pretty(value).as_bytes(),
        0o600,
    )
}

/// Run the tool `argv` in the private copy `source` for at most `seconds`,
/// with the watch over its disk use, and answer its status. What the watch
/// found is kept whether or not the tool ended by itself.
fn executed(
    context: &Context,
    argv: &[OsString],
    source: &Path,
    variables: &[(&str, &OsStr)],
    seconds: u64,
    bounds: Bounds,
) -> Outcome<i32> {
    let results = context.output().join("results");
    let mut budget = Budget::new(source, &results, bounds);
    let mut unseen = None;
    let how = Run {
        directory: Some(source),
        within: Duration::from_secs(seconds),
        variables,
        ..Run::default()
    };
    let status = {
        // A watch that cannot look is as good as a bound that is passed.
        let mut healthy = || match budget.healthy(false) {
            Ok(healthy) => healthy,
            Err(fault) => {
                unseen = Some(fault);
                false
            }
        };
        context.watch("mutation", argv, &how, &mut healthy)
    };
    budget.healthy(true)?;
    keep(context, "budget.json", &budget.kept())?;
    if let Some(fault) = unseen {
        return Err(fault.into());
    }
    let status = status?;
    require(budget.failure.is_none(), "mutation_disk_budget")?;
    Ok(status)
}

/// The run after its evidence directory is made: what is learned goes into
/// `result` as it is learned, so that a failure keeps what came before it.
fn attempted(
    options: &Mutation<'_>,
    context: &mut Context,
    policy: &Policy,
    result: &mut Map<String, Value>,
) -> Outcome<bool> {
    let started = Instant::now();
    let family = if options.darwin { "darwin-" } else { "linux-" };
    let supported = ["linux-x86_64", "darwin-x86_64"].contains(&options.platform);
    require(
        supported && options.platform.starts_with(family),
        "mutation_tool_platform",
    )?;
    let tool = (options.tool)()?;
    let identities = [
        ("toolPlatform", options.platform.to_owned()),
        ("toolSha256", identity(&tool)?),
        (
            "toolLockSha256",
            identity(&options.root.join("build/security-tools.lock.json"))?,
        ),
    ];
    result.extend(identities.map(|(name, value)| (name.to_owned(), value.into())));
    let source = context.snapshot(Snapshot::Source, options.git)?;
    let manifest = context.output().join("source-manifest.json");
    result.insert(
        "sourceManifestSha256".to_owned(),
        identity(&manifest)?.into(),
    );
    // Build scripts are trusted sources: what may be run in the sources may
    // be run in the copy.
    let listed = record::strict(&files::bounded(&manifest, MAX_REPORT, false)?)?;
    for name in listed.as_object().map(Map::keys).into_iter().flatten() {
        let original = options.root.join(name).metadata();
        let original = original.map_err(|error| files::unusable(&error))?;
        if original.mode() & 0o111 != 0 {
            let run = std::fs::set_permissions(source.join(name), Permissions::from_mode(0o700));
            run.map_err(|error| files::unusable(&error))?;
        }
    }
    let variables = environment(
        context,
        options.programs,
        &source,
        options.openssl,
        options.ambient,
    )?;
    let variables: Vec<(&str, &OsStr)> = variables
        .iter()
        .map(|(name, value)| (*name, value.as_os_str()))
        .collect();
    let results = context.output().join("results");
    let how = Run {
        directory: Some(&source),
        within: Duration::from_secs(120),
        variables: &variables,
        ..Run::default()
    };
    let listing = command(&tool, &source, &results, policy, true);
    let (_, listing) = context.run("mutation-inventory", &listing, &how)?;
    let expected = inventory(&listing, policy)?;
    let listed: Vec<Value> = expected.iter().map(|(_, mutant)| mutant.clone()).collect();
    keep(context, "inventory.json", &listed.into())?;
    result.insert("enumeratedMutants".to_owned(), expected.len().into());
    let remaining = policy
        .total_seconds
        .checked_sub(started.elapsed().as_secs());
    let remaining = remaining
        .filter(|left| *left > 0)
        .ok_or(Fault("mutation_deadline"))?;
    let running = command(&tool, &source, &results, policy, false);
    let status = executed(
        context,
        &running,
        &source,
        &variables,
        remaining,
        options.bounds,
    )?;
    let summary = assess(&results.join("mutants.out"), &expected, policy)?;
    keep(context, "summary.json", &summary)?;
    let passed = status == 0 && summary["passed"] == true;
    let elapsed = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    result.insert("summary".to_owned(), summary);
    result.insert("exitStatus".to_owned(), status.into());
    result.insert("elapsedSeconds".to_owned(), elapsed.into());
    Ok(passed)
}

/// Run the mutation check and answer whether it passed. A policy in
/// another form and an evidence directory that cannot be made new are
/// refused before anything is kept. From there on every ending is kept as
/// `outcome.json`, with the code of what failed, and the private copy of
/// the sources is removed.
pub fn run(options: &Mutation<'_>) -> Outcome<bool> {
    let place = options.root.join(POLICY);
    let policy = policy_at(&place)?;
    let made = DirBuilder::new().mode(0o700).create(options.output);
    made.map_err(|error| files::unusable(&error))?;
    let output = options.output.canonicalize();
    let output = output.map_err(|error| files::unusable(&error))?;
    let mut context = Context::new(options.root, &output, Limits::REVIEWED, options.ambient)?;
    let mut result = Map::new();
    result.insert("policySha256".to_owned(), identity(&place)?.into());
    let mut passed = match attempted(options, &mut context, &policy, &mut result) {
        Ok(passed) => passed,
        Err(failure) => {
            result.insert("error".to_owned(), failure.to_string().into());
            false
        }
    };
    let source = output.join("source");
    if source.symlink_metadata().is_ok() && std::fs::remove_dir_all(&source).is_err() {
        passed = false;
        result.insert("error".to_owned(), "mutation_snapshot_cleanup".into());
    }
    result.insert("passed".to_owned(), passed.into());
    keep(&context, "outcome.json", &result.into())?;
    Ok(passed)
}

/// The one line and the status with which the command ends: whether the
/// check passed, and where its evidence is. A run that could not begin has
/// failed.
pub fn finished(outcome: &Outcome<bool>, output: &Path) -> (String, u8) {
    let passed = matches!(outcome, Ok(true));
    let verdict = if passed { "passed" } else { "failed" };
    let line = format!(
        "Mutation policy checks {verdict}; evidence: {}\n",
        output.display()
    );
    (line, u8::from(!passed))
}

#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod tests;
