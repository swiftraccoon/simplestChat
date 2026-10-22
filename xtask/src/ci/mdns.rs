//! Complete opt-in macOS mDNS controller. The C probe and JS remain references.
mod config;
mod execute;
mod files;
#[cfg(test)]
mod oracle_tests;
mod policy;
#[cfg(test)]
mod tests;
pub(super) mod tools;

use super::process::Native;
use crate::{
    owned_process::Signals,
    tooling::{self, Failure},
};
use std::{path::Path, process::ExitCode, time::Instant};

pub(super) fn failure(code: u8) -> Failure {
    Failure::new(code, "Cannot complete the owned mDNS diagnostic.")
}
pub(super) trait Clock {
    fn elapsed(&self) -> f64;
    fn utc(&self) -> String;
}
struct NativeClock(Instant);
impl Clock for NativeClock {
    fn elapsed(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
    fn utc(&self) -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
}
const USAGE: &str =
    "usage: cargo run --locked -q -p xtask -- ci mdns preflight|snapshot [--artifact-only]";
pub(super) fn run(root: &Path, args: &[String]) -> anyhow::Result<ExitCode> {
    if args == ["--help"] || args == ["-h"] {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let environment = tooling::inherited_env();
    let caller = match std::env::current_dir() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("{}", failure(1).message);
            return Ok(ExitCode::FAILURE);
        }
    };
    let config = match config::Config::read(root, &caller, args, &environment, std::env::consts::OS)
    {
        Ok(value) => value,
        Err(_) => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };
    let clock = NativeClock(Instant::now());
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let signals = Signals::new()?;
            let mut native = Native::new(&config.caller, &config.environment, signals);
            execute::run(&config, &mut native, &clock, &tools::Paths::native()).await
        });
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("{}", failure(error.code).message);
            Ok(ExitCode::from(error.code))
        }
    }
}
