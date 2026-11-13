//! The checks of the fast tier.
//!
//! Each runs pinned scanners through the invocation's bounded commands and
//! holds what they report to the reviewed policy. A scanner that exits with
//! success has only written a report: every report is read, and one of
//! another shape fails the check.

use crate::{
    Fault, Result,
    context::{Context, Limits, Outcome, Run, executable},
    files,
    findings::{self, field, list, object},
    policy::Exception,
    record, require, secrets,
    tools::{self, Site},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha512};
use std::{
    ffi::{OsStr, OsString},
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// What the reference derives its inert canary from.
const CANARY_SEED: &[u8] = b"inert source scanner coverage only";
/// The scanner's status when it found something and was asked to say so.
const FOUND: i32 = 10;
/// How long an audit that reads an advisory database or an index may take.
const AUDIT: Duration = Duration::from_secs(300);
/// The requirement files whose pinned Python packages are audited, each
/// under the name of its check.
const REQUIREMENTS: [(&str, &str); 5] = [
    ("pip-audit-0", "build/python-requirements.txt"),
    ("pip-audit-1", "ops/ansible/requirements.txt"),
    ("pip-audit-2", "security/requirements.txt"),
    (
        "pip-audit-3",
        "vendor/mediasoup-sys-0.19.0/python-invoke-requirements.txt",
    ),
    (
        "pip-audit-4",
        "vendor/mediasoup-sys-0.19.0/python-tools-requirements.txt",
    ),
];
/// The directories whose npm lock files are audited.
const PACKAGES: [(&str, &str); 2] = [("npm-audit-0", "web"), ("npm-audit-1", "web/e2e")];

/// Where the programs of the checks are.
pub trait Programs {
    /// The path of the pinned scanner `name`, verified before it is given.
    fn program(&self, name: &'static str) -> Outcome<PathBuf>;

    /// The path of the host's own `name`, looked for when it is asked for.
    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf>;

    /// The interpreter of the hash-locked scanner environment.
    fn python(&self) -> Outcome<PathBuf>;
}

/// The scanners of one installation, each verified against its receipt and
/// the lock whenever it is asked for, beside the host's own programs.
#[derive(Clone, Copy, Debug)]
pub struct Installed<'a> {
    /// Where the lock is and what the installation is for.
    pub site: Site<'a>,
    /// The installation.
    pub directory: &'a Path,
    /// The caller's search path.
    pub search: Option<&'a OsStr>,
    /// The scanner environment's interpreter.
    pub python: &'a Path,
}

impl Programs for Installed<'_> {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Ok(tools::path(&self.site, name, Some(self.directory))?)
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        executable(name, self.search)
    }

    fn python(&self) -> Outcome<PathBuf> {
        Ok(self.python.to_path_buf())
    }
}

/// A program and its arguments as one command.
fn command<'a>(program: &'a Path, arguments: &[&'a OsStr]) -> Vec<&'a OsStr> {
    let mut argv = vec![program.as_os_str()];
    argv.extend_from_slice(arguments);
    argv
}

/// Check the syntax of the workflows and hold every offline, pedantic finding
/// about them to a review. The auditor's statuses for findings are answers
/// only beside a report that has reviewed findings in it.
pub fn workflows(
    context: &mut Context,
    programs: &dyn Programs,
    reviews: &[Exception],
) -> Outcome<()> {
    let actionlint = programs.program("actionlint")?;
    context.run("actionlint", &[actionlint], &Run::default())?;
    let zizmor = programs.program("zizmor")?;
    let arguments = [
        "--offline",
        "--pedantic",
        "--no-ignores",
        "--no-config",
        "--strict-collection",
        "--collect=all",
        "--format=json-v1",
        ".github",
    ];
    let how = Run {
        accepted: &[0, 11, 12, 13, 14],
        ..Run::default()
    };
    let (status, report) = context.run(
        "zizmor",
        &command(&zizmor, &arguments.map(OsStr::new)),
        &how,
    )?;
    let reviewed = findings::zizmor(&report, reviews)?;
    require(
        status == 0 || reviewed > 0,
        "zizmor_unsuccessful_empty_report",
    )?;
    context.annotate([("reviewedFindings", reviewed.into())]);
    Ok(())
}

/// What the secret scan is given.
#[derive(Clone, Copy, Debug)]
pub struct Scan<'a> {
    /// The reviewed exceptions.
    pub reviews: &'a [Exception],
    /// The commit the pushed range starts after, when there is one.
    pub base: Option<&'a str>,
    /// The private copy of the sources.
    pub snapshot: &'a Path,
    /// Whether vendored sources are scanned as well.
    pub include_vendor: bool,
    /// The git that reads the repository.
    pub git: &'a Path,
}

