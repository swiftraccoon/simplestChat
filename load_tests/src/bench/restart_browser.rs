//! One Rust owner for two guest servers and a controlled Node browser journey.
//! Importing starts no process, listener or browser.

mod config;
mod controller;
mod files;
mod native;
mod protocol;
mod report;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;

use std::{ffi::OsString, process::ExitCode};

pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
        println!(concat!(
            "load_test bench restart-browser-smoke --binary ABS --browser ",
            "chromium|firefox|webkit --output FRESH_ABS [--node ABS]"
        ));
        return ExitCode::SUCCESS;
    }
    ExitCode::from(native::run(arguments))
}
