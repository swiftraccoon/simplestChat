//! The tiers of the gate and their command line.
//!
//! `fast` is the blocking source gate of a local checkout and of hosted
//! automation alike. `deep` runs it and then holds the vendored tree to its
//! record, runs the native suites when the vendored tree was asked for, and
//! runs the mutation check. Every invocation has a new private directory of
//! evidence. Its `summary.json` holds only the names of the checks, how they
//! ended and how long they took: it is what may be uploaded.

use crate::{
    Fault, Result, actions,
    context::{Context, Failure, Limits, Note, Outcome, Run, Snapshot, executable},
    digest, environment,
    fast::{self, Programs, Scan},
    files, findings, licenses, migrations,
    mutation::{self, Mutation},
    openssl, policy, record, require, scope, semgrep,
    tools::{self, Curl, Site},
    vendor,
};
use serde_json::Value;
use std::{
    cell::{OnceCell, RefCell},
    ffi::{OsStr, OsString},
    fs::DirBuilder,
    io::{ErrorKind, Write},
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const USAGE: &str = concat!(
    "usage: simplestchat-security fast|deep [--base COMMIT] [--output DIR] [--include-vendor]\n",
    "       [--engine docker|podman] [--deep-check all|native|mutation]\n",
    "       [--openssl-prefix DIR] [--mutation-tool-platform linux-x86_64|darwin-x86_64]",
);
/// How long preparing the native suites, or running one, may take.
const NATIVE: Duration = Duration::from_secs(1800);
/// The native suites, each under the name of its check.
const SUITES: [(&str, &str); 3] = [
    ("native-asan", "asan"),
    ("native-ubsan", "ubsan"),
    ("native-replay", "replay"),
];

/// What a command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Whether the tier is `deep`; otherwise it is `fast`.
    pub deep: bool,
    /// The commit a change is compared with.
    pub base: Option<String>,
    /// The new directory of the evidence; one below `results` otherwise.
    pub output: Option<PathBuf>,
    /// Whether the vendored tree is scanned as well.
    pub include_vendor: bool,
    /// The container engine of the native suites.
    pub engine: &'static str,
    /// Which of the deep tier's own checks run.
    pub check: &'static str,
    /// The static OpenSSL of the mutation check.
    pub openssl: Option<PathBuf>,
    /// The platform the mutation tool is for.
    pub platform: Option<&'static str>,
}

impl Request {
    const fn tier(&self) -> &'static str {
        if self.deep { "deep" } else { "fast" }
    }

    /// What the summary says of the tier asked for.
    fn kind(&self) -> Kind<'static> {
        let deep_check = if self.deep {
            self.check.into()
        } else {
            Value::Null
        };
        Kind {
            tier: self.tier(),
            deep_check,
            include_vendor: self.include_vendor,
        }
    }
}

