//! `simplestchat-security codeql local --revision SHA --output DIR ...` and
//! `codeql upload-ready --output DIR --revision SHA --language LANGUAGE`:
//! the reference's command line, the run of every selected language against
//! the clean commit and its private copy, and `summary.json` with one line
//! on standard output; and the categories whose fresh reports may be
//! uploaded, one `<category>=true` line each.

use super::{
    Host, Options,
    analysis::analyze_language,
    analysis::checked,
    reason,
    reports::{complete_health, upload_ready},
};
use crate::{
    Fault, Result,
    arguments::{Read, read},
    budget::detect,
    codeql::{
        tools::{self, LANGUAGES, STORED_LANGUAGES},
        triage::{decode, object, raised_by, write_private},
    },
    commit,
    context::{self, Context, Failure, Limits, Outcome, Run, Snapshot},
    document::Document,
    paths,
    policy::space,
    require,
    scope::{automated, require_local_vendor},
};
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs::{self, DirBuilder},
    io::Write,
    os::unix::fs::{DirBuilderExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::ExitCode,
};

/// How `codeql local` is used.
pub const USAGE: &str = concat!(
    "usage: simplestchat-security codeql local --revision SHA --output DIR",
    " [--codeql PATH] [--language LANGUAGE] [--suite security|all]",
    " [--openssl-prefix PATH] [--database-cache DIR] [--rust-cargo-cache DIR]",
    " [--include-vendor] [--cache-tool PATH]",
);
/// How `codeql upload-ready` is used.
pub const UPLOAD_USAGE: &str = concat!(
    "usage: simplestchat-security codeql upload-ready --output DIR --revision SHA",
    " --language LANGUAGE [--category CATEGORY ...]",
);
/// The options of `codeql local`, in the order the reference's parser was
/// told them, and the cache tool's last.
const OPTIONS: [&str; 10] = [
    "--codeql",
    "--revision",
    "--output",
    "--language",
    "--suite",
    "--openssl-prefix",
    "--database-cache",
    "--rust-cargo-cache",
    "--include-vendor",
    "--cache-tool",
];
/// The option of `codeql local` that takes no value.
const FLAG: &str = "--include-vendor";
/// The suites an analysis may be asked for.
const SUITES: [&str; 2] = ["security", "all"];
/// The options of `codeql upload-ready`.
const UPLOAD_OPTIONS: [&str; 4] = ["--output", "--revision", "--language", "--category"];

/// What a command line comes to, as the reference's parser read one.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<T> {
    /// What it asked.
    Asked(T),
    /// The usage, asked for: the reference's parser then ended with 0.
    Help,
    /// No command line of the command's: it then ended with 2.
    Refused,
}

/// How each argument of `arguments` was read where the options are
/// `options`: after `--`, every argument is a word.
fn reads<'a>(arguments: &'a [String], options: &[&'static str]) -> Vec<Read<'a>> {
    let mut ended = false;
    let mut found = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let read = if ended {
            Read::Word
        } else {
            read(argument, options)
        };
        ended |= read == Read::End;
        found.push(read);
    }
    found
}

/// The values of the command line `arguments` of the options `options`, as
/// the reference's parser read them: an option given more than once counts
/// as it was given last unless `repeated` lets it gather every value, a
/// value comes after its option or its `=` and must be one that `valid`
/// admits, `flag` takes none, nothing else is admitted, and the help wins
/// wherever it is asked for before anything wrong.
fn values<'a>(
    arguments: &'a [String],
    options: &[&'static str],
    flag: Option<&str>,
    repeated: Option<&str>,
    valid: &dyn Fn(&str, &str) -> bool,
) -> Line<Vec<(&'static str, &'a str)>> {
    let reads = reads(arguments, options);
    let mut values: Vec<(&'static str, &'a str)> = Vec::new();
    let mut stray = false;
    let mut index = 0;
    while let Some(read) = reads.get(index).copied() {
        index += 1;
        let (option, value) = match read {
            Read::Word | Read::End | Read::Unknown => {
                stray = true;
                continue;
            }
            Read::Help(true) => return Line::Help,
            Read::Help(false) | Read::Ambiguous => return Line::Refused,
            Read::Named(option, given) if Some(option) == flag => {
                if given.is_some() {
                    return Line::Refused;
                }
                (option, "")
            }
            Read::Named(option, Some(value)) => (option, value),
            Read::Named(option, None) => {
                if reads.get(index) != Some(&Read::Word) {
                    return Line::Refused;
                }
                index += 1;
                (option, arguments[index - 1].as_str())
            }
        };
        if !valid(option, value) {
            return Line::Refused;
        }
        if Some(option) != repeated {
            values.retain(|(held, _)| *held != option);
        }
        values.push((option, value));
    }
    if stray {
        Line::Refused
    } else {
        Line::Asked(values)
    }
}

