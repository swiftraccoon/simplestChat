//! What a migration runs on its two hosts: the read-only inspection before
//! anything changes, and the handoff of a kept hostname's certificate.

mod inspect;
mod tls;

use std::ffi::OsString;
use std::process::ExitCode;

/// Run `migrate inspect …` or `migrate tls …` on the selected migration host.
/// Any other command line is a syntax error, returned as 2 before any effect.
pub fn run(arguments: &[OsString]) -> ExitCode {
    match arguments.first().and_then(|family| family.to_str()) {
        Some("inspect") => inspect::run(&arguments[1..]),
        Some("tls") => tls::run(arguments),
        _ => ExitCode::from(2),
    }
}