/// The request for `tier`, or nothing for a command line that is none: an
/// option that is unknown, given twice or without its value, a value that
/// is empty or begins like an option, or a choice that is not offered.
pub(crate) fn parse(tier: &str, arguments: &[String]) -> Option<Request> {
    let choice = |value: &str, offered: &[&'static str]| {
        let found = offered.iter().find(|offered| **offered == value);
        found.copied()
    };
    let mut request = Request {
        deep: choice(tier, &["fast", "deep"])? == "deep",
        base: None,
        output: None,
        include_vendor: false,
        engine: "docker",
        check: "all",
        openssl: None,
        platform: None,
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut rest = arguments.iter();
    while let Some(option) = rest.next() {
        if seen.contains(&option.as_str()) {
            return None;
        }
        seen.push(option);
        if option == "--include-vendor" {
            request.include_vendor = true;
            continue;
        }
        let value = rest.next()?;
        if value.is_empty() || value.starts_with('-') {
            return None;
        }
        match option.as_str() {
            "--base" => request.base = Some(value.clone()),
            "--output" => request.output = Some(PathBuf::from(value)),
            "--engine" => request.engine = choice(value, &["docker", "podman"])?,
            "--deep-check" => request.check = choice(value, &["all", "native", "mutation"])?,
            "--openssl-prefix" => request.openssl = Some(PathBuf::from(value)),
            "--mutation-tool-platform" => {
                request.platform = Some(choice(value, &["linux-x86_64", "darwin-x86_64"])?);
            }
            _ => return None,
        }
    }
    Some(request)
}

/// What a tier takes from the host it runs on.
pub trait Host {
    /// A variable of the caller's environment.
    fn ambient(&self, name: &str) -> Option<OsString>;

    /// The time it is.
    fn now(&self) -> SystemTime;

    /// The verified scanner `name` of the fast tier's installation, which is
    /// made when the first scanner is asked for.
    fn scanner(&self, name: &'static str) -> Result<PathBuf>;

    /// The locked version of the source scanner's engine, and the digest of
    /// the lock as it is now.
    fn engine(&self) -> Result<(String, String)>;

    /// The lock's name for the platform this host is.
    fn platform(&self) -> Result<&'static str>;

    /// Install the mutation tool for `platform` as the new `directory`.
    fn mutants(&self, directory: &Path, platform: &str) -> Result<()>;

    /// The verified mutation tool of that installation.
    fn mutant(&self, directory: &Path, platform: &str) -> Result<PathBuf>;

    /// Run the mutation check and answer whether it passed.
    fn mutation(&self, options: &Mutation<'_>) -> Outcome<bool>;

    /// Hold the vendored tree of `root` to its record, with the sources
    /// kept in `cache` and the evidence in the new directory `output`, and
    /// answer what is wrong with it.
    fn vendor(&self, root: &Path, cache: &Path, output: &Path) -> std::result::Result<(), String>;

    /// This gate's own program, whose `native-suites` command runs the
    /// native suites.
    fn program(&self) -> Result<PathBuf>;
}

/// This machine: the lock and the scanner cache of the checkout, the
/// caller's environment and the clock.
pub struct Machine<'a> {
    lock: PathBuf,
    cache: PathBuf,
    ambient: &'a dyn Fn(&str) -> Option<OsString>,
    installed: OnceCell<Result<PathBuf>>,
}

impl<'a> Machine<'a> {
    /// The machine for the checkout at `root`.
    pub fn new(root: &Path, ambient: &'a dyn Fn(&str) -> Option<OsString>) -> Self {
        Self {
            lock: root.join("build/security-tools.lock.json"),
            cache: root.join(".cache/security-tools"),
            ambient,
            installed: OnceCell::new(),
        }
    }

    fn site<'s>(&'s self, platform: &'s str) -> Site<'s> {
        Site {
            lock: &self.lock,
            cache: &self.cache,
            platform,
            limits: tools::Limits::REVIEWED,
        }
    }
}

impl Host for Machine<'_> {
    fn ambient(&self, name: &str) -> Option<OsString> {
        (self.ambient)(name)
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn scanner(&self, name: &'static str) -> Result<PathBuf> {
        let site = self.site(tools::host()?);
        let made = || tools::install(&site, &[], None, &mut Curl::default());
        let directory = self.installed.get_or_init(made).clone()?;
        tools::path(&site, name, Some(&directory))
    }

    fn engine(&self) -> Result<(String, String)> {
        tools::locked(&self.lock, "semgrep-core")
    }

    fn platform(&self) -> Result<&'static str> {
        tools::host()
    }

    fn mutants(&self, directory: &Path, platform: &str) -> Result<()> {
        let names = ["cargo-mutants".to_owned()];
        let site = self.site(platform);
        tools::install(&site, &names, Some(directory), &mut Curl::default())?;
        Ok(())
    }

    fn mutant(&self, directory: &Path, platform: &str) -> Result<PathBuf> {
        tools::path(&self.site(platform), "cargo-mutants", Some(directory))
    }

    fn mutation(&self, options: &Mutation<'_>) -> Outcome<bool> {
        mutation::run(options)
    }

    fn vendor(&self, root: &Path, cache: &Path, output: &Path) -> std::result::Result<(), String> {
        let verified = vendor::verify::fetching(&vendor::verify::Verification {
            root,
            cache,
            output,
            bounds: vendor::Bounds::REVIEWED,
        });
        verified.map_err(|vendor::Problem(complaint)| complaint)
    }

    fn program(&self) -> Result<PathBuf> {
        std::env::current_exe().map_err(|_| Fault("security_program_unlocated"))
    }
}

/// The programs of one invocation: the host's scanners, the host's own
/// programs on the search path of the invocation's commands, and the
/// interpreter of the scanner environment.
struct Equipped<'a> {
    host: &'a dyn Host,
    search: Option<OsString>,
    python: PathBuf,
}

