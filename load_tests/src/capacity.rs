//! Capacity command dispatch; suggest/compare remain offline and run is explicit.

#[cfg(feature = "full")]
use anyhow::{Result, ensure};
use std::ffi::OsString;
#[cfg(feature = "full")]
use std::io::Write;
#[cfg(feature = "full")]
use std::path::PathBuf;
use std::process::ExitCode;

pub(crate) mod format;
pub(crate) mod json;
pub(crate) mod model;
#[cfg(feature = "full")]
mod presentation;
#[cfg(all(test, feature = "full"))]
mod tests;
pub mod vps;

#[cfg(feature = "full")]
const USAGE: &str = "Usage: load_test capacity suggest --vcpus N --memory-gib GIB \
[--port-mbps MBPS] [--calibration PATH] [--included-egress-tb TB] \
[--app-cpus CPUS] [--app-memory-mib MIB] [--env]\n\
       load_test capacity compare REPORT.json [REPORT.json ...]\n\
       load_test capacity run --server-image IMAGE --generator-image IMAGE [OPTIONS]";

#[cfg(feature = "full")]
#[derive(Default)]
struct Options {
    vcpus: Option<i64>,
    memory_gib: Option<f64>,
    port_mbps: f64,
    calibration: Option<PathBuf>,
    included_egress_tb: f64,
    app_cpus: Option<f64>,
    app_memory_mib: Option<i64>,
    env: bool,
}

#[cfg(feature = "full")]
enum Command {
    Suggest(Options),
    Compare(Vec<PathBuf>),
}

#[cfg(feature = "full")]
fn arguments(arguments: &[OsString]) -> Option<Command> {
    let (command, arguments) = arguments.split_first()?;
    if command == "compare" {
        if arguments.is_empty()
            || arguments
                .iter()
                .any(|argument| argument.to_string_lossy().starts_with('-'))
        {
            return None;
        }
        return Some(Command::Compare(
            arguments.iter().map(PathBuf::from).collect(),
        ));
    }
    if command != "suggest" {
        return None;
    }
    let mut result = Options::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str()? {
            "--env" => result.env = true,
            "--vcpus" => result.vcpus = Some(arguments.next()?.to_str()?.parse().ok()?),
            "--memory-gib" => result.memory_gib = Some(arguments.next()?.to_str()?.parse().ok()?),
            "--port-mbps" => result.port_mbps = arguments.next()?.to_str()?.parse().ok()?,
            "--calibration" => result.calibration = Some(PathBuf::from(arguments.next()?)),
            "--included-egress-tb" => {
                result.included_egress_tb = arguments.next()?.to_str()?.parse().ok()?
            }
            "--app-cpus" => result.app_cpus = Some(arguments.next()?.to_str()?.parse().ok()?),
            "--app-memory-mib" => {
                result.app_memory_mib = Some(arguments.next()?.to_str()?.parse().ok()?)
            }
            _ => return None,
        }
    }
    result.vcpus?;
    result.memory_gib?;
    Some(Command::Suggest(result))
}

#[cfg(feature = "full")]
fn suggest(options: Options) -> Result<String> {
    let vcpus = options.vcpus.unwrap();
    let memory_gib = options.memory_gib.unwrap();
    ensure!(
        vcpus >= 1 && memory_gib > 0.0,
        "a host has at least one vCPU and some memory"
    );
    ensure!(
        memory_gib.is_finite() && memory_gib * 1024.0 < -(i64::MIN as f64),
        "host memory is outside the integer range"
    );
    let port_mbps = if options.port_mbps == 0.0 {
        1000.0
    } else {
        options.port_mbps
    };
    let (ceilings, source) = if let Some(path) = options
        .calibration
        .filter(|path| !path.as_os_str().is_empty())
    {
        let report = json::read(&path)?;
        let profile = model::Profile::read(&report)?;
        ensure!(
            profile == model::Profile::default(),
            concat!(
                "suggest supports only the default five-person audio/video workload with ",
                "one speaker and no chat; this report measured {}. Use its measured summary ",
                "and projection for that workload."
            ),
            profile.describe()
        );
        (
            model::ceilings(&report)?,
            format!("ceilings from {}", path.display()),
        )
    } else {
        (
            model::reference(),
            "reference ceilings measured on an EPYC-class VPS on 2026-09-26; `run` measures yours"
                .into(),
        )
    };
    let host = model::Host {
        vcpus,
        memory_mib: (memory_gib * 1024.0) as i64,
        port_mbps,
    };
    let suggestion = model::suggest(host, &ceilings, options.app_cpus, options.app_memory_mib)?;
    let lines = if options.env {
        suggestion
            .settings()
            .into_iter()
            .filter(|setting| setting.starts_with(|character: char| character.is_ascii_uppercase()))
            .collect()
    } else {
        presentation::suggestion(
            &suggestion,
            host,
            &ceilings,
            &source,
            options.included_egress_tb,
        )
    };
    Ok(format!("{}\n", lines.join("\n")))
}

#[cfg(feature = "full")]
fn execute(command: Command) -> Result<String> {
    match command {
        Command::Suggest(options) => suggest(options),
        Command::Compare(paths) => presentation::compare(
            &paths
                .iter()
                .map(|path| json::read(path))
                .collect::<Result<Vec<_>>>()?,
        ),
    }
}

/// Dispatch capacity commands; only an explicit `run` starts measurement.
pub fn run(arguments: &[OsString]) -> ExitCode {
    run_with_program(arguments, "load_test")
}

fn usage(program: &str) -> String {
    #[cfg(feature = "full")]
    {
        USAGE.replace("load_test", program)
    }
    #[cfg(not(feature = "full"))]
    {
        crate::capacity_run::usage(program)
    }
}

/// Dispatch the same capacity command with a bounded caller presentation name.
/// The name selects help text only; it never selects a program or workload root.
pub fn run_with_program(arguments_: &[OsString], program: &str) -> ExitCode {
    if program.is_empty()
        || program.len() > 64
        || !program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return ExitCode::from(2);
    }
    if arguments_.first().is_some_and(|value| value == "run") {
        return crate::capacity_run::run(&arguments_[1..], program);
    }
    let usage = usage(program);
    if arguments_
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{usage}");
        return ExitCode::SUCCESS;
    }
    #[cfg(feature = "full")]
    {
        let Some(command) = arguments(arguments_) else {
            eprintln!("{usage}");
            return ExitCode::from(2);
        };
        match execute(command)
            .and_then(|output| Ok(std::io::stdout().lock().write_all(output.as_bytes())?))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("capacity: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(feature = "full"))]
    {
        eprintln!("{usage}");
        ExitCode::from(2)
    }
}

#[cfg(all(test, not(feature = "full")))]
mod lite_tests {
    use super::*;

    #[test]
    fn lightweight_help_and_dispatch_offer_only_the_available_measurement() {
        let help = usage("simplestchat-ops");
        assert!(help.starts_with("Usage: simplestchat-ops capacity run "));
        assert!(!help.contains("capacity suggest"));
        assert!(!help.contains("capacity compare"));
        assert_eq!(
            run_with_program(&["--help".into()], "simplestchat-ops"),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run_with_program(
                &[
                    "suggest".into(),
                    "--vcpus".into(),
                    "2".into(),
                    "--memory-gib".into(),
                    "2".into()
                ],
                "simplestchat-ops"
            ),
            ExitCode::from(2),
        );
    }
}
