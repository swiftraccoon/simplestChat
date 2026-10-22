//! Explicit owned local comparisons. Importing policy starts no runtime, child
//! process, server, network request or workload.

pub mod artifacts;
pub mod clock;
pub mod commands;
pub mod comparison;
pub mod environment;
mod impaired;
pub mod local;
pub mod media_sampler;
pub mod metrics;
pub mod options;
pub mod process;
pub mod provenance;
pub mod reports;
pub mod resources;
pub mod sampling;
pub mod subscription;
pub mod summary;
pub mod verdict;

pub(super) mod podman;

#[cfg(test)]
mod media_sampler_tests;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod reports_tests;
#[cfg(test)]
mod subscription_tests;
#[cfg(test)]
mod summary_tests;

/// Explicit offline policy or owned local/container benchmark entrypoint.
/// Importing the library never starts a runtime or process.
pub fn run(arguments: &[std::ffi::OsString]) -> std::process::ExitCode {
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("local") => local::run(&arguments[1..]),
        Some("podman") => podman::run(&arguments[1..]),
        Some("impaired") => impaired::run(&arguments[1..]),
        Some("browser-mechanics") => impaired::run_browser(&arguments[1..]),
        Some("browser-adaptive") => impaired::run_browser_adaptive(&arguments[1..]),
        Some("browser-tcp") => impaired::run_browser_tcp(&arguments[1..]),
        Some("--help" | "-h") if arguments.len() == 1 => {
            println!(concat!(
                "Usage: load_test bench local|podman|impaired|",
                "browser-mechanics|browser-adaptive|browser-tcp [OPTIONS]"
            ));
            std::process::ExitCode::SUCCESS
        }
        _ => {
            eprintln!(concat!(
                "Usage: load_test bench local|podman|impaired|",
                "browser-mechanics|browser-adaptive|browser-tcp [OPTIONS]"
            ));
            std::process::ExitCode::from(2)
        }
    }
}
