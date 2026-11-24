//! `simplestchat-security codeql local` and `codeql upload-ready`: the
//! complete analysis of the exact committed sources with the pinned
//! analyzer, on a developer's own machine or in CI, that uploads nothing
//! and claims nothing of GitHub's ingestion; and whether a fresh report may
//! be uploaded although its findings failed the policy.
//!
//! Every language is extracted from a private copy of the commit, analyzed
//! anew or from a database that the cache tool restored and proved, and its
//! report held to the pin, to its own category and to the first-party
//! sources before the current reviews are enforced by this program's own
//! triage. The evidence is private: `summary.json`, a `.validated.json`
//! marker beside each report, and a `<language>-cache-ready.json` for a
//! database the cache tool saved.
//!
//! Where the reference's interpreter raised, the run stops with the name of
//! the class it raised, as the reference's evidence names it: a fixed code
//! is in lowercase words, and a raised class begins with a capital.

#[path = "local_analysis.rs"]
pub mod analysis;
#[path = "local_command.rs"]
pub mod command;
#[path = "local_reports.rs"]
pub mod reports;

pub use command::run;

use super::{tools::LANGUAGES, triage::classified};
use crate::{
    Fault,
    budget::Budget,
    context::{Failure, Outcome, Run},
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

/// The coverage counters of a Rust extraction that a report keeps, in the
/// order of their names.
pub const RUST_EXTRACTION_STATISTICS: [&str; 13] = [
    "Elements extracted",
    "Elements unextracted",
    "Extraction errors",
    "Extraction warnings",
    "Files extracted - total",
    "Files extracted - total user",
    "Files extracted - with errors",
    "Files extracted - without errors",
    "Lines of user code extracted",
    "Macro calls - resolved",
    "Macro calls - total",
    "Macro calls - unresolved",
    "Taint edges - number of edges",
];
/// The two suites, in the order they are analyzed.
pub const CATEGORIES: [&str; 2] = ["security", "quality-advisory"];
/// The checks that extract each automated language, in the order of
/// [`AUTOMATED_LANGUAGES`](super::tools::AUTOMATED_LANGUAGES).
const CREATE: [&str; 4] = [
    "codeql-create-actions",
    "codeql-create-javascript-typescript",
    "codeql-create-python",
    "codeql-create-rust",
];
/// The checks that analyze each language's suites, in the order of
/// [`LANGUAGES`] and of [`CATEGORIES`].
const ANALYZE: [[&str; 2]; 5] = [
    [
        "codeql-analyze-actions-security",
        "codeql-analyze-actions-quality-advisory",
    ],
    [
        "codeql-analyze-javascript-typescript-security",
        "codeql-analyze-javascript-typescript-quality-advisory",
    ],
    [
        "codeql-analyze-python-security",
        "codeql-analyze-python-quality-advisory",
    ],
    [
        "codeql-analyze-rust-security",
        "codeql-analyze-rust-quality-advisory",
    ],
    [
        "codeql-analyze-c-cpp-security",
        "codeql-analyze-c-cpp-quality-advisory",
    ],
];
/// The checks that enforce the reviews on each language's reports, in the
/// same order.
const POLICY: [[&str; 2]; 5] = [
    [
        "codeql-policy-actions-security",
        "codeql-policy-actions-quality-advisory",
    ],
    [
        "codeql-policy-javascript-typescript-security",
        "codeql-policy-javascript-typescript-quality-advisory",
    ],
    [
        "codeql-policy-python-security",
        "codeql-policy-python-quality-advisory",
    ],
    [
        "codeql-policy-rust-security",
        "codeql-policy-rust-quality-advisory",
    ],
    [
        "codeql-policy-c-cpp-security",
        "codeql-policy-c-cpp-quality-advisory",
    ],
];
/// The languages whose analyzed databases are cached.
const CACHED: [&str; 2] = ["rust", "c-cpp"];
/// The checks of the cache tool for each cached language: the key, the
/// restoration, the proof of the sources and the publication.
const CACHE: [[&str; 4]; 2] = [
    [
        "codeql-cache-key-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-source-rust",
        "codeql-cache-save-rust",
    ],
    [
        "codeql-cache-key-c-cpp",
        "codeql-cache-restore-c-cpp",
        "codeql-cache-source-c-cpp",
        "codeql-cache-save-c-cpp",
    ],
];
/// What a check of a name there is none of is refused as.
const UNNAMED: Failure = Failure::Fixed("invalid_scanner_check_name");

/// The check of `table` for `name` among `names`.
fn named<T: Copy>(names: &[&str], table: &[T], name: &str) -> Outcome<T> {
    let at = names.iter().position(|held| *held == name);
    at.and_then(|at| table.get(at).copied()).ok_or(UNNAMED)
}

/// The check that extracts the automated language `language`.
fn create_check(language: &str) -> Outcome<&'static str> {
    named(&super::tools::AUTOMATED_LANGUAGES, &CREATE, language)
}