/// The value `values` give `option`.
fn value<'a>(values: &[(&'static str, &'a str)], option: &str) -> Option<&'a str> {
    let found = values.iter().find(|(held, _)| *held == option);
    found.map(|(_, value)| *value)
}

/// The path `values` give `option`, spelled as the reference spelled one.
fn spelled(values: &[(&'static str, &str)], option: &str) -> Option<PathBuf> {
    value(values, option).map(|given| paths::path(OsStr::new(given)))
}

/// The command line `arguments` of `codeql local` against the checkout
/// `root`, as the reference's parser read it: a revision and an output, a
/// language and a suite among their choices, the static OpenSSL below the
/// checkout unless another is named, and the cache tool, which the
/// reference did not need.
pub fn parsed(root: &Path, arguments: &[String]) -> Line<Options> {
    let valid = |option: &str, value: &str| match option {
        "--language" => LANGUAGES.contains(&value),
        "--suite" => SUITES.contains(&value),
        _ => true,
    };
    let values = match values(arguments, &OPTIONS, Some(FLAG), None, &valid) {
        Line::Asked(values) => values,
        Line::Help => return Line::Help,
        Line::Refused => return Line::Refused,
    };
    let (Some(revision), Some(output)) = (value(&values, "--revision"), value(&values, "--output"))
    else {
        return Line::Refused;
    };
    let prefix = || paths::path(root.join("target/openssl-4.1.0").as_os_str());
    Line::Asked(Options {
        codeql: spelled(&values, "--codeql"),
        revision: revision.to_owned(),
        output: paths::path(OsStr::new(output)),
        language: value(&values, "--language").map(str::to_owned),
        suite: value(&values, "--suite").unwrap_or("security").to_owned(),
        openssl_prefix: spelled(&values, "--openssl-prefix").unwrap_or_else(prefix),
        database_cache: spelled(&values, "--database-cache"),
        rust_cargo_cache: spelled(&values, "--rust-cargo-cache"),
        include_vendor: value(&values, FLAG).is_some(),
        cache_tool: spelled(&values, "--cache-tool"),
    })
}

/// The languages a run of `language`, or of every one, covers: every
/// supported one in a vendor scan, otherwise the `first_party` ones.
fn languages(language: Option<&str>, include_vendor: bool, first_party: &[&str]) -> Vec<String> {
    if let Some(language) = language.filter(|language| !language.is_empty()) {
        return vec![language.to_owned()];
    }
    let every: &[&str] = if include_vendor {
        &LANGUAGES
    } else {
        first_party
    };
    every
        .iter()
        .map(|language| (*language).to_owned())
        .collect()
}

/// The languages a run analyzes, once a vendor scan is proved local where
/// `automated` says automation runs, and the native language is asked for
/// only in a vendor scan; a first-party run of every language covers the
/// `first_party` ones.
pub fn selected_languages(
    language: Option<&str>,
    include_vendor: bool,
    automated: bool,
    first_party: &[&str],
) -> Result<Vec<String>> {
    require_local_vendor(include_vendor, automated)?;
    require(
        language != Some("c-cpp") || include_vendor,
        "codeql_vendor_opt_in_required",
    )?;
    Ok(languages(language, include_vendor, first_party))
}

/// Whether `byte` is white space as the reference's bytes strip it.
fn blank(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Bind the run to the complete clean commit `revision`, which need not be
/// pushed: the checkout's head is that commit, and nothing is uncommitted.
pub fn source_identity(context: &mut Context, host: &Host<'_>, revision: &str) -> Outcome<()> {
    require(commit(revision), "codeql_revision")?;
    let git = context::executable("git", host.search)?;
    let argv = [git.as_os_str(), "rev-parse".as_ref(), "HEAD".as_ref()];
    let (_, head) = checked(context, host, "codeql-source-head", &argv, &Run::default())?;
    let head = String::from_utf8(head).map_err(|_| Fault("UnicodeDecodeError"))?;
    require(head.trim_matches(space) == revision, "codeql_source_head")?;
    let git = context::executable("git", host.search)?;
    let argv = [
        git.as_os_str(),
        "status".as_ref(),
        "--porcelain=v1".as_ref(),
        "--untracked-files=normal".as_ref(),
    ];
    let (_, status) = checked(
        context,
        host,
        "codeql-source-status",
        &argv,
        &Run::default(),
    )?;
    require(
        status.iter().copied().all(blank),
        "codeql_source_not_committed",
    )?;
    Ok(())
}

/// The private copy of the sources, without what is ignored, whose files
/// that the checkout lets anyone execute are executable by the caller.
pub fn snapshot(context: &mut Context, host: &Host<'_>) -> Outcome<PathBuf> {
    let git = context::executable("git", host.search)?;
    let source = context.snapshot(Snapshot::Source, &git)?;
    let written = fs::read(context.output().join("source-manifest.json"));
    let manifest = decode(&written.map_err(|error| raised_by(&error))?)?;
    for (name, _) in object(&manifest)?.fields().unwrap_or_default() {
        let checked = fs::metadata(context.root().join(name));
        let mode = checked
            .map_err(|error| raised_by(&error))?
            .permissions()
            .mode();
        if mode & 0o111 != 0 {
            let private = fs::Permissions::from_mode(0o700);
            let set = fs::set_permissions(source.join(name), private);
            set.map_err(|error| raised_by(&error))?;
        }
    }
    Ok(source)
}

/// The reports of every selected language, each analyzed from the same
/// private copy of the clean commit, once the commit and the copy are
/// proved unchanged at the end and every language and suite passed.
pub fn execute(
    context: &mut Context,
    host: &Host<'_>,
    options: &Options,
) -> Outcome<Vec<Document>> {
    let present = |name: &str| (host.variable)(name).is_some();
    let languages = selected_languages(
        options.language.as_deref(),
        options.include_vendor,
        automated(present),
        host.first_party,
    )?;
    source_identity(context, host, &options.revision)?;
    let source = snapshot(context, host)?;
    let databases = context.output().join("databases");
    let made = DirBuilder::new().mode(0o700).create(databases);
    made.map_err(|error| raised_by(&error))?;
    let codeql = match &options.codeql {
        Some(codeql) => codeql.clone(),
        None => {
            let installer = tools::Host {
                bundle: host.bundle,
                search: host.search,
            };
            tools::install(context, &installer)?
        }
    };
    let languages: Vec<&str> = languages.iter().map(String::as_str).collect();
    tools::verify(context, &codeql, &languages)?;
    let mut reports = Vec::new();
    for language in &languages {
        let analyzed = analyze_language(context, host, options, &source, &codeql, language)?;
        reports.extend(analyzed);
    }
    source_identity(context, host, &options.revision)?;
    let git = context::executable("git", host.search)?;
    context.snapshot(Snapshot::Final, &git)?;
    let output = context.output();
    let first = fs::read(output.join("source-manifest.json"));
    let last = fs::read(output.join("final-source-manifest.json"));
    let first = first.map_err(|error| raised_by(&error))?;
    let last = last.map_err(|error| raised_by(&error))?;
    require(first == last, "codeql_source_changed")?;
    complete_health(&reports, &languages, &options.suite)?;
    Ok(reports)
}

/// A value of a check's summary line as a document.
fn noted(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(held) => Document::Bool(*held),
        Value::Number(number) => Document::Number(number.clone()),
        Value::String(text) => Document::Text(text.clone()),
        Value::Array(items) => Document::List(items.iter().map(noted).collect()),
        Value::Object(fields) => {
            let fields = fields
                .iter()
                .map(|(name, value)| (name.clone(), noted(value)));
            Document::Record(fields.collect())
        }
    }
}

/// The checks `context` recorded, each with its fields in their order.
fn checks(context: &Context) -> Document {
    let lines = context.checks().iter().map(|note| {
        let fields = note
            .iter()
            .map(|(name, value)| ((*name).to_owned(), noted(value)));
        Document::Record(fields.collect())
    });
    Document::List(lines.collect())
}

/// Write `summary.json` of the run of `options` against the checkout
/// `root`: whether it passed, the reason it failed, what it covered (the
/// first-party languages of `host` when it named none), the
/// identities of the pin and of the copy of the sources, every report and
/// every check.
fn summarize(
    root: &Path,
    options: &Options,
    host: &Host<'_>,
    context: &Context,
    failure: Option<String>,
    reports: Vec<Document>,
) -> Outcome<()> {
    let pin = fs::read(root.join(tools::PIN)).map_err(|error| raised_by(&error))?;
    let manifest = options.output.join("source-manifest.json");
    let manifest = if manifest.is_file() {
        let read = fs::read(&manifest).map_err(|error| raised_by(&error))?;
        Some(crate::digest(&read))
    } else {
        None
    };
    let excluded = if options.include_vendor {
        Vec::new()
    } else {
        vec!["vendor/**".into()]
    };
    let covered = languages(
        options.language.as_deref(),
        options.include_vendor,
        host.first_party,
    );
    let summary = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("passed", failure.is_none().into()),
        ("failure", failure.into()),
        ("revision", options.revision.as_str().into()),
        ("fullScan", true.into()),
        ("includeVendor", options.include_vendor.into()),
        ("excludedPaths", Document::List(excluded)),
        ("suite", options.suite.as_str().into()),
        ("toolchainSha256", crate::digest(&pin).into()),
        ("sourceManifestSha256", manifest.into()),
        (
            "languages",
            Document::List(covered.into_iter().map(Document::from).collect()),
        ),
        ("reports", Document::List(reports)),
        ("checks", checks(context)),
    ]);
    let path = options.output.join("summary.json");
    Ok(write_private(&path, summary.pretty().as_bytes())?)
}

