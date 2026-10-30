//! Complete authenticated three-member export for a trusted host installer.
//! The opaque acquisition stays owned through publication, replay and final
//! witnesses. Expected installed pins select custody, never signature authority.
//! An explicitly retained ZIP replaces transport bytes only; fresh acquisition
//! metadata, opaque verification and final witnesses remain mandatory.
//! No candidate, SSH command or installed host operation is executed here.

mod execute;
mod files;
mod lifecycle;
mod plan;
mod request;
#[cfg(test)]
mod tests;
mod worker;

use crate::tooling::{Failure, Result};

pub(super) const TRANSFER: u64 = 257 * 1024 * 1024;
fn failure(code: u8) -> Failure {
    Failure::new(code, "Authenticated Ops distribution export is unproven.")
}
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| failure(1))
}
fn keep(primary: &mut Option<Failure>, cause: Failure) {
    if primary.is_none() {
        *primary = Some(cause);
    }
}
const USAGE: &str = "usage: xtask ops-artifact export --request ABS --request-sha256 SHA256\n";

/// Capture the original entry before parsing, then retain one synchronous
/// registration across every runtime, umask, replay and final completion fence.
pub(super) fn run(arguments: &[String]) -> std::process::ExitCode {
    use std::{io::Write, process::ExitCode, time::Instant};
    let started = Instant::now();
    if arguments == ["--help"] {
        let mut output = std::io::stdout().lock();
        return if output
            .write_all(USAGE.as_bytes())
            .and_then(|()| output.flush())
            .is_ok()
        {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let [request_flag, path, sha_flag, selected] = arguments else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    if request_flag != "--request"
        || sha_flag != "--request-sha256"
        || !request::path(std::path::Path::new(path))
        || !super::digest(selected)
    {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    match lifecycle::run(started, std::path::Path::new(path), selected) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.message);
            ExitCode::from(error.code)
        }
    }
}
