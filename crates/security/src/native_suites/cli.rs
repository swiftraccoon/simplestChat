//! `simplestchat-security native-suites`: verify the reviewed corpus,
//! prepare a suite's image, compile a suite and keep it, or run one, each
//! with its evidence in a new private directory.

use super::{
    FAMILIES, MIB, MODES,
    budget::{Budget, detect},
    corpus::corpus,
    engine::Engine,
    local::{Local, failure_code},
    run::{Host, Request, compile, prepare, run as suite, write_json},
    sandbox::native_architecture,
};
use crate::{
    Fault, Result, context,
    document::Document,
    engine::{Host as Selection, Name, prefix},
    image::host::Signals,
    require, scope,
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read as _, Write},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How the command is used.
const USAGE: &str = concat!(
    "usage: simplestchat-security native-suites verify\n",
    "       simplestchat-security native-suites prepare --output DIR [--engine docker|podman]\n",
    "       simplestchat-security native-suites run --mode asan|ubsan|replay --image ID\n",
    "           --output DIR [--compiled-directory DIR] [--engine docker|podman]\n",
    "       simplestchat-security native-suites compile --mode asan|ubsan|replay --image ID\n",
    "           --compiled-directory DIR --output DIR [--engine docker|podman]",
);
/// The operations, each with the options it takes.
const OPERATIONS: [(&str, &[&str]); 4] = [
    ("verify", &[]),
    ("prepare", &["--engine", "--output"]),
    (
        "run",
        &[
            "--engine",
            "--output",
            "--mode",
            "--image",
            "--compiled-directory",
        ],
    ),
    (
        "compile",
        &[
            "--engine",
            "--output",
            "--mode",
            "--image",
            "--compiled-directory",
        ],
    ),
];
/// The two names of the help, which every parser of the reference's knew.
const HELP: [&str; 2] = ["-h", "--help"];
/// How long a question that selects the engine may take.
const SELECTION_SECONDS: Duration = Duration::from_secs(30);
/// The most bytes it may print on either of its streams.
const SELECTION_BYTES: u64 = 2 * MIB;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub struct Asked {
    /// The operation.
    pub operation: String,
    /// The engine: Docker unless another is named.
    pub engine: Name,
    /// The new directory of the evidence.
    pub output: Option<PathBuf>,
    /// The suite.
    pub mode: Option<String>,
    /// The image.
    pub image: Option<String>,
    /// Where a build is kept, or is to be.
    pub compiled: Option<PathBuf>,
}

/// What a command line comes to, as the reference's parser read one.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// An operation with its options.
    Asked(Asked),
    /// The usage, asked for: the reference's parser then ended with 0.
    Help,
    /// No command line of this command's: the reference's parser then
    /// ended with 2.
    Refused,
}

/// How the reference's parser read one argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read<'a> {
    /// A word that names no option: a value, or one too many.
    Word,
    /// `--`, after which every argument is a word.
    End,
    /// An option, with the value given after its `=` where there is one.
    Named(&'static str, Option<&'a str>),
    /// The help: asked for, or refused for a value given to it.
    Help(bool),
    /// The beginning of more than one option.
    Ambiguous,
    /// An option that the parser does not take.
    Unknown,
}