/// The invocation of `options` against the checkout `root`, in the new
/// private directory of its evidence: an output that is not absolute, and a
/// database cache without its tool, are refused before anything is made.
fn begun(root: &Path, options: &Options, host: &Host<'_>) -> Outcome<Context> {
    require(options.output.is_absolute(), "codeql_output_absolute")?;
    options.cached()?;
    let made = DirBuilder::new().mode(0o700).create(&options.output);
    made.map_err(|error| raised_by(&error))?;
    Context::new(root, &options.output, Limits::REVIEWED, host.variable)
}

/// The exit status where the reference's interpreter left with what it
/// raised: `reason` named on `err`, and no line on standard output.
fn stopped(err: &mut dyn Write, failure: &Failure) -> u8 {
    let _ = writeln!(err, "Local CodeQL stopped: {}", reason(failure));
    1
}

/// The exit status of a run of `options` against the checkout `root`:
/// 0 when every selected language and suite passed, and 1 otherwise, with
/// the summary written and one line on `out` naming its directory. What the
/// reference's run did not catch leaves no summary and one line on `err`.
pub fn main(
    root: &Path,
    options: &Options,
    host: &Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    (host.private)();
    let mut context = match begun(root, options, host) {
        Ok(context) => context,
        Err(failure) => return stopped(err, &failure),
    };
    let (failure, reports) = match execute(&mut context, host, options) {
        Ok(reports) => (None, reports),
        // The reference's run caught every class it could raise but this.
        Err(failure) if reason(&failure) == "TypeError" => return stopped(err, &failure),
        Err(failure) => (Some(reason(&failure)), Vec::new()),
    };
    let passed = failure.is_none();
    if let Err(failure) = summarize(root, options, host, &context, failure, reports) {
        return stopped(err, &failure);
    }
    let status = if passed { "passed" } else { "failed" };
    let line = format!(
        "Local CodeQL: {status}; evidence: {}",
        options.output.display()
    );
    if writeln!(out, "{line}").is_err() {
        return 1;
    }
    u8::from(!passed)
}

