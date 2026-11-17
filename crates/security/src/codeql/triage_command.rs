//! `simplestchat-security codeql triage MODE --output DIR --revision SHA`:
//! the reference's command line, its checks of what was asked before
//! anything is read, and its private evidence: `request.json`, then
//! `report.json` or `failure.json`, and an `action-NNN.json` before and
//! after each dismissal. A failure leaves only its fixed reason and one
//! fixed line on standard error; a SARIF or check verdict that does not
//! pass ends with 1.

use super::{
    Ledger, MAX_POLICY, MAX_REPORT, bounded_file, classified, decode, field,
    findings::verdict,
    github::{Bounded, Github, Reach, System},
    object,
    plan::{Checkout, analysis_health, api_plan, apply_plan},
    raised_by, same,
    sarif::{first_party_report, sarif_findings},
    stopped, write_private,
};
use crate::{
    Fault, Result,
    arguments::{Read, read},
    codeql::sources::{Git, NativeSources, Shown},
    commit,
    document::Document,
    paths,
    policy::{self, Date, Exception},
    require,
    scope::{automated, require_local_vendor},
    vendor::Bounds,
};
use std::{
    ffi::{OsStr, OsString},
    fs::DirBuilder,
    io::Write,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

/// How the command is used.
pub const USAGE: &str = concat!(
    "usage: simplestchat-security codeql triage health|sarif|plan|check|apply",
    " --output DIR --revision SHA [--input FILE] [--repository OWNER/NAME]",
    " [--ref REF] [--authorize-dismissals] [--source-cache DIR] [--include-vendor]",
);
/// The modes, the command's one word.
pub const MODES: [&str; 5] = ["health", "sarif", "plan", "check", "apply"];
/// The options that take none.
const FLAGS: [&str; 2] = ["--authorize-dismissals", "--include-vendor"];
/// Every option, in the order the reference's parser was told them.
const OPTIONS: [&str; 8] = [
    "--input",
    "--output",
    "--repository",
    "--revision",
    "--ref",
    "--authorize-dismissals",
    "--source-cache",
    "--include-vendor",
];
/// The line every failure ends with.
const FAILED: &str =
    "CodeQL evidence or review validation failed; no unreviewed alert was dismissed.";

/// What was asked: the mode, the report or plan read, where the evidence
/// goes, the repository and the exact revision and reference, the intent
/// to dismiss, and the native sources' cache of an explicit vendor scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The mode.
    pub mode: String,
    /// The SARIF report, or the plan to apply.
    pub input: Option<PathBuf>,
    /// The new private directory of the evidence.
    pub output: PathBuf,
    /// The repository, `owner/name`.
    pub repository: String,
    /// The exact commit.
    pub revision: String,
    /// The exact reference.
    pub reference: String,
    /// Whether dismissals are authorized.
    pub authorize_dismissals: bool,
    /// The native sources' private cache.
    pub source_cache: Option<PathBuf>,
    /// Whether the vendored sources are scanned too.
    pub include_vendor: bool,
}

/// What a command line comes to, as the reference's parser read one.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// What it asked.
    Asked(Options),
    /// The usage, asked for: the reference's parser then ended with 0.
    Help,
    /// No command line of the command's: it then ended with 2.
    Refused,
}

/// The mode of the positional arguments that begin at `at`, as the
/// reference's parser matched its one positional: one word, with the first
/// `--` before or right after it passed over. It answers the mode and where
/// the words it took end, or nothing where no word is there.
fn positional<'a>(
    arguments: &'a [String],
    reads: &[Read<'_>],
    at: usize,
) -> Option<(&'a str, usize)> {
    let mut index = at;
    if reads.get(index) == Some(&Read::End) {
        index += 1;
    }
    if reads.get(index) != Some(&Read::Word) {
        return None;
    }
    let word = arguments[index].as_str();
    index += 1;
    if reads.get(index) == Some(&Read::End) {
        index += 1;
    }
    Some((word, index))
}