/// The options of every scan: full redaction, no inline or file-based
/// suppression, a JSON report on standard output, and no size cut-off, since
/// the snapshot has already bounded every file.
fn options(configuration: &Path) -> [&OsStr; 12] {
    [
        OsStr::new("--config"),
        configuration.as_os_str(),
        OsStr::new("--redact=100"),
        OsStr::new("--ignore-gitleaks-allow"),
        OsStr::new("--gitleaks-ignore-path"),
        OsStr::new("/dev/null"),
        OsStr::new("--log-level=warn"),
        OsStr::new("--no-banner"),
        OsStr::new("--report-format=json"),
        OsStr::new("--report-path=-"),
        OsStr::new("--max-target-megabytes=0"),
        OsStr::new("--max-decode-depth=3"),
    ]
}

/// A credential-shaped value that was never issued: the first 36 symbols of
/// the base64 of a fixed SHA-512, without the two that are no letters or
/// digits. It exists only at run time, never in a file of the repository.
fn canary() -> String {
    const SYMBOLS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789xy";
    let digest = Sha512::digest(CANARY_SEED);
    let mut canary = String::from("ghp_");
    for [first, second, third] in digest[..27].as_chunks::<3>().0 {
        let bits = u32::from(*first) << 16 | u32::from(*second) << 8 | u32::from(*third);
        for shift in [18, 12, 6, 0] {
            canary.push(char::from(SYMBOLS[(bits >> shift & 63) as usize]));
        }
    }
    canary
}

/// Prove that the detector finds the canary, redacted, in projected binary
/// bytes, although a comment beside it asks to be let through.
fn selftest(context: &mut Context, scanner: &Path) -> Outcome<()> {
    let canary = canary();
    let source = context.output().join("secret-canary.original");
    let mut original = b"\x7fELF\x02\x01\x01\x00".to_vec();
    original.extend_from_slice(format!("TOKEN={canary} # gitleaks:allow\n").as_bytes());
    files::write_private(&source, &original, 0o600)?;
    let inputs = context.output().join("secret-canary-inputs");
    DirBuilder::new()
        .mode(0o700)
        .create(&inputs)
        .map_err(|error| files::unusable(&error))?;
    let target = inputs.join("content000000");
    let projection = secrets::project(&source, &target, 1024)?;
    let configuration = context.output().join("secret-canary.toml");
    files::write_private(&configuration, b"[extend]\nuseDefault = true\n", 0o600)?;
    let mut arguments = vec![OsStr::new("dir"), inputs.as_os_str()];
    arguments.extend(options(&configuration));
    arguments.push(OsStr::new("--exit-code=10"));
    let how = Run {
        accepted: &[FOUND],
        ..Run::default()
    };
    let (_, report) = context.run("gitleaks-selftest", &command(scanner, &arguments), &how)?;
    let document = record::document(&report)?;
    let entries = list(&document)?;
    require(entries.len() == 1, "secret_detector_selftest_count")?;
    let entry = object(&entries[0])?;
    let named = entry.get("RuleID") == Some(&json!("github-pat"))
        && entry.get("Secret") == Some(&json!("REDACTED"));
    let placed = named && {
        let file = record::string(field(entry, "File")?)?;
        let projected = std::fs::canonicalize(&target).map_err(|error| files::unusable(&error))?;
        std::fs::canonicalize(file).ok() == Some(projected)
    };
    let leaked = report
        .windows(canary.len())
        .any(|window| window == canary.as_bytes());
    require(placed && !leaked, "secret_detector_selftest_identity")?;
    context.annotate([
        ("expectedFinding", "github-pat".into()),
        ("sourceSha256", projection.source_sha256.into()),
        ("projectionSha256", projection.projection_sha256.into()),
    ]);
    Ok(())
}

/// Run one scan and require its explicit, empty list of findings.
fn clean(
    context: &mut Context,
    name: &'static str,
    argv: &[&OsStr],
    variables: &[(&str, &OsStr)],
) -> Outcome<()> {
    let how = Run {
        variables,
        ..Run::default()
    };
    let (_, report) = context.run(name, argv, &how)?;
    let document = record::document(&report)?;
    require(list(&document)?.is_empty(), "secret_findings_present")?;
    Ok(())
}

