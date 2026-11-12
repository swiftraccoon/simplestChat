//! What a migration runs on its two hosts: the read-only inspection before
//! anything changes, the steps that move its data, the handoff of a kept
//! hostname's certificate, and the bridge that forwards cached traffic from
//! a sealed source.

#[cfg(test)]
mod client;
mod data;
mod deployment;
mod handoff;
mod inspect;
mod request;
mod snapshot;
mod snapshot_sql;
mod source;
mod target;
mod tls;
mod value;

use std::ffi::OsString;
use std::process::ExitCode;

/// Run `migrate inspect …`, one of the data steps, `migrate tls …` or
/// `migrate handoff …` on the selected migration host. Any other command
/// line is a syntax error, returned as 2 before any effect.
pub fn run(arguments: &[OsString]) -> ExitCode {
    match arguments.first().and_then(|family| family.to_str()) {
        Some("inspect") => inspect::run(&arguments[1..]),
        Some(step) if data::Action::named(step).is_some() => data::run(arguments),
        Some("tls") => tls::run(arguments),
        Some("handoff") => handoff::run(arguments),
        _ => ExitCode::from(2),
    }
}
