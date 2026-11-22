//! Fixed browser discovery, then the disposable release fixture as root beside
//! the scanner download.

mod execute;
mod files;
mod fixture;
pub(super) mod invocation;
mod plan;
#[cfg(test)]
mod tests;

use crate::{browser_suite::process::Signals, process::Native};
use simplestchat_toolkit::tooling;
use std::{path::Path, process::ExitCode};

const USAGE: &str = concat!(
    "usage: cargo run --locked -q -p simplestchat-ci -- release-checks ",
    "--http-policy-binary ABSOLUTE_FEATURE_BINARY",
);

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if arguments.len() != 2 || arguments[0] != "--http-policy-binary" {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let system = rustix::system::uname();
    let result = (|| -> tooling::Result<()> {
        let context = plan::Context::new(
            root,
            tooling::inherited_env(),
            system.sysname().to_str().unwrap_or(""),
            Path::new(&arguments[1]),
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| files::failure("Release-check runtime unavailable."))?;
        runtime.block_on(async {
            let mut native = Native::new(&context.root, &context.environment, Signals::new()?);
            let mut scanner = Native::new(&context.root, &context.environment, Signals::new()?);
            execute::execute(&context, &mut native, &mut scanner).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Disposable release checks failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}

/// `release-fixture`: the root fixture `release-checks` starts through `sudo`.
pub(super) fn run_fixture(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    fixture::run(root, arguments)
}
