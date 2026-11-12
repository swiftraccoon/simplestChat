//! Prefer the official archives in the admitted hosted Ubuntu mirror
//! profiles: GitHub's Azure-hosted and AWS-hosted Ubuntu 24.04 images.

mod files;
mod policy;
#[cfg(test)]
mod tests;

use simplestchat_toolkit::tooling;
use std::process::ExitCode;

const USAGE: &str = "usage: simplestchat-ci apt-mirror";

/// The one line a refusal leaves: that it refused, and the reason, which is
/// one of this command's own fixed sentences. It names the check that
/// refused and nothing of the host.
fn refusal(error: &tooling::Failure) -> String {
    format!(
        "Hosted Ubuntu mirror preparation refused. {}",
        error.message
    )
}

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
            eprintln!("{}", refusal(&error));
            Ok(ExitCode::from(error.code))
        }
    }
}