/// The variables under which the scanner's fixed diff command reads an index
/// of this invocation's own, the snapshot as its working tree, and never a
/// path outside the scan.
fn working(context: &mut Context, scan: &Scan<'_>) -> Outcome<[(&'static str, OsString); 3]> {
    let git = scan.git;
    let asked = ["rev-parse", "--absolute-git-dir"].map(OsStr::new);
    let (_, printed) = context.run(
        "secret-diff-git-dir",
        &command(git, &asked),
        &Run::default(),
    )?;
    let printed = std::str::from_utf8(&printed).map_err(|_| Fault("secret_diff_git_directory"))?;
    let directory =
        std::fs::canonicalize(printed.trim()).map_err(|error| files::unusable(&error))?;
    require(directory.is_dir(), "secret_diff_git_directory")?;
    let variables = [
        ("GIT_DIR", directory.into_os_string()),
        ("GIT_WORK_TREE", scan.snapshot.into()),
        (
            "GIT_INDEX_FILE",
            context.output().join("secret-diff-index").into(),
        ),
    ];
    let given = variables
        .each_ref()
        .map(|(name, value)| (*name, value.as_os_str()));
    let how = Run {
        variables: &given,
        ..Run::default()
    };
    let read = ["read-tree", "HEAD"].map(OsStr::new);
    context.run("secret-diff-index", &command(git, &read), &how)?;
    if !scan.include_vendor {
        let listed = ["ls-files", "-z", "--", ":(top)vendor/"].map(OsStr::new);
        let (_, names) = context.run("secret-diff-vendor-paths", &command(git, &listed), &how)?;
        if !names.is_empty() {
            let removed = ["update-index", "--force-remove", "-z", "--stdin"].map(OsStr::new);
            let how = Run {
                input: &names,
                ..how
            };
            context.run("secret-diff-scope", &command(git, &removed), &how)?;
        }
    }
    Ok(variables)
}

/// Scan the projected snapshot, the pushed range and the working changes for
/// secrets with full redaction.
///
/// The configuration is held to its reviews first. The detector must then
/// find the canary before anything else is believed. The range is scanned
/// only when its base is an ancestor of the head: after a rewrite of the
/// branch there is no range of pushed commits, the tree scan has covered
/// every current byte, and the evidence names the skipped scan.
pub fn secret_scan(context: &mut Context, programs: &dyn Programs, scan: &Scan<'_>) -> Outcome<()> {
    let configuration = context.root().join("security/gitleaks.toml");
    findings::gitleaks_configuration(&configuration, scan.reviews, scan.include_vendor)?;
    let room = secrets::free(context.output())?;
    let neutral = secrets::neutral_snapshot(
        context.output(),
        scan.snapshot,
        &configuration,
        scan.include_vendor,
        context.limits(),
        room,
    )?;
    let scanner = programs.program("gitleaks")?;
    selftest(context, &scanner)?;
    let mut tree = vec![OsStr::new("dir"), neutral.inputs.as_os_str()];
    tree.extend(options(&neutral.configuration));
    clean(context, "gitleaks-tree", &command(&scanner, &tree), &[])?;
    context.annotate([
        ("sourceFiles", neutral.files.into()),
        ("sourceBytes", neutral.source_bytes.into()),
        ("projectionBytes", neutral.projection_bytes.into()),
        ("projectionCoverageSha256", neutral.coverage_sha256.into()),
    ]);
    if let Some(base) = scan.base {
        let hexadecimal = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        require(
            base.len() == 40 && base.bytes().all(hexadecimal),
            "invalid_security_diff_base",
        )?;
        let asked = ["merge-base", "--is-ancestor", base, "HEAD"].map(OsStr::new);
        // 1: the base is no ancestor; 128: this clone does not have it.
        let how = Run {
            accepted: &[0, 1, 128],
            ..Run::default()
        };
        let (status, _) = context.run("diff-base", &command(scan.git, &asked), &how)?;
        if status == 0 {
            let scope = if scan.include_vendor {
                ""
            } else {
                " -- . :(top,exclude)vendor/**"
            };
            let range = OsString::from(format!("--log-opts={base}..HEAD{scope}"));
            let mut pushed = vec![OsStr::new("git"), OsStr::new("."), range.as_os_str()];
            pushed.extend(options(&configuration));
            clean(context, "gitleaks-diff", &command(&scanner, &pushed), &[])?;
        } else {
            context.annotate([("rangeScan", "skipped_base_not_an_ancestor".into())]);
        }
    }
    let variables = working(context, scan)?;
    let given = variables
        .each_ref()
        .map(|(name, value)| (*name, value.as_os_str()));
    let mut changes = vec![
        OsStr::new("git"),
        scan.snapshot.as_os_str(),
        OsStr::new("--pre-commit"),
    ];
    changes.extend(options(&configuration));
    clean(
        context,
        "gitleaks-working-diff",
        &command(&scanner, &changes),
        &given,
    )
}

