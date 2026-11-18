//! `simplestchat-security codeql
//! jobs|coverage|tools|cargo-key|triage|local|upload-ready`: the CodeQL
//! tier's commands, each answering as its reference did.

use super::{cargo, coverage, tools};
use crate::{
    arguments::{Read, read},
    budget::{Budget, detect},
    paths,
};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::Path,
    process::ExitCode,
};

/// How the commands are used.
pub const USAGE: &str = concat!(
    "usage: simplestchat-security codeql jobs\n",
    "       simplestchat-security codeql coverage FILE\n",
    "       simplestchat-security codeql tools --output DIR [--codeql PATH]",
    " [--language LANGUAGE]\n",
    "       simplestchat-security codeql cargo-key --source-root DIR",
    " [--publication-check DIR]\n",
    "       simplestchat-security codeql triage health|sarif|plan|check|apply",
    " --output DIR --revision SHA ...\n",
    "       simplestchat-security codeql local --revision SHA --output DIR ...\n",
    "       simplestchat-security codeql upload-ready --output DIR --revision SHA",
    " --language LANGUAGE [--category CATEGORY ...]",
);
/// The options of `tools`.
const TOOLS: [&str; 3] = ["--codeql", "--language", "--output"];
/// The options of `cargo-key`.
const CARGO: [&str; 2] = ["--source-root", "--publication-check"];
/// The variable that asks for fewer compilers.
const JOBS: &str = "CODEQL_BUILD_JOBS";

/// What a command line comes to, as the reference's parser read one.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// The options given, each with the value it was given last.
    Asked(Vec<(&'static str, String)>),
    /// The usage, asked for: the reference's parser then ended with 0.
    Help,
    /// No command line of the command's: the reference's parser then ended
    /// with 2.
    Refused,
}

/// The options of `arguments` for a command that takes `options`, as the
/// reference's parser read them: an option given more than once counts as
/// it was given last, an option with no value of its own takes the next
/// argument when that is a word, a value must be one that `valid` admits,
/// and every option `required` names must be given. The command takes no
/// word of its own.
pub fn parsed(
    options: &[&'static str],
    required: &str,
    arguments: &[String],
    valid: &dyn Fn(&str, &str) -> bool,
) -> Line {
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
    let mut asked: Vec<(&'static str, String)> = Vec::new();
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
        if !valid(option, value) {
            return Line::Refused;
        }
        asked.retain(|(held, _)| *held != option);
        asked.push((option, value.to_owned()));
    }
    let given = asked.iter().any(|(option, _)| *option == required);
    if given && !stray {
        Line::Asked(asked)
    } else {
        Line::Refused
    }
}

/// The value `asked` gives `option`.
fn value<'a>(asked: &'a [(&'static str, String)], option: &str) -> Option<&'a str> {
    let found = asked.iter().find(|(held, _)| *held == option);
    found.map(|(_, value)| value.as_str())
}

/// The path `asked` gives `option`, spelled as the reference spelled one.
fn path(asked: &[(&'static str, String)], option: &str) -> Option<std::path::PathBuf> {
    value(asked, option).map(|given| paths::path(OsStr::new(given)))
}

/// What the commands look up and act through on this host.
pub struct Around<'a> {
    /// The caller's environment variable `name`, where it is set.
    pub variable: &'a dyn Fn(&str) -> Option<OsString>,
    /// The caller's search path.
    pub search: Option<&'a OsStr>,
    /// The kernel's names for this system and machine.
    pub system: &'a str,
    /// The kernel's name for this machine.
    pub machine: &'a str,
    /// The pin's name for this host's analyzer bundle.
    pub bundle: &'a str,
    /// What a fresh build may use, with the compilers asked for.
    pub budget: &'a dyn Fn(Option<&str>) -> crate::Result<Budget>,
    /// Keep what the command writes from other accounts, as the reference's
    /// analyzer setup did once its command line was read.
    pub private: &'a dyn Fn(),
    /// Where an answer goes.
    pub out: &'a mut dyn Write,
    /// Where a failure is told.
    pub err: &'a mut dyn Write,
}

/// The exit status of `codeql jobs`: the compilers a fresh build may run,
/// fewer when the caller's variable asks for fewer, or why none may run.
/// Whatever else the command line says is not read.
fn jobs(around: &mut Around<'_>) -> u8 {
    let asked = (around.variable)(JOBS).map(|given| given.into_string().unwrap_or_default());
    match (around.budget)(asked.as_deref()) {
        Ok(budget) => u8::from(writeln!(around.out, "{}", budget.workers).is_err()),
        Err(fault) => {
            let _ = writeln!(around.err, "CodeQL resource budget failed: {fault}");
            1
        }
    }
}

/// The exit status of the command line `arguments` against the checkout
/// `root`.
pub fn command(root: &Path, arguments: &[String], around: &mut Around<'_>) -> u8 {
    let Some((group, rest)) = arguments.split_first() else {
        let _ = writeln!(around.err, "{USAGE}");
        return 2;
    };
    let line = match group.as_str() {
        "jobs" => return jobs(around),
        "coverage" => return coverage::command(rest, around.out, around.err),
        "tools" => {
            let language = |option: &str, value: &str| {
                option != "--language" || tools::LANGUAGES.contains(&value)
            };
            parsed(&TOOLS, "--output", rest, &language)
        }
        "cargo-key" => parsed(&CARGO, "--source-root", rest, &|_, _| true),
        _ => Line::Refused,
    };
    let asked = match line {
        Line::Asked(asked) => asked,
        Line::Help => {
            let _ = writeln!(around.out, "{USAGE}");
            return 0;
        }
        Line::Refused => {
            let _ = writeln!(around.err, "{USAGE}");
            return 2;
        }
    };
    if group == "tools" {
        (around.private)();
        let request = tools::Asked {
            codeql: path(&asked, "--codeql"),
            language: value(&asked, "--language").map(str::to_owned),
            output: path(&asked, "--output").unwrap_or_default(),
        };
        let host = tools::Host {
            bundle: around.bundle,
            search: around.search,
        };
        return tools::command(root, &request, &host, around.variable, around.out);
    }
    let request = cargo::Asked {
        source_root: path(&asked, "--source-root").unwrap_or_default(),
        publication_check: path(&asked, "--publication-check"),
    };
    let host = cargo::Host {
        variable: around.variable,
        system: around.system,
        machine: around.machine,
        search: around.search,
    };
    cargo::command(root, &request, &host, around.out, around.err)
}

/// Run the command line `arguments` against the checkout `root`; `triage`
/// reaches GitHub, its clock and the caller's environment on its own, and
/// `local` and `upload-ready` this program and this host.
pub fn run(root: &Path, arguments: &[String]) -> ExitCode {
    if let Some((group, rest)) = arguments.split_first() {
        match group.as_str() {
            "triage" => return super::triage::run(root, rest),
            "local" => return super::local::run(root, rest),
            "upload-ready" => return super::local::command::upload_run(root, rest),
            _ => {}
        }
    }
    let variable = |name: &str| std::env::var_os(name);
    let search = variable("PATH");
    let name = rustix::system::uname();
    let system = name.sysname().to_string_lossy().into_owned();
    let machine = name.machine().to_string_lossy().into_owned();
    let bundle = tools::host_bundle();
    let private = || {
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    };
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let mut around = Around {
        variable: &variable,
        search: search.as_deref(),
        system: &system,
        machine: &machine,
        bundle: &bundle,
        budget: &detect,
        private: &private,
        out: &mut out,
        err: &mut err,
    };
    ExitCode::from(command(root, arguments, &mut around))
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