/// The exit status of the command line `arguments` of `codeql local`
/// against the checkout `root`.
pub fn command(
    root: &Path,
    arguments: &[String],
    host: &Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match parsed(root, arguments) {
        Line::Asked(options) => main(root, &options, host, out, err),
        Line::Help => {
            let _ = writeln!(out, "{USAGE}");
            0
        }
        Line::Refused => {
            let _ = writeln!(err, "{USAGE}");
            2
        }
    }
}

/// Run the command line `arguments` of `codeql local` against the checkout
/// `root`, with this program's own triage and vendor verification, this
/// host's budget, and the caller's environment and search path.
pub fn run(root: &Path, arguments: &[String]) -> ExitCode {
    let variable = |name: &str| std::env::var_os(name);
    let search = variable("PATH");
    let name = rustix::system::uname();
    let system = name.sysname().to_string_lossy().into_owned();
    let machine = name.machine().to_string_lossy().into_owned();
    let bundle = tools::host_bundle();
    // A program that cannot be named cannot be run: its checks then fail.
    let program = std::env::current_exe().unwrap_or_default();
    let private = || {
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    };
    let host = Host {
        variable: &variable,
        search: search.as_deref(),
        system: &system,
        machine: &machine,
        bundle: &bundle,
        linux: cfg!(target_os = "linux"),
        program: &program,
        budget: &detect,
        private: &private,
        witness: &|_, _, _| {},
        first_party: &STORED_LANGUAGES,
    };
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    ExitCode::from(command(root, arguments, &host, &mut out, &mut err))
}

