//! Host monitoring: fixed read-only samples, the alert recorder and the
//! private operational report. Importing this module starts nothing. Every
//! command addresses fixed local paths, the local Docker daemon and the
//! deployment's own database; none takes a host, statement or path argument.

mod alerts;
mod backups;
mod collect;
mod credential;
#[cfg(test)]
mod database_tests;
mod external;
mod logging;
mod prepare;
mod report;
mod text;
mod transfer;

use crate::host::command::Local;
use crate::host::{Host, Paths, System, failure, state};
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Write one line; a line that cannot be written is a failure of its own.
fn emit(mut stream: impl Write, line: &str) -> bool {
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .is_ok()
}

/// Whether this process may touch the prepared host's private state.
fn root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Give Prometheus, and nobody else beside root, the prepared metrics token.
fn scrape_credential() -> failure::Result<&'static str> {
    let environment = std::path::Path::new("/etc/simplestchat-public/app.env");
    let destination = std::path::Path::new("/etc/simplestchat-monitoring/metrics-token");
    let prometheus = state::Access {
        owner: 0,
        group: 10003,
        mode: 0o440,
    };
    credential::credential(environment, destination, &prometheus)
}

/// Run one `monitor` command on the prepared host. Each takes no further
/// argument; a syntax error returns 2 before any effect.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let [action] = arguments else {
        return ExitCode::from(2);
    };
    let refusal = match action.to_str() {
        Some("collect") => "Monitoring collection failed; inspect the collector's private state.\n",
        Some("credential") => {
            "Scrape credential preparation failed; inspect the prepared environment privately.\n"
        }
        Some("external") => "External check import failed; previous evidence remains private.\n",
        Some("prepare") => {
            "Operational schema preparation failed; inspect the database privately.\n"
        }
        Some("report") => "Operational report unavailable; inspect private monitoring health.\n",
        _ => return ExitCode::from(2),
    };
    let clock = System::new();
    let host = Host {
        commands: &Local,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    // Everything a command creates is private to root.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let outcome = if root() {
        match action.to_str() {
            Some("collect") => collect::collect(&host).map(|line| Some(line.to_owned())),
            Some("credential") => scrape_credential().map(|line| Some(line.to_owned())),
            Some("external") => external::import(&host).map(|line| line.map(str::to_owned)),
            Some("prepare") => prepare::prepare(&host).map(|line| Some(line.to_owned())),
            _ => report::report(&host).map(Some),
        }
    } else {
        Err(failure::Failure::Refused(
            "Requires root on the prepared host",
        ))
    };
    let printed = outcome.is_ok_and(|line| line.is_none_or(|line| emit(std::io::stdout(), &line)));
    if printed {
        return ExitCode::SUCCESS;
    }
    let _ = emit(std::io::stderr(), refusal);
    ExitCode::FAILURE
}

/// Run `database logging disable` on the prepared host under the shared
/// workload lock. The outcome is printed even when the remediation fails.
pub fn run_database(arguments: &[OsString]) -> ExitCode {
    let words: Vec<&str> = arguments.iter().filter_map(|word| word.to_str()).collect();
    if words != ["logging", "disable"] || words.len() != arguments.len() {
        return ExitCode::from(2);
    }
    let refused = |class: &str| {
        let line =
            format!("Database logging remediation failed ({class}); inspect private evidence.\n");
        let _ = emit(std::io::stderr(), &line);
        ExitCode::FAILURE
    };
    if !root() {
        return refused("ReleaseError");
    }
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        if signal_hook::flag::register(signal, Arc::clone(&stop)).is_err() {
            return refused("OSError");
        }
    }
    let workload = match crate::release::assets::Workload::acquire() {
        Ok(workload) => workload,
        Err(failure) if failure.busy() => return refused("BlockingIOError"),
        Err(_) => return refused("ReleaseError"),
    };
    let clock = System::new();
    let commands = logging::Interruptible {
        inner: &Local,
        stop: &stop,
    };
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let configuration = std::path::Path::new("/etc/simplestchat-public");
    let (line, journal) = logging::disable(&host, configuration);
    let released = workload.finish().is_ok();
    let printed = line.is_some_and(|line| emit(std::io::stdout(), &line));
    match journal {
        Some(journal) => {
            let _ = emit(std::io::stderr(), &journal);
            ExitCode::FAILURE
        }
        None if printed && released && !stop.load(Ordering::SeqCst) => ExitCode::SUCCESS,
        // The remediation passed, but its outcome was not delivered, the lock
        // did not close cleanly or a signal arrived: that is not a success.
        None => refused("ReleaseError"),
    }
}

#[cfg(test)]
mod recorded {
    use serde_json::Value;

    /// The answers the retired Python helpers gave for fixed inputs.
    pub(super) fn reference() -> Value {
        serde_json::from_str(include_str!("monitor/recorded_reference.json")).unwrap()
    }
}