/// How the reference's parser read `argument` where its options are
/// `options` and the help: `--`; an option named whole, or a long one by a
/// beginning that is no other's, each with or without a value after `=`;
/// the short help with more after it; and otherwise a word, unless it begins
/// like an option and is no negative number and holds no space.
fn read<'a>(argument: &'a str, options: &[&'static str]) -> Read<'a> {
    if argument == "--" {
        return Read::End;
    }
    if !argument.starts_with('-') {
        return Read::Word;
    }
    if HELP.contains(&argument) {
        return Read::Help(true);
    }
    if let Some(option) = options.iter().find(|option| **option == argument) {
        return Read::Named(option, None);
    }
    if argument.len() == 1 {
        return Read::Word;
    }
    if let Some((name, value)) = argument.split_once('=') {
        if HELP.contains(&name) {
            return Read::Help(false);
        }
        if let Some(option) = options.iter().find(|option| **option == name) {
            return Read::Named(option, Some(value));
        }
    }
    if argument.starts_with("--") {
        let (beginning, value) = match argument.split_once('=') {
            Some((beginning, value)) => (beginning, Some(value)),
            None => (argument, None),
        };
        let all = options.iter().copied().chain(["--help"]);
        let begun: Vec<&'static str> = all.filter(|option| option.starts_with(beginning)).collect();
        match begun[..] {
            [] => {}
            ["--help"] => return Read::Help(value.is_none()),
            [option] => return Read::Named(option, value),
            _ => return Read::Ambiguous,
        }
    } else if argument.starts_with("-=") {
        // Every option begins with the one prefix before the `=`.
        return Read::Ambiguous;
    } else if let Some(rest) = argument.strip_prefix("-h") {
        // The parser took what follows the short help for more short
        // options, unless it began with a prefix.
        return Read::Help(!rest.starts_with('-'));
    }
    let number = argument[1..].strip_prefix('.').unwrap_or(&argument[1..]);
    if number.starts_with(|letter: char| letter.is_ascii_digit()) || argument.contains(' ') {
        return Read::Word;
    }
    Read::Unknown
}

/// The request of `arguments` for the operation `operation`, which takes
/// `options`, as the reference's parser read them: an option given more
/// than once counts as it was given last, an option with no value of its
/// own takes the next argument when that is a word, and an engine and a
/// suite must be ones there are.
fn requested(operation: &str, options: &[&'static str], arguments: &[String]) -> Line {
    let mut asked = Asked {
        operation: operation.to_owned(),
        engine: Name::Docker,
        output: None,
        mode: None,
        image: None,
        compiled: None,
    };
    let mut ended = false;
    let mut reads = Vec::new();
    for argument in arguments {
        let read = if ended {
            Read::Word
        } else {
            read(argument, options)
        };
        ended |= read == Read::End;
        reads.push(read);
    }
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
            Read::Named(option, Some(value)) => (option, value),
            Read::Named(option, None) => {
                if reads.get(index) != Some(&Read::Word) {
                    return Line::Refused;
                }
                index += 1;
                (option, arguments[index - 1].as_str())
            }
        };
        match option {
            "--engine" => match Name::parsed(value) {
                Some(engine) => asked.engine = engine,
                None => return Line::Refused,
            },
            "--mode" if MODES.contains(&value) => asked.mode = Some(value.to_owned()),
            "--mode" => return Line::Refused,
            "--output" => asked.output = Some(PathBuf::from(value)),
            "--image" => asked.image = Some(value.to_owned()),
            _ => asked.compiled = Some(PathBuf::from(value)),
        }
    }
    let (output, mode, image) = (
        asked.output.is_some(),
        asked.mode.is_some(),
        asked.image.is_some(),
    );
    let complete = match operation {
        "verify" => true,
        "prepare" => output,
        "run" => output && mode && image,
        _ => output && mode && image && asked.compiled.is_some(),
    };
    if complete && !stray {
        Line::Asked(asked)
    } else {
        Line::Refused
    }
}

/// What the command line `arguments` comes to, as the reference's parser
/// read it: the help or a stray option before the operation, `--` before
/// it, and the operation, whose own parser reads the rest. Its operations
/// for the sandbox's side are none of this command's: nothing of this
/// repository's runs in the image.
pub fn parsed(arguments: &[String]) -> Line {
    let mut stray = false;
    let mut ended = false;
    for (index, argument) in arguments.iter().enumerate() {
        let read = if ended {
            Read::Word
        } else {
            read(argument, &[])
        };
        match read {
            Read::End => ended = true,
            Read::Unknown => stray = true,
            Read::Help(true) => return Line::Help,
            Read::Help(false) | Read::Ambiguous | Read::Named(..) => return Line::Refused,
            Read::Word => {
                let found = OPERATIONS.iter().find(|(name, _)| name == argument);
                let Some((name, options)) = found else {
                    return Line::Refused;
                };
                return match requested(name, options, &arguments[index + 1..]) {
                    Line::Asked(_) if stray => Line::Refused,
                    line => line,
                };
            }
        }
    }
    Line::Refused
}