/// What `codeql upload-ready` asks: the evidence of a local run, its exact
/// commit, the language, and the categories in the order they are asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// The evidence directory of the local run.
    pub output: PathBuf,
    /// The exact commit the reports must be of.
    pub revision: String,
    /// The language of the reports.
    pub language: String,
    /// The categories, each as often as it was asked.
    pub categories: Vec<String>,
}

/// The command line `arguments` of `codeql upload-ready`: an output, a
/// revision, a language, and the categories, both suites in their order
/// when none is named.
pub fn upload_parsed(arguments: &[String]) -> Line<Upload> {
    let valid =
        |option: &str, value: &str| option != "--category" || super::CATEGORIES.contains(&value);
    let values = match values(arguments, &UPLOAD_OPTIONS, None, Some("--category"), &valid) {
        Line::Asked(values) => values,
        Line::Help => return Line::Help,
        Line::Refused => return Line::Refused,
    };
    let required = ["--output", "--revision", "--language"].map(|option| value(&values, option));
    let [Some(output), Some(revision), Some(language)] = required else {
        return Line::Refused;
    };
    let mut categories: Vec<String> = values
        .iter()
        .filter(|(option, _)| *option == "--category")
        .map(|(_, category)| (*category).to_owned())
        .collect();
    if categories.is_empty() {
        categories = super::CATEGORIES.map(str::to_owned).to_vec();
    }
    Line::Asked(Upload {
        output: paths::path(OsStr::new(output)),
        revision: revision.to_owned(),
        language: language.to_owned(),
        categories,
    })
}

/// The exit status of `codeql upload-ready` for `asked` against the
/// checkout `root`: a `<category>=true` line on `out` for each category
/// whose report may be uploaded, nothing for one that may not, and a
/// refusal on `err` that ends the run with 1 after the lines before it.
pub fn upload(root: &Path, asked: &Upload, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    for category in &asked.categories {
        let ready = upload_ready(
            root,
            &asked.output,
            &asked.revision,
            category,
            &asked.language,
        );
        match ready {
            Ok(true) => {
                if writeln!(out, "{category}=true").is_err() {
                    return 1;
                }
            }
            Ok(false) => {}
            Err(failure) => {
                let _ = writeln!(err, "CodeQL upload readiness failed: {}", reason(&failure));
                return 1;
            }
        }
    }
    0
}

/// The exit status of the command line `arguments` of `codeql
/// upload-ready` against the checkout `root`.
pub fn upload_command(
    root: &Path,
    arguments: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match upload_parsed(arguments) {
        Line::Asked(asked) => upload(root, &asked, out, err),
        Line::Help => {
            let _ = writeln!(out, "{UPLOAD_USAGE}");
            0
        }
        Line::Refused => {
            let _ = writeln!(err, "{UPLOAD_USAGE}");
            2
        }
    }
}

/// Run the command line `arguments` of `codeql upload-ready` against the
/// checkout `root`.
pub fn upload_run(root: &Path, arguments: &[String]) -> ExitCode {
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    ExitCode::from(upload_command(root, arguments, &mut out, &mut err))
}

#[cfg(test)]
#[path = "local_command_tests.rs"]
mod tests;
