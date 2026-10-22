//! Offline report commands. Input failures produce bounded coverage reports;
//! command errors never echo paths, record contents or operating-system details.

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

pub mod diagnostics;
pub mod lifecycle;
mod media;
mod media_correlation;
mod media_generator;
mod media_input;
mod media_schema;
mod receiver_stall;
mod receiver_stall_evidence;

/// Reuse the offline reader's complete fixed snapshot contract at the live
/// loopback sampler boundary. No unvalidated response is retained as evidence.
pub(crate) fn validate_media_snapshot(value: &serde_json::Value) -> anyhow::Result<()> {
    media_schema::snapshot(value)
}

/// Independent fixed-protocol oracle for the selected generator's offline
/// command. Benchmark callers invoke that executable too, then require semantic
/// equality before any child-controlled output crosses the artifact boundary.
pub(crate) fn benchmark_report(
    mode: &str,
    path: &Path,
    input: &serde_json::Value,
    admitted: bool,
    expected: u32,
) -> anyhow::Result<serde_json::Value> {
    match mode {
        "diagnostics" => Ok(serde_json::to_value(
            diagnostics::read(path, diagnostics::MAX_RECORDS).map_err(anyhow::Error::msg)?,
        )?),
        "lifecycle" => Ok(serde_json::to_value(lifecycle::read(
            path,
            &serde_json::to_string(input)?,
            Some(f64::from(expected)),
        ))?),
        "media" | "receiver-stall" => {
            let mut samples = input.clone();
            let mut results = if admitted {
                media_input::results(path).unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            };
            media::normalize_numbers(&mut samples);
            media::normalize_numbers(&mut results);
            Ok(if mode == "media" {
                media::analyze(&samples, &results).report
            } else {
                receiver_stall::correlate(&samples, &results)
            })
        }
        _ => anyhow::bail!("Unknown benchmark report command"),
    }
}

const USAGE: &str = "Usage: load_test report diagnostics PATH [--max-records 1..100000]";

/// Print one sanitized report. Exit 0 means complete coverage, 1 means incomplete
/// coverage or an output failure, and 2 means invalid command arguments. The JSON
/// report remains available on stdout when incomplete coverage causes exit 1.
pub fn run(args: &[OsString]) -> ExitCode {
    if args.first().is_some_and(|arg| arg == "lifecycle") {
        return run_lifecycle(&args[1..]);
    }
    if args
        .first()
        .is_some_and(|arg| arg == "media" || arg == "receiver-stall")
    {
        return run_media(args);
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
    let input = read_bounded_stdin(lifecycle::TIMELINE_BYTES).unwrap_or_else(|| "null".to_owned());
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

/// The owned parent closes stdin after one JSON document. Refuse a terminal, bound
/// bytes and time, and temporarily use nonblocking reads so an unclosed FIFO or
/// inherited pipe cannot keep the offline command alive indefinitely.
fn read_bounded_stdin(maximum: usize) -> Option<String> {
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
                    if bytes.len() + count > maximum {
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

const MEDIA_USAGE: &str = "Usage: load_test report media|receiver-stall RESULTS_PATH [--results-unavailable] < SAMPLES.json";

fn run_media(args: &[OsString]) -> ExitCode {
    if args.len() == 2 && (args[1] == "--help" || args[1] == "-h") {
        println!("{MEDIA_USAGE}");
        return ExitCode::SUCCESS;
    }
    if !(args.len() == 2 || args.len() == 3 && args[2] == "--results-unavailable") {
        eprintln!("{MEDIA_USAGE}");
        return ExitCode::from(2);
    }
    let mut samples = read_bounded_stdin(media_input::SAMPLE_BYTES)
        .and_then(|input| serde_json::from_str(&input).ok())
        .unwrap_or(serde_json::Value::Null);
    let mut results = if args.len() == 2 {
        media_input::results(Path::new(&args[1])).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    media::normalize_numbers(&mut samples);
    media::normalize_numbers(&mut results);
    let report = if args[0] == "media" {
        media::analyze(&samples, &results).report
    } else {
        receiver_stall::correlate(&samples, &results)
    };
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &report).is_err() || writeln!(stdout).is_err() {
        eprintln!("Media report output failed.");
        return ExitCode::FAILURE;
    }
    if report["coverage"]["complete"] == true {
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