/// The command line `arguments`, as the reference's parser read it: its
/// one word the mode, an option given more than once as it was given last,
/// a value after its option or its `=`, `--` before words only, and the
/// help wherever it is asked for before anything wrong.
pub fn parsed(arguments: &[String]) -> Line {
    let mut ended = false;
    let mut reads = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let read = if ended {
            Read::Word
        } else {
            read(argument, &OPTIONS)
        };
        ended |= read == Read::End;
        reads.push(read);
    }
    let mut mode = None;
    let mut values: Vec<(&'static str, &str)> = Vec::new();
    let mut flags: Vec<&'static str> = Vec::new();
    let mut stray = false;
    let mut index = 0;
    while let Some(read) = reads.get(index).copied() {
        match read {
            Read::Word | Read::End => {
                if mode.is_none()
                    && let Some((word, end)) = positional(arguments, &reads, index)
                {
                    if !MODES.contains(&word) {
                        return Line::Refused;
                    }
                    mode = Some(word);
                    index = end;
                    continue;
                }
                stray = true;
                index += 1;
            }
            Read::Unknown => {
                stray = true;
                index += 1;
            }
            Read::Help(true) => return Line::Help,
            Read::Help(false) | Read::Ambiguous => return Line::Refused,
            Read::Named(option, given) if FLAGS.contains(&option) => {
                if given.is_some() {
                    return Line::Refused;
                }
                flags.push(option);
                index += 1;
            }
            Read::Named(option, given) => {
                let value = match given {
                    Some(value) => value,
                    None => {
                        if reads.get(index + 1) != Some(&Read::Word) {
                            return Line::Refused;
                        }
                        index += 1;
                        arguments[index].as_str()
                    }
                };
                values.retain(|(held, _)| *held != option);
                values.push((option, value));
                index += 1;
            }
        }
    }
    let value = |option: &str| {
        let found = values.iter().find(|(held, _)| *held == option);
        found.map(|(_, value)| *value)
    };
    let spelled = |option: &str| value(option).map(|given| paths::path(OsStr::new(given)));
    let (Some(mode), Some(revision), Some(output), false) =
        (mode, value("--revision"), spelled("--output"), stray)
    else {
        return Line::Refused;
    };
    Line::Asked(Options {
        mode: mode.to_owned(),
        input: spelled("--input"),
        output,
        repository: value("--repository")
            .unwrap_or("swiftraccoon/simplestChat")
            .to_owned(),
        revision: revision.to_owned(),
        reference: value("--ref").unwrap_or("refs/heads/main").to_owned(),
        authorize_dismissals: flags.contains(&"--authorize-dismissals"),
        source_cache: spelled("--source-cache"),
        include_vendor: flags.contains(&"--include-vendor"),
    })
}

/// Refuse a vendor scan that is not local, or not of a SARIF report, a
/// source cache without one, a revision that is not exact, and an intent to
/// dismiss in any mode but apply or none in apply. `automated` says that
/// automation runs this.
pub fn validate_request(options: &Options, automated: bool) -> Result<()> {
    require_local_vendor(options.include_vendor, automated)?;
    require(
        !options.include_vendor || options.mode == "sarif",
        "codeql_vendor_local_sarif_only",
    )?;
    require(
        options.source_cache.is_none() || options.include_vendor,
        "codeql_vendor_opt_in_required",
    )?;
    require(commit(&options.revision), "codeql_revision")?;
    require(
        options.authorize_dismissals == (options.mode == "apply"),
        "codeql_apply_authorization",
    )
}

/// What the command reaches through: GitHub, its clock and the caller's
/// environment, what was committed, and the day reviews are held to.
#[derive(Clone, Copy)]
pub struct Host<'a> {
    /// GitHub, the clock and the caller's environment.
    pub reach: Reach<'a>,
    /// What was committed.
    pub git: &'a dyn Shown,
    /// The day reviews are held to.
    pub today: Date,
}

/// The reviews of the policy of `ledger` valid on `today`, refused by the
/// class the reference's interpreter raised where it raised one.
fn exceptions(ledger: &Ledger, today: Date, include_vendor: bool) -> Result<Vec<Exception>> {
    let content = bounded_file(&ledger.policy, MAX_POLICY)?;
    policy::read(&ledger.policy, today, include_vendor).map_err(|fault| {
        if fault.0 != "invalid_tool_json" {
            classified(fault)
        } else if std::str::from_utf8(&content).is_err() {
            Fault("UnicodeDecodeError")
        } else {
            Fault("JSONDecodeError")
        }
    })
}

