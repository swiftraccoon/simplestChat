//! `image check IMAGE [--output DIR] [--engine docker|podman]
//! [--artifact-dir DIR] [--image-tools DIR] [--image-database-cache DIR]`:
//! the image tier. It exports the image once when no export is named,
//! installs the image scanners or verifies the ones it is given, and runs
//! the image gate as one check of a new private directory of evidence,
//! whose `summary.json` is what may be uploaded.

use crate::{
    Result,
    context::{Context, Outcome, Run, executable},
    environment,
    image::{gate::LOCK, host::TOOL_CACHE},
    require, tier,
    tools::{self, Curl, Site},
};
use serde_json::Value;
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const USAGE: &str = concat!(
    "usage: simplestchat-security image check IMAGE [--output DIR] [--engine docker|podman]\n",
    "       [--artifact-dir DIR] [--image-tools DIR] [--image-database-cache DIR]",
);
/// How long the export, or the gate, may take.
const PATIENCE: Duration = Duration::from_secs(1800);
/// The scanners of the image tier.
const SCANNERS: [&str; 3] = ["syft", "grype", "gitleaks"];
/// The lock's name of the platform the image is for.
const PLATFORM: &str = "linux-x86_64";

/// What a command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Request {
    /// The image.
    pub(super) image: String,
    /// The new directory of the evidence, if it is named.
    pub(super) output: Option<PathBuf>,
    /// The engine.
    pub(super) engine: &'static str,
    /// The export of the image, if there is one already.
    pub(super) artifact: Option<PathBuf>,
    /// A prepared installation of the scanners, if there is one.
    pub(super) tools: Option<PathBuf>,
    /// The database cache, if there is one.
    pub(super) cache: Option<PathBuf>,
}

/// The request, or nothing for a command line that is none: an option that
/// is unknown, given twice or without its value, a value that is empty or
/// begins like an option, an engine that is not offered, or no image or two.
pub(super) fn parsed(arguments: &[String]) -> Option<Request> {
    let mut request = Request {
        image: String::new(),
        output: None,
        engine: "docker",
        artifact: None,
        tools: None,
        cache: None,
    };
    let (mut image, mut seen) = (None, Vec::new());
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        if !argument.starts_with('-') {
            if image.is_some() || argument.is_empty() {
                return None;
            }
            image = Some(argument.clone());
            continue;
        }
        if seen.contains(&argument.as_str()) {
            return None;
        }
        seen.push(argument.as_str());
        let value = rest
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with('-'))?;
        match argument.as_str() {
            "--output" => request.output = Some(value.into()),
            "--engine" => {
                let offered = ["docker", "podman"].into_iter();
                request.engine = offered.into_iter().find(|offered| offered == value)?;
            }
            "--artifact-dir" => request.artifact = Some(value.into()),
            "--image-tools" => request.tools = Some(value.into()),
            "--image-database-cache" => request.cache = Some(value.into()),
            _ => return None,
        }
    }
    request.image = image?;
    Some(request)
}

/// What the image tier asks of the machine it runs on, beside its own
/// context.
pub(super) trait Machine {
    /// The interpreter that runs the release's exporter.
    fn python(&self, context: &mut Context) -> Outcome<PathBuf>;

    /// Install the scanners `names` for the lock's platform `platform` as
    /// the directory `directory`, or verify the ones there, and answer it.
    fn install(
        &self,
        names: &[String],
        directory: &Path,
        platform: &'static str,
    ) -> Result<PathBuf>;

    /// The program that runs the image gate.
    fn program(&self) -> &Path;
}

/// This machine, for the checkout at `root`: the scanners' own environment
/// for the exporter, the scanners checksum-pinned by its lock, and this very
/// program for the gate.
struct Here<'a> {
    root: &'a Path,
    program: PathBuf,
}

impl Machine for Here<'_> {
    fn python(&self, context: &mut Context) -> Outcome<PathBuf> {
        let search = context.variable("PATH").map(OsStr::to_owned);
        let python3 = executable("python3", search.as_deref())?;
        environment::interpreter(context, &python3)
    }

    fn install(
        &self,
        names: &[String],
        directory: &Path,
        platform: &'static str,
    ) -> Result<PathBuf> {
        let (lock, cache) = (self.root.join(LOCK), self.root.join(TOOL_CACHE));
        let site = Site {
            lock: &lock,
            cache: &cache,
            platform,
            limits: tools::Limits::REVIEWED,
        };
        tools::install(&site, names, Some(directory), &mut Curl::default())
    }

    fn program(&self) -> &Path {
        &self.program
    }
}

