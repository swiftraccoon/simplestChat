//! Offline report commands. Input failures produce bounded coverage reports;
//! command errors never echo paths, record contents or operating-system details.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

pub mod diagnostics;

const USAGE: &str = "Usage: load_test report diagnostics PATH [--max-records 1..100000]";

/// Print one sanitized report. Exit 0 means complete coverage, 1 means incomplete
/// coverage or an output failure, and 2 means invalid command arguments. The JSON
/// report remains available on stdout when incomplete coverage causes exit 1.
pub fn run(args: &[OsString]) -> ExitCode {
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some((path, max_records)) = arguments(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let report = diagnostics::read(path, max_records)
        .expect("command argument validation establishes the diagnostic record bound");
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &report).is_err() || writeln!(stdout).is_err() {
        eprintln!("Diagnostic report output failed.");
        return ExitCode::FAILURE;
    }
    if report.coverage.complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn arguments(args: &[OsString]) -> Option<(&Path, usize)> {
    if args.first()? != "diagnostics" || !matches!(args.len(), 2 | 4) {
        return None;
    }
    let max_records = if args.len() == 4 {
        if args[2] != "--max-records" {
            return None;
        }
        args[3].to_str()?.parse::<usize>().ok()?
    } else {
        diagnostics::MAX_RECORDS
    };
    if !(1..=diagnostics::MAX_RECORDS).contains(&max_records) {
        return None;
    }
    Some((Path::new(&args[1]), max_records))
}

#[cfg(test)]
mod tests;