impl Programs for Equipped<'_> {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Ok(self.host.scanner(name)?)
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        executable(name, self.search.as_deref())
    }

    fn python(&self) -> Outcome<PathBuf> {
        Ok(self.python.clone())
    }
}

/// Put the pinned compiler's own directory first on the search path of the
/// invocation's commands: the one that holds what `rustup` names as the
/// cargo of the channel the toolchain file pins.
pub(crate) fn pinned(context: &mut Context) -> Outcome<()> {
    let fault = Fault("security_rust_toolchain");
    let channel = mutation::channel(context.root())?.ok_or(fault)?;
    let rustup = executable("rustup", context.variable("PATH"))?;
    let arguments = ["which", "--toolchain", channel.as_str(), "cargo"].map(OsStr::new);
    let mut argv = vec![rustup.as_os_str()];
    argv.extend(arguments);
    let (status, found) = context.ask(&argv, &Run::default(), (4096, 4096))?;
    let found = String::from_utf8(found).map_err(|_| fault)?;
    let found = found.trim_matches(policy::space);
    require(status == 0 && !found.is_empty(), fault.0)?;
    let directory = Path::new(found).parent().ok_or(fault)?;
    context.prefer(directory);
    Ok(())
}

/// A program and its arguments as one command.
fn command<'a>(program: &'a Path, arguments: &[&'a OsStr]) -> Vec<&'a OsStr> {
    let mut argv = vec![program.as_os_str()];
    argv.extend_from_slice(arguments);
    argv
}

/// The blocking checks of the sources: the workflows, secrets, dependencies
/// and their advisories, the licences and the actions a change declares
/// anew, the reviewed source rules, new migrations and the pinned OpenSSL.
/// What they read is one private copy of the sources, and neither the
/// sources nor the commit may change while they run.
fn fast(
    context: &mut Context,
    request: &Request,
    host: &dyn Host,
    programs: &dyn Programs,
    git: &Path,
) -> Outcome<()> {
    let (base, vendor) = (request.base.as_deref(), request.include_vendor);
    let head = command(git, &["rev-parse", "HEAD"].map(OsStr::new));
    let (_, revision) = context.run("source-revision", &head, &Run::default())?;
    let snapshot = context.snapshot(Snapshot::Source, git)?;
    let today = policy::Date::at(host.now());
    let reviews = policy::read(&snapshot.join("security/exceptions.json"), today, vendor)?;
    fast::workflows(context, programs, &reviews)?;
    let scan = Scan {
        reviews: &reviews,
        base,
        snapshot: &snapshot,
        include_vendor: vendor,
        git,
    };
    fast::secret_scan(context, programs, &scan)?;
    let since = host.now().duration_since(UNIX_EPOCH).unwrap_or_default();
    fast::dependencies(context, programs, &reviews, since)?;
    let review = licenses::Review {
        snapshot: &snapshot,
        base,
        include_vendor: vendor,
        bounds: licenses::Bounds::REVIEWED,
        reviewed: &licenses::REVIEWED,
        controller: licenses::CONTROLLER,
    };
    licenses::check(context, programs, &review)?;
    let review = actions::Review {
        snapshot: &snapshot,
        base,
        include_vendor: vendor,
    };
    actions::check(context, programs, &review)?;
    let (version, _) = host.engine()?;
    let engine = semgrep::Engine {
        path: &programs.program("semgrep-core")?,
        version: &version,
        lock: &|| host.engine().map(|(_, lock)| lock),
    };
    semgrep::gate(context, &engine, git, vendor)?;
    let new = findings::new_migrations(context.root())?;
    migrations::lint(context, programs, &new)?;
    context.note(vec![
        ("name", "migration-review".into()),
        ("newFiles", new.len().into()),
        ("exitStatus", 0.into()),
    ]);
    openssl::check(context, programs, &snapshot, host.now())?;
    context.snapshot(Snapshot::Final, git)?;
    let listed = |stem: &str| {
        let manifest = context.output().join(format!("{stem}-manifest.json"));
        files::bounded(&manifest, context.limits().tree, false)
    };
    let original = listed(Snapshot::Source.label())?;
    require(
        original == listed(Snapshot::Final.label())?,
        "security_source_changed_during_checks",
    )?;
    let (_, last) = context.run("final-revision", &head, &Run::default())?;
    require(revision == last, "security_revision_changed_during_checks")?;
    let revision = String::from_utf8(revision).map_err(|_| Fault("security_source_revision"))?;
    context.note(vec![
        ("name", "source-identity".into()),
        ("exitStatus", 0.into()),
        ("revision", revision.trim_matches(policy::space).into()),
        ("sourceSha256", digest(&original).into()),
    ]);
    Ok(())
}

/// Build and run the finite native suites with this gate's own `program`,
/// in the offline sandbox that the preparation reports.
fn native(context: &mut Context, program: &Path, engine: &str) -> Outcome<()> {
    let slow = Run {
        within: NATIVE,
        ..Run::default()
    };
    let preparation = context.output().join("native-preparation");
    let arguments = [
        OsStr::new("native-suites"),
        OsStr::new("prepare"),
        OsStr::new("--engine"),
        OsStr::new(engine),
        OsStr::new("--output"),
        preparation.as_os_str(),
    ];
    context.run("native-prepare", &command(program, &arguments), &slow)?;
    let report = preparation.join("report.json");
    let report = record::document(&files::bounded(&report, context.limits().report, false)?)?;
    require(report.is_object(), "scanner_report_object")?;
    let image = record::string(&report["imageId"])?;
    for (name, mode) in SUITES {
        let output = context.output().join(name);
        let arguments = [
            OsStr::new("native-suites"),
            OsStr::new("run"),
            OsStr::new("--engine"),
            OsStr::new(engine),
            OsStr::new("--image"),
            OsStr::new(image),
            OsStr::new("--mode"),
            OsStr::new(mode),
            OsStr::new("--output"),
            output.as_os_str(),
        ];
        context.run(name, &command(program, &arguments), &slow)?;
    }
    Ok(())
}

/// Run the mutation check with the pinned tool for the platform that was
/// asked for, installed into the evidence, and the static OpenSSL that was
/// named, that the caller's environment names, or at its usual place.
fn mutated(
    context: &mut Context,
    request: &Request,
    host: &dyn Host,
    programs: &dyn Programs,
    git: &Path,
) -> Outcome<()> {
    let platform = match request.platform {
        Some(platform) => platform,
        None => host.platform()?,
    };
    let directory = context.output().join("mutation-tools");
    host.mutants(&directory, platform)?;
    let named = request.openssl.clone();
    let named = named.or_else(|| host.ambient("OPENSSL_DIR").map(PathBuf::from));
    let prefix = context
        .root()
        .join(named.unwrap_or_else(|| PathBuf::from("target/openssl-4.1.0")));
    let prefix = prefix.canonicalize();
    let prefix = prefix.map_err(|error| files::unusable(&error))?;
    let (status, _) = context.told("mutation-policy", |outer| {
        let output = outer.output().join("mutation");
        let outcome = host.mutation(&Mutation {
            root: outer.root(),
            output: &output,
            platform,
            darwin: cfg!(target_os = "macos"),
            openssl: &prefix,
            tool: &|| Ok(host.mutant(&directory, platform)?),
            programs,
            git,
            ambient: &|name| outer.variable(name).map(OsStr::to_owned),
            bounds: mutation::Bounds::REVIEWED,
        });
        mutation::finished(&outcome, &output)
    })?;
    if status == 0 {
        Ok(())
    } else {
        Err(Failure::Command("mutation-policy"))
    }
}

/// The deep tier's own checks, after the whole source gate: the vendored
/// tree against its record, the native suites when the vendored tree was
/// asked for, and the mutation check.
fn deep(
    context: &mut Context,
    request: &Request,
    host: &dyn Host,
    programs: &dyn Programs,
    git: &Path,
) -> Outcome<()> {
    let (cache, output) = (
        context.output().join("vendor-cache"),
        context.output().join("vendor"),
    );
    // What is wrong with the vendored tree is kept as the line the
    // reference's command printed, and never leaves the evidence.
    let said = RefCell::new(String::new());
    let verified = |outer: &Context| {
        let verified = host.vendor(outer.root(), &cache, &output);
        verified.map_err(|complaint| {
            said.replace(format!("Vendor integrity failed: {complaint}\n"));
            Failure::Command("vendor-integrity")
        })
    };
    context.step("vendor-integrity", verified, |_| said.take())?;
    if request.include_vendor && request.check != "mutation" {
        native(context, &host.program()?, request.engine)?;
    }
    if request.check != "native" {
        mutated(context, request, host, programs, git)?;
    }
    Ok(())
}

/// Hold the request to the rules of its tier: the vendored tree is scanned
/// only where no automation runs, the deep tier's options belong to the
/// deep tier, and the native suites need the vendored tree.
fn ruled(request: &Request, host: &dyn Host) -> Result<()> {
    let automated = scope::automated(|name| host.ambient(name).is_some());
    scope::require_local_vendor(request.include_vendor, automated)?;
    let plain = request.check == "all" && request.openssl.is_none() && request.platform.is_none();
    require(request.deep || plain, "security_deep_arguments")?;
    require(
        request.check != "native" || request.include_vendor,
        "native_scan_requires_include_vendor",
    )
}

/// Hold the request to its rules, equip the invocation with the pinned
/// compiler and the scanner environment, and run the tier.
fn checked(context: &mut Context, request: &Request, host: &dyn Host) -> Outcome<()> {
    ruled(request, host)?;
    pinned(context)?;
    let search = context.variable("PATH").map(OsStr::to_owned);
    let python3 = executable("python3", search.as_deref())?;
    let python = environment::interpreter(context, &python3)?;
    let programs = Equipped {
        host,
        search,
        python,
    };
    let git = programs.prerequisite("git")?;
    fast(context, request, host, &programs, &git)?;
    if request.deep {
        deep(context, request, host, &programs, &git)?;
    }
    Ok(())
}

/// What a summary says of the tier it is of, besides how it ended.
#[derive(Clone, Debug)]
pub(crate) struct Kind<'a> {
    /// The tier's name.
    pub(crate) tier: &'a str,
    /// The deep tier's own checks, or nothing.
    pub(crate) deep_check: Value,
    /// Whether the vendored tree was scanned as well.
    pub(crate) include_vendor: bool,
}

