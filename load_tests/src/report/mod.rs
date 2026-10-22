//! Offline report commands. Input failures produce bounded coverage reports;
//! command errors never echo paths, record contents or operating-system details.

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

pub mod diagnostics;
pub mod lifecycle;

const USAGE: &str = "Usage: load_test report diagnostics PATH [--max-records 1..100000]";

/// Print one sanitized report. Exit 0 means complete coverage, 1 means incomplete
/// coverage or an output failure, and 2 means invalid command arguments. The JSON
/// report remains available on stdout when incomplete coverage causes exit 1.
pub fn run(args: &[OsString]) -> ExitCode {
    if args.first().is_some_and(|arg| arg == "lifecycle") {
        return run_lifecycle(&args[1..]);
    }
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

const LIFECYCLE_USAGE: &str =
    "Usage: load_test report lifecycle LOG_PATH --expected-participants N < TIMELINE.json";

fn run_lifecycle(args: &[OsString]) -> ExitCode {
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{LIFECYCLE_USAGE}");
        return ExitCode::SUCCESS;
    }
    let expected = args
        .get(2)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<f64>().ok());
    if args.len() != 3 || args[1] != "--expected-participants" || expected.is_none() {
        eprintln!("{LIFECYCLE_USAGE}");
        return ExitCode::from(2);
    }
    let input = read_timeline_stdin().unwrap_or_else(|| "null".to_owned());
    let report = lifecycle::read(Path::new(&args[0]), &input, expected);
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &report).is_err() || writeln!(stdout).is_err() {
        eprintln!("Lifecycle report output failed.");
        return ExitCode::FAILURE;
    }
    if report.coverage.complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The owned parent closes stdin after one timeline. Refuse a terminal, bound
/// bytes and time, and temporarily use nonblocking reads so an unclosed FIFO or
/// inherited pipe cannot keep the offline command alive indefinitely.
fn read_timeline_stdin() -> Option<String> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let original = OFlag::from_bits_truncate(fcntl(&stdin, FcntlArg::F_GETFL).ok()?);
    fcntl(&stdin, FcntlArg::F_SETFL(original | OFlag::O_NONBLOCK)).ok()?;
    let result = (|| {
        let mut input = stdin.lock();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            match input.read(&mut buffer) {
                Ok(0) => return String::from_utf8(bytes).ok(),
                Ok(count) => {
                    if bytes.len() + count > lifecycle::TIMELINE_BYTES {
                        return None;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(_) => return None,
            }
        }
    })();
    fcntl(&stdin, FcntlArg::F_SETFL(original)).ok()?;
    result
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