/// The check that analyzes `language`'s suite `category`.
fn analyze_check(language: &str, category: &str) -> Outcome<&'static str> {
    named(
        &CATEGORIES,
        &named(&LANGUAGES, &ANALYZE, language)?,
        category,
    )
}

/// The check that enforces the reviews on `language`'s suite `category`.
fn policy_check(language: &str, category: &str) -> Outcome<&'static str> {
    named(
        &CATEGORIES,
        &named(&LANGUAGES, &POLICY, language)?,
        category,
    )
}

/// The suites an analysis of `suite` covers: both for `all`, and otherwise
/// the security suite alone.
pub fn categories(suite: &str) -> &'static [&'static str] {
    if suite == "all" {
        &CATEGORIES
    } else {
        &CATEGORIES[..1]
    }
}

/// What the evidence says of `failure`: a fixed code as it is, and a code
/// of the shared steps that stood in for a class the reference's
/// interpreter raised, as that class.
pub fn reason(failure: &Failure) -> String {
    match failure {
        Failure::Fixed("codeql_coverage_field") => "KeyError".to_owned(),
        Failure::Fixed("codeql_coverage_json") => "JSONDecodeError".to_owned(),
        Failure::Fixed(code) => classified(Fault(code)).0.to_owned(),
        other => other.to_string(),
    }
}

/// What a run asks for, as its command line said it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The installed CLI to use instead of the pinned bundle's.
    pub codeql: Option<PathBuf>,
    /// The exact commit that is analyzed.
    pub revision: String,
    /// The new private directory of the evidence.
    pub output: PathBuf,
    /// The one language to analyze instead of every one.
    pub language: Option<String>,
    /// `security`, or `all` for the advisory suite as well.
    pub suite: String,
    /// The static OpenSSL of a native build.
    pub openssl_prefix: PathBuf,
    /// The directory of the analyzed-database cache.
    pub database_cache: Option<PathBuf>,
    /// The directory of the Rust extractor's dependency build.
    pub rust_cargo_cache: Option<PathBuf>,
    /// Whether the vendored and native sources are analyzed too.
    pub include_vendor: bool,
    /// The program that keeps the analyzed-database cache.
    pub cache_tool: Option<PathBuf>,
}

impl Options {
    /// The analyzed-database cache and the program that keeps it, where a
    /// cache is asked for: a cache needs the absolute path of its program.
    pub fn cached(&self) -> Outcome<Option<(&Path, &Path)>> {
        let Some(directory) = &self.database_cache else {
            return Ok(None);
        };
        let tool = self.cache_tool.as_deref().filter(|tool| tool.is_absolute());
        let tool = tool.ok_or(Failure::Fixed("codeql_cache_tool_required"))?;
        Ok(Some((directory, tool)))
    }
}

/// What a run reaches beyond its checkout: the caller's environment and
/// search path, this host's system, machine and platform, this program,
/// the resource budget, the caller's mask, and whoever is told of every
/// command a check of the run's own runs.
#[derive(Clone, Copy)]
pub struct Host<'a> {
    /// The caller's environment variable `name`, where it is set.
    pub variable: &'a dyn Fn(&str) -> Option<OsString>,
    /// The caller's search path, which `git`, `curl`, `tar`, `rustc` and
    /// `cargo` are found on.
    pub search: Option<&'a OsStr>,
    /// The kernel's name for this system.
    pub system: &'a str,
    /// The kernel's name for this machine.
    pub machine: &'a str,
    /// The pin's name for this host's analyzer bundle.
    pub bundle: &'a str,
    /// Whether this host is Linux, the one platform of a native build.
    pub linux: bool,
    /// This program, whose `codeql triage` and `vendor verify` are run as
    /// checks.
    pub program: &'a Path,
    /// What a fresh build may use, with the compilers asked for.
    pub budget: &'a dyn Fn(Option<&str>) -> crate::Result<Budget>,
    /// Keep what the run writes from other accounts, as the reference did
    /// once its command line was read.
    pub private: &'a dyn Fn(),
    /// Told of each command a check of the run's own runs, before it runs.
    pub witness: &'a dyn Fn(&'static str, &[&OsStr], &Run<'_>),
    /// The languages a first-party run covers when it names none: those the
    /// checkout has sources of, which
    /// [`STORED_LANGUAGES`](super::tools::STORED_LANGUAGES) lists.
    pub first_party: &'a [&'a str],
}

#[cfg(test)]
#[path = "local_tests.rs"]
pub(crate) mod tests;