/// The summary of an invocation of the tier `kind` that ended in `failure`
/// or none, with `checks`, as the reference writes it: the fields in their
/// fixed order, every check with its fields in the order they were
/// recorded, two spaces a level and ASCII only.
pub(crate) fn summarized(kind: &Kind<'_>, failure: Option<&str>, checks: &[Note]) -> String {
    let fields: [(&str, Value); 6] = [
        ("schemaVersion", 1.into()),
        ("tier", kind.tier.into()),
        ("deepCheck", kind.deep_check.clone()),
        ("includeVendor", kind.include_vendor.into()),
        ("passed", failure.is_none().into()),
        ("failure", failure.into()),
    ];
    let mut text = String::from("{\n");
    let field = |text: &mut String, depth: usize, name: &str, value: &Value| {
        text.push_str(&"  ".repeat(depth));
        text.push_str(&record::quoted(name));
        text.push_str(": ");
        record::spread(value, depth, text);
    };
    for (name, value) in &fields {
        field(&mut text, 1, name, value);
        text.push_str(",\n");
    }
    text.push_str("  \"checks\": [");
    for (index, check) in checks.iter().enumerate() {
        text.push_str(if index == 0 { "\n    {" } else { ",\n    {" });
        for (place, (name, value)) in check.iter().enumerate() {
            text.push_str(if place == 0 { "\n" } else { ",\n" });
            field(&mut text, 3, name, value);
        }
        text.push_str("\n    }");
    }
    text.push_str(if checks.is_empty() {
        "]\n}\n"
    } else {
        "\n  ]\n}\n"
    });
    text
}

