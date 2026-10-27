//! Prefer the official archives in one admitted hosted Ubuntu mirror profile.

mod files;
mod policy;
#[cfg(test)]
mod tests;

use crate::tooling;
use std::process::ExitCode;

const USAGE: &str = "usage: xtask ci apt-mirror";

pub(super) fn run(arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let host = rustix::system::uname();
    let result = policy::host(
        &tooling::inherited_env(),
        host.sysname().to_str().unwrap_or(""),
        host.machine().to_str().unwrap_or(""),
        rustix::process::geteuid().as_raw(),
    )
    .and_then(|()| files::apply());
    match result {
        Ok(()) => {
            println!("Official Ubuntu archive mirror priorities admitted.");
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            eprintln!("Hosted Ubuntu mirror preparation refused.");
            Ok(ExitCode::from(error.code))
        }
    }
}