/// A new random run identity: thirty-two small hexadecimal digits.
pub(crate) fn run_identity() -> Result<String> {
    let mut random = [0_u8; 16];
    let mut source = fs::File::open("/dev/urandom").map_err(|_| Fault("native_run_identity"))?;
    source
        .read_exact(&mut random)
        .map_err(|_| Fault("native_run_identity"))?;
    Ok(crate::hexadecimal(&random))
}

/// The new private evidence directory `output`, which must be named whole.
fn fresh(output: &Path) -> Result<()> {
    require(output.is_absolute(), "absolute_output_required")?;
    let created = fs::DirBuilder::new().mode(0o700).create(output);
    created.map_err(|_| Fault("invalid_native_security_input"))
}

/// What the command `arguments`, which begin with its program, printed as
/// the selection of an engine asked it: once it ended with 0 within the
/// bounds of the reference's own questions.
pub(crate) fn selecting(arguments: &[OsString]) -> Result<Vec<u8>> {
    let (program, rest) = arguments.split_first().ok_or(Fault("engine_missing"))?;
    let mut command = std::process::Command::new(program);
    command.args(rest);
    let room = (SELECTION_BYTES, SELECTION_BYTES);
    let asked = context::asked(&mut command, &[], SELECTION_SECONDS, room);
    let (status, printed) = asked.map_err(|failure| Fault(failure_code(&failure)))?;
    require(status == 0, "command_failed")?;
    Ok(printed)
}

/// The local engine `name` names, as this host's search path `search` and
/// home `home` find it, whose commands stop once `cancelled` says so.
pub(crate) fn selected<'c>(
    name: Name,
    search: Option<&OsStr>,
    home: Option<PathBuf>,
    cancelled: &'c dyn Fn() -> bool,
) -> Result<Box<dyn Engine + 'c>> {
    let located = |program: &'static str| context::executable(program, search).ok();
    let present = |path: &Path| path.exists();
    let mut asking = selecting;
    let mut selection = Selection {
        located: &located,
        home,
        present: &present,
        mac: cfg!(target_os = "macos"),
        asked: &mut asking,
    };
    let prefix = prefix(name, &mut selection)?;
    Ok(Box::new(Local { prefix, cancelled }))
}

/// What the command looks up and acts through on this host.
pub(crate) struct Around<'a, 'e> {
    /// The caller's environment variable `name`, where it is set.
    pub variable: &'a dyn Fn(&str) -> Option<OsString>,
    /// The local engine `name` names, reached through the prefix that
    /// selects it; asked for only once the evidence directory is made.
    pub engine: &'a mut dyn FnMut(Name) -> Result<Box<dyn Engine + 'e>>,
    /// This host's machine, as its system names it.
    pub machine: &'a str,
    /// What a build may use on this host.
    pub budget: &'a dyn Fn() -> Result<Budget>,
    /// How long the command has run.
    pub clock: &'a dyn Fn() -> Duration,
    /// A new run identity.
    pub run_id: &'a dyn Fn() -> Result<String>,
    /// Where the command's answer goes.
    pub out: &'a mut dyn Write,
    /// Where its failure is told.
    pub err: &'a mut dyn Write,
}

/// The operation `asked` below `root`, in this order: refused where
/// automation runs, then its evidence directory made, which `made` says
/// once it is, then its engine selected, and then the operation, whose
/// answer is kept as the directory's report.
fn operated(
    root: &Path,
    asked: &Asked,
    around: &mut Around<'_, '_>,
    made: &mut bool,
) -> Result<Document> {
    let variable = around.variable;
    scope::require_local_vendor(true, scope::automated(|name| variable(name).is_some()))?;
    let output = asked
        .output
        .as_deref()
        .ok_or(Fault("absolute_output_required"))?;
    fresh(output)?;
    *made = true;
    let mut engine = (around.engine)(asked.engine)?;
    let mut host = Host {
        root,
        engine: &mut *engine,
        architecture: native_architecture(around.machine),
        budget: around.budget,
        run_id: (around.run_id)()?,
        clock: around.clock,
    };
    let request = Request {
        mode: asked.mode.as_deref().unwrap_or_default(),
        image: asked.image.as_deref().unwrap_or_default(),
        compiled: asked.compiled.as_deref(),
    };
    let result = match asked.operation.as_str() {
        "prepare" => prepare(&mut host, output),
        "compile" => compile(&mut host, &request, output),
        _ => suite(&mut host, &request, output),
    }?;
    write_json(&output.join("report.json"), &result)?;
    Ok(result)
}