/// Run the image tier as `request` asks, in `context`, on `machine`: the
/// export first when none is named, then the scanners, then the gate.
fn checked(context: &mut Context, request: &Request, machine: &dyn Machine) -> Outcome<()> {
    let output = context.output().to_path_buf();
    let patient = Run {
        within: PATIENCE,
        ..Run::default()
    };
    let artifact = match &request.artifact {
        Some(artifact) => artifact.clone(),
        None => {
            let docker = request.engine == "docker";
            require(docker, "canonical_release_export_requires_docker")?;
            let python = machine.python(context)?;
            let export = output.join("export");
            let argv: [OsString; 6] = [
                python.into(),
                "build/build-release.py".into(),
                "--image-id".into(),
                request.image.clone().into(),
                "--output".into(),
                export.clone().into(),
            ];
            context.run("image-export", &argv, &patient)?;
            export
        }
    };
    if let Some(tools) = &request.tools {
        let kind = tools.symlink_metadata();
        let prepared = tools.is_absolute() && kind.is_ok_and(|kind| kind.is_dir());
        require(prepared, "invalid_prepared_image_tools")?;
    }
    let directory = request
        .tools
        .clone()
        .unwrap_or_else(|| output.join("image-tools"));
    let names: Vec<String> = SCANNERS.iter().map(|name| (*name).to_owned()).collect();
    let installed = machine.install(&names, &directory, PLATFORM)?;
    let mut argv: Vec<OsString> = vec![
        machine.program().into(),
        "image".into(),
        "scan".into(),
        request.image.clone().into(),
        "--artifact-dir".into(),
        artifact.into(),
        "--output".into(),
        output.join("image").into(),
        "--engine".into(),
        request.engine.into(),
        "--tools-directory".into(),
        installed.into(),
    ];
    if let Some(cache) = &request.cache {
        argv.extend(["--database-cache".into(), cache.clone().into()]);
    }
    context.run("image-security", &argv, &patient)?;
    Ok(())
}

/// The image tier that `request` asks for over the checkout at `root`, on
/// `machine`: whether it passed, and where its evidence is.
pub(super) fn tiered(
    root: &Path,
    request: &Request,
    machine: &dyn Machine,
    ambient: &dyn Fn(&str) -> Option<OsString>,
) -> Outcome<(bool, PathBuf)> {
    let kind = tier::Kind {
        tier: "image",
        deep_check: Value::Null,
        include_vendor: false,
    };
    let named = request.output.as_deref();
    tier::evidenced(root, &kind, named, ambient, |context| {
        checked(context, request, machine)
    })
}

/// Run `image check` for the checkout at `root`: one line on `said` that
/// says whether the image passed and where its evidence is, with status 0
/// only when it passed. Whatever the tier and the commands it runs create is
/// closed to every other account.
fn ran(root: &Path, arguments: &[String], said: &mut dyn Write) -> ExitCode {
    let Some(request) = parsed(arguments) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let Ok(program) = std::env::current_exe() else {
        eprintln!("Security gate failed: image_gate_program");
        return ExitCode::from(1);
    };
    let machine = Here { root, program };
    let ambient = |name: &str| std::env::var_os(name);
    match tiered(root, &request, &machine, &ambient) {
        Ok((passed, output)) => {
            let line = tier::proclaimed("image", passed, &output);
            concluded(passed, said.write_all(&line))
        }
        Err(failure) => {
            eprintln!("Security gate failed: {failure}");
            ExitCode::from(1)
        }
    }
}

/// The exit status of a tier that `passed`, once its line was `written`.
fn concluded(passed: bool, written: std::io::Result<()>) -> ExitCode {
    match written {
        Ok(()) if passed => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

/// Run `image check` for the checkout at `root`, its line on standard
/// output.
pub(super) fn run(root: &Path, arguments: &[String]) -> ExitCode {
    ran(root, arguments, &mut std::io::stdout())
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