/// The exit status of a run of `options` against the checkout `root` that
/// has made its evidence directory once `created` says so.
fn triaged(root: &Path, options: &Options, host: &Host<'_>, created: &mut bool) -> Result<u8> {
    let environment = host.reach.environment;
    let present = |name: &str| environment.iter().any(|(held, _)| held == name);
    validate_request(options, automated(present))?;
    DirBuilder::new()
        .mode(0o700)
        .create(&options.output)
        .map_err(|error| raised_by(&error))?;
    *created = true;
    let request = Document::record([
        ("mode", options.mode.as_str().into()),
        ("repository", options.repository.as_str().into()),
        ("revision", options.revision.as_str().into()),
        ("ref", options.reference.as_str().into()),
    ]);
    let evidence = |name: &str| options.output.join(name);
    let request = format!("{}\n", request.spaced());
    write_private(&evidence("request.json"), request.as_bytes())?;
    let report =
        |value: &Document| write_private(&evidence("report.json"), value.pretty().as_bytes());
    let (revision, reference) = (options.revision.as_str(), options.reference.as_str());
    if options.mode == "health" {
        require(options.input.is_none(), "codeql_unexpected_input")?;
        require(
            options.source_cache.is_none(),
            "codeql_unexpected_source_cache",
        )?;
        let mut client = Github::new(&options.repository, false, host.reach)?;
        report(&analysis_health(&mut client, revision, reference)?)?;
        return Ok(0);
    }
    let ledger = Ledger::of(root);
    let reviews = exceptions(&ledger, host.today, options.include_vendor)?;
    let mut native = match &options.source_cache {
        Some(cache) => {
            let made = NativeSources::new(root, revision, cache, host.git, Bounds::REVIEWED);
            Some(made.map_err(stopped)?)
        }
        None => None,
    };
    let mut checkout = Checkout {
        root,
        sources: native.as_mut(),
        git: host.git,
    };
    let output = if options.mode == "sarif" || options.mode == "apply" {
        let input = options.input.as_ref();
        let input = input.ok_or(Fault("codeql_input_required"))?;
        let raw = decode(&bounded_file(input, MAX_REPORT)?)?;
        let raw = object(&raw)?;
        if options.mode == "sarif" {
            if !options.include_vendor {
                first_party_report(raw)?;
            }
            let sources = checkout.sources.as_deref_mut();
            let clock = host.reach.clock;
            let findings = sarif_findings(raw, root, revision, sources, host.git, clock)?;
            verdict(&findings, &reviews)?
        } else {
            require(
                same(field(raw, "revision")?, revision) && same(field(raw, "ref")?, reference),
                "codeql_apply_selection",
            )?;
            let mut client = Github::new(&options.repository, true, host.reach)?;
            let mut number = 0;
            let mut journal = |value: &Document| {
                number += 1;
                let path = evidence(&format!("action-{number:03}.json"));
                write_private(&path, value.pretty().as_bytes())
            };
            apply_plan(
                &mut client,
                &mut checkout,
                raw,
                &reviews,
                &mut journal,
                &ledger,
            )?
        }
    } else {
        require(options.input.is_none(), "codeql_unexpected_input")?;
        let mut client = Github::new(&options.repository, false, host.reach)?;
        api_plan(
            &mut client,
            &mut checkout,
            revision,
            reference,
            &reviews,
            &ledger,
        )?
    };
    report(&output)?;
    let judged = options.mode == "sarif" || options.mode == "check";
    Ok(u8::from(
        judged && output.get("passed") != Some(&Document::Bool(true)),
    ))
}

/// The exit status of a run of `options` against the checkout `root`. A
/// failure leaves its reason in `failure.json` once the evidence directory
/// is made, and the fixed line on `err`.
pub fn main(root: &Path, options: &Options, host: &Host<'_>, err: &mut dyn Write) -> u8 {
    let mut created = false;
    let fault = match triaged(root, options, host, &mut created) {
        Ok(status) => return status,
        Err(fault) => fault,
    };
    if created {
        let failure = Document::record([("passed", false.into()), ("reason", fault.0.into())]);
        let failure = format!("{}\n", failure.spaced());
        let path = options.output.join("failure.json");
        // The reference's interpreter left with what it raised here, before
        // the line was written.
        if write_private(&path, failure.as_bytes()).is_err() {
            return 1;
        }
    }
    let _ = writeln!(err, "{FAILED}");
    1
}

/// The exit status of the command line `arguments` against the checkout
/// `root`.
pub fn command(
    root: &Path,
    arguments: &[String],
    host: &Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match parsed(arguments) {
        Line::Asked(options) => main(root, &options, host, err),
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

/// Run the command line `arguments` against the checkout `root`, through
/// the caller's environment, its `gh` and its `git`.
pub fn run(root: &Path, arguments: &[String]) -> ExitCode {
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let clock = System::new();
    let reach = Reach {
        clock: &clock,
        transport: &Bounded,
        environment: &environment,
    };
    let host = Host {
        reach,
        git: &Git,
        today: Date::today(),
    };
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    ExitCode::from(command(root, arguments, &host, &mut out, &mut err))
}

#[cfg(test)]
#[path = "triage_command_tests.rs"]
mod tests;