/// The exit status of the operation `asked` below `root`, once `ready`
/// says that it may begin: its answer printed as one sorted line, or its
/// failure told, and reported in its evidence directory when this run made
/// that and nothing is reported there yet.
pub(crate) fn finished(
    root: &Path,
    asked: &Asked,
    around: &mut Around<'_, '_>,
    ready: Result<()>,
) -> u8 {
    let mut made = false;
    let outcome = ready.and_then(|()| operated(root, asked, around, &mut made));
    match outcome {
        Ok(result) => {
            let _ = writeln!(around.out, "{}", result.sorted().spaced());
            0
        }
        Err(fault) => {
            let output = asked.output.as_deref();
            let report = output.map(|output| output.join("report.json"));
            if let Some(report) = report.filter(|report| made && report.symlink_metadata().is_err())
            {
                let failed = Document::record([
                    ("schemaVersion", 1_u64.into()),
                    ("status", "failed".into()),
                    ("error", fault.0.into()),
                ]);
                let _ = write_json(&report, &failed);
            }
            let _ = writeln!(around.err, "Native security check failed: {fault}");
            1
        }
    }
}

/// The exit status of the verification of the reviewed corpus below
/// `root`: its families and its number of inputs printed, or why it is
/// refused told.
fn verified(root: &Path, around: &mut Around<'_, '_>) -> u8 {
    match corpus(root) {
        Ok(cases) => {
            let families = Document::List(FAMILIES.iter().map(|family| (*family).into()).collect());
            let verified = Document::record([
                ("status", "passed".into()),
                ("corpusCases", cases.len().into()),
                ("families", families),
            ]);
            let _ = writeln!(around.out, "{}", verified.spaced());
            0
        }
        Err(fault) => {
            let _ = writeln!(around.err, "Native security check failed: {fault}");
            1
        }
    }
}

/// The exit status of the command line `arguments` below `root`: the
/// usage printed when it is asked for and told when the line is refused,
/// the corpus verified, or else the operation, once `ready` lets it begin.
pub(crate) fn command(
    root: &Path,
    arguments: &[String],
    around: &mut Around<'_, '_>,
    ready: &mut dyn FnMut() -> Result<()>,
) -> u8 {
    match parsed(arguments) {
        Line::Help => {
            let _ = writeln!(around.out, "{USAGE}");
            0
        }
        Line::Refused => {
            let _ = writeln!(around.err, "{USAGE}");
            2
        }
        Line::Asked(asked) if asked.operation == "verify" => verified(root, around),
        Line::Asked(asked) => {
            let ready = ready();
            finished(root, &asked, around, ready)
        }
    }
}

/// Run the command line `arguments` against the checkout `root`.
pub fn run(root: &Path, arguments: &[String]) -> ExitCode {
    let flag = Arc::new(AtomicBool::new(false));
    let mut signals = None;
    let mut ready = || -> Result<()> {
        signals = Some(Signals::cancelling(&flag)?);
        Ok(())
    };
    let cancelled = || flag.load(Ordering::SeqCst);
    let variable = |name: &str| std::env::var_os(name);
    let (search, home) = (variable("PATH"), variable("HOME").map(PathBuf::from));
    let mut engine = |name: Name| selected(name, search.as_deref(), home.clone(), &cancelled);
    let machine = rustix::system::uname();
    let machine = machine.machine().to_string_lossy().into_owned();
    let origin = Instant::now();
    let clock = move || origin.elapsed();
    let budget = || detect(None);
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let mut around = Around {
        variable: &variable,
        engine: &mut engine,
        machine: &machine,
        budget: &budget,
        clock: &clock,
        run_id: &run_identity,
        out: &mut out,
        err: &mut err,
    };
    // The signals, made first, are given back last.
    ExitCode::from(command(root, arguments, &mut around, &mut ready))
}