/// A new private directory for the evidence of `tier` below `results`,
/// which is made when it is not there.
fn fresh(results: &Path, tier: &str) -> Result<PathBuf> {
    match DirBuilder::new().mode(0o700).create(results) {
        Err(error) if !results.is_dir() => {
            return Err(files::unusable(&error));
        }
        _ => {}
    }
    let process = std::process::id();
    for _ in 0..16 {
        let moment = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let path = results.join(format!("security.{tier}.{process}-{moment:09}"));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(files::unusable(&error)),
        }
    }
    Err(Fault("security_evidence_directory"))
}

/// Run the tier that `request` names over the sources below `root`, and
/// answer whether it passed and where its evidence is. The evidence is a new
/// private directory: the one that was named, from `root` when it is named
/// from nowhere, or one below `results`. However the tier ends, the summary
/// is written. An invocation that cannot have its evidence has no summary.
pub fn gate(root: &Path, request: &Request, host: &dyn Host) -> Outcome<(bool, PathBuf)> {
    let ambient = |name: &str| host.ambient(name);
    let named = request.output.as_deref();
    evidenced(root, &request.kind(), named, &ambient, |context| {
        checked(context, request, host)
    })
}

/// Do `work` as an invocation of the tier `kind` over the sources below
/// `root`, and answer whether it passed and where its evidence is: the new
/// private directory `named`, from `root` when it is named from nowhere, or
/// one below `results`. However the work ends, the summary is written; an
/// invocation that cannot have its evidence has no summary.
pub(crate) fn evidenced(
    root: &Path,
    kind: &Kind<'_>,
    named: Option<&Path>,
    ambient: &dyn Fn(&str) -> Option<OsString>,
    work: impl FnOnce(&mut Context) -> Outcome<()>,
) -> Outcome<(bool, PathBuf)> {
    let unusable = |error: std::io::Error| files::unusable(&error);
    let output = match named {
        Some(named) => {
            let named = root.join(named);
            DirBuilder::new()
                .mode(0o700)
                .create(&named)
                .map_err(unusable)?;
            named
        }
        None => fresh(&root.join("results"), kind.tier)?,
    };
    let output = output.canonicalize().map_err(unusable)?;
    let mut context = Context::new(root, &output, Limits::REVIEWED, ambient)?;
    let outcome = work(&mut context);
    let failure = outcome.as_ref().err().map(ToString::to_string);
    let text = summarized(kind, failure.as_deref(), context.checks());
    files::write_private(&output.join("summary.json"), text.as_bytes(), 0o600)?;
    Ok((outcome.is_ok(), output))
}

