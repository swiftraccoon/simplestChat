//! Complete guest-only shutdown and stopped-recorder smokes. Importing is inert.

mod config;
mod controller;
mod files;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod fixtures;
mod native;
#[cfg(test)]
mod oracle_tests;
mod report;
#[cfg(test)]
mod tests;
mod wire;
#[cfg(test)]
mod wire_tests;

use config::Mode;
use std::{ffi::OsString, process::ExitCode};

pub(super) fn run(diagnostics: bool, arguments: &[OsString]) -> ExitCode {
    let mode = if diagnostics {
        Mode::Diagnostics
    } else {
        Mode::Shutdown
    };
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
        println!(
            "load_test bench {} --binary ABSOLUTE_SERVER [--output FRESH_ABSOLUTE_DIR]",
            mode.name()
        );
        return ExitCode::SUCCESS;
    }
    // Runtime results are bounded private artifacts. No potentially blocking
    // public projection can outlive the single signal/deadline owner.
    ExitCode::from(native::run(mode, arguments))
}