/// How many packages the Cargo lock at `path` names.
fn locked(path: &Path, limits: &Limits) -> Result<u64> {
    let content = files::bounded(path, limits.source, false)?;
    let text = std::str::from_utf8(&content).map_err(|_| Fault("cargo_lock_invalid"))?;
    let document: Value = toml::from_str(text).map_err(|_| Fault("cargo_lock_invalid"))?;
    Ok(list(field(object(&document)?, "package")?)?.len() as u64)
}

/// A report that is a record, as the audits of other ecosystems print one.
fn reported(report: &[u8]) -> Result<Value> {
    let document = record::document(report)?;
    require(document.is_object(), "scanner_report_object")?;
    Ok(document)
}

/// Audit every locked dependency against today's advisories and hold the
/// crates to the reviewed budget and policy.
///
/// The Rust audit reads the whole lock with a Cargo home of its own; each of
/// its advisories needs a review, and its status for a finding is an answer
/// only beside reviewed findings. cargo-deny holds bans, licenses and
/// sources; Cargo's own metadata is held to the budget of duplicated
/// packages. No pinned Python package and no npm package of either lock may
/// have an advisory at all. `now` is the time since the epoch.
pub fn dependencies(
    context: &mut Context,
    programs: &dyn Programs,
    reviews: &[Exception],
    now: Duration,
) -> Outcome<()> {
    let auditor = programs.program("cargo-audit")?;
    let lock = context.root().join("Cargo.lock");
    let database = context.root().join(".cache/security-rustsec-db");
    let (home, own) = (
        context.output().join("home"),
        context.output().join("cargo-audit-home"),
    );
    let arguments = [
        OsStr::new("audit"),
        OsStr::new("--json"),
        OsStr::new("--file"),
        lock.as_os_str(),
        OsStr::new("--db"),
        database.as_os_str(),
    ];
    let how = Run {
        directory: Some(&home),
        within: AUDIT,
        accepted: &[0, 1],
        variables: &[("CARGO_HOME", own.as_os_str())],
        ..Run::default()
    };
    let (status, report) = context.run("cargo-audit", &command(&auditor, &arguments), &how)?;
    let packages = locked(&lock, context.limits())?;
    let unreadable = |fault: Fault| match fault.0 {
        "invalid_tool_json" => Fault("cargo_audit_invalid_report"),
        _ => fault,
    };
    let reviewed = findings::cargo(&report, reviews, packages, now).map_err(unreadable)?;
    require(
        status == 0 || reviewed > 0,
        "cargo_unsuccessful_empty_report",
    )?;
    context.annotate([("reviewedFindings", reviewed.into())]);
    let slow = Run {
        within: AUDIT,
        ..Run::default()
    };
    let deny = programs.program("cargo-deny")?;
    let policy = ["--locked", "check", "bans", "licenses", "sources"].map(OsStr::new);
    context.run("cargo-deny", &command(&deny, &policy), &slow)?;
    let cargo = programs.prerequisite("cargo")?;
    let asked = [
        "metadata",
        "--locked",
        "--all-features",
        "--format-version=1",
    ]
    .map(OsStr::new);
    let (_, metadata) = context.run("cargo-metadata", &command(&cargo, &asked), &slow)?;
    let budget = context.root().join("security/dependency-budget.json");
    findings::dependency_budget(&metadata, &budget)?;
    for (name, requirements) in REQUIREMENTS {
        let python = programs.python()?;
        let audit = [
            "-m",
            "pip_audit",
            "--require-hashes",
            "--disable-pip",
            "-r",
            requirements,
            "--format=json",
        ];
        let (_, report) = context.run(name, &command(&python, &audit.map(OsStr::new)), &slow)?;
        let document = reported(&report)?;
        for dependency in list(field(object(&document)?, "dependencies")?)? {
            let advisories = list(field(object(dependency)?, "vulns")?)?;
            require(advisories.is_empty(), "python_dependency_advisory")?;
        }
    }
    for (name, package) in PACKAGES {
        let npm = programs.prerequisite("npm")?;
        let directory = context.root().join(package);
        let audit = ["audit", "--package-lock-only", "--ignore-scripts", "--json"].map(OsStr::new);
        let how = Run {
            directory: Some(&directory),
            ..slow
        };
        let (_, report) = context.run(name, &command(&npm, &audit), &how)?;
        let document = reported(&report)?;
        let counted = object(field(object(&document)?, "metadata")?)?;
        let counts = object(field(counted, "vulnerabilities")?)?;
        require(
            field(counts, "total")? == &json!(0),
            "npm_dependency_advisory",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