/// The line that says how an invocation ended and where its evidence is.
fn announced(request: &Request, passed: bool, output: &Path) -> Vec<u8> {
    proclaimed(request.tier(), passed, output)
}

/// The line that says how an invocation of `tier` ended and where its
/// evidence is.
pub(crate) fn proclaimed(tier: &str, passed: bool, output: &Path) -> Vec<u8> {
    let verdict = if passed { "passed" } else { "failed" };
    let mut line = format!("Security {tier}: {verdict}; evidence: ").into_bytes();
    line.extend_from_slice(output.as_os_str().as_bytes());
    line.push(b'\n');
    line
}

/// Run one tier for the checkout at `root`: one line on standard output
/// that says whether it passed and where its evidence is, with status 0 only
/// when it passed. Whatever the invocation and the commands it runs create
/// is closed to every other account.
pub fn run(
    root: &Path,
    tier: &str,
    arguments: &[String],
    ambient: &dyn Fn(&str) -> Option<OsString>,
) -> ExitCode {
    let Some(request) = parse(tier, arguments) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    match gate(root, &request, &Machine::new(root, ambient)) {
        Ok((passed, output)) => {
            let line = announced(&request, passed, &output);
            match std::io::stdout().write_all(&line) {
                Ok(()) if passed => ExitCode::SUCCESS,
                _ => ExitCode::from(1),
            }
        }
        Err(failure) => {
            eprintln!("Security gate failed: {failure}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests;
